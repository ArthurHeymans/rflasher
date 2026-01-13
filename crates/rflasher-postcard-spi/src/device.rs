//! PostcardSpi device implementation
//!
//! This module provides the main `PostcardSpi` struct that implements
//! the `SpiMaster` trait using postcard-rpc over USB.

use crate::error::{Error, Result};
use postcard_rpc::{header::VarSeqKind, host_client::HostClient};
use postcard_spi_icd::*;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{AddressWidth as CoreAddressWidth, IoMode as CoreIoMode, SpiCommand};
use std::sync::OnceLock;

/// Global tokio runtime for blocking operations
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Get or create the global tokio runtime
fn get_runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime")
    })
}

/// Run an async future in a blocking context
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    // Try to get current runtime handle first
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        // We're inside a tokio runtime, use block_in_place
        tokio::task::block_in_place(|| handle.block_on(fut))
    } else {
        // No runtime, use the global one
        get_runtime().block_on(fut)
    }
}

/// PostcardSpi programmer
///
/// This struct represents a connection to a postcard-spi device (e.g., a Pico)
/// and implements the `SpiMaster` trait for communicating with SPI flash chips.
pub struct PostcardSpi {
    /// The postcard-rpc client
    client: HostClient<SpiWireError>,
    /// Cached device information
    info: DeviceInfo,
}

impl PostcardSpi {
    /// Open a connection to a postcard-spi device by USB serial number
    ///
    /// # Arguments
    /// * `serial` - USB serial number to match
    ///
    /// # Example
    /// ```ignore
    /// let programmer = PostcardSpi::open_by_serial("12345678")?;
    /// ```
    pub fn open_by_serial(serial: &str) -> Result<Self> {
        let serial_owned = serial.to_string();
        let client = HostClient::try_new_raw_nusb(
            move |d| d.serial_number() == Some(&serial_owned),
            "postcard_spi/error",
            8,
            VarSeqKind::Seq1,
        )
        .map_err(Error::ConnectionFailed)?;

        Self::from_client(client)
    }

    /// Open a connection to a postcard-spi device by VID/PID
    ///
    /// Connects to the first device matching the given VID and PID.
    ///
    /// # Arguments
    /// * `vid` - USB Vendor ID
    /// * `pid` - USB Product ID
    pub fn open_by_vid_pid(vid: u16, pid: u16) -> Result<Self> {
        let client = HostClient::try_new_raw_nusb(
            move |d| d.vendor_id() == vid && d.product_id() == pid,
            "postcard_spi/error",
            8,
            VarSeqKind::Seq1,
        )
        .map_err(Error::ConnectionFailed)?;

        Self::from_client(client)
    }

    /// Open the first available postcard-spi device
    ///
    /// Uses the default VID/PID from the ICD.
    pub fn open() -> Result<Self> {
        Self::open_by_vid_pid(USB_VID, USB_PID)
    }

    /// Create from an existing HostClient and initialize
    fn from_client(client: HostClient<SpiWireError>) -> Result<Self> {
        let mut programmer = Self {
            client,
            info: DeviceInfo {
                name: [0; 16],
                version: 0,
                max_transfer_size: 0,
                num_cs: 0,
                current_cs: 0,
                supported_modes: IoModeFlags::new(0),
                current_speed_hz: 0,
            },
        };

        // Query device info
        programmer.refresh_info()?;

        log::info!(
            "Connected to postcard-spi device: {} (version {}, {} CS lines, max {} bytes)",
            programmer.info.name_str(),
            programmer.info.version,
            programmer.info.num_cs,
            programmer.info.max_transfer_size
        );

        Ok(programmer)
    }

    /// Refresh the cached device information
    pub fn refresh_info(&mut self) -> Result<()> {
        let info = block_on(async {
            self.client
                .send_resp::<GetInfoEndpoint>(&())
                .await
                .map_err(|e| Error::RpcError(format!("{:?}", e)))
        })?;

        self.info = info;
        Ok(())
    }

    /// Get the cached device information
    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// Set the SPI clock frequency
    ///
    /// Returns the actual frequency that was set (may differ from requested).
    pub fn set_speed(&mut self, hz: u32) -> Result<u32> {
        let resp = block_on(async {
            self.client
                .send_resp::<SetSpeedEndpoint>(&SetSpeedReq { hz })
                .await
                .map_err(|e| Error::RpcError(format!("{:?}", e)))
        })?;

        self.info.current_speed_hz = resp.actual_hz;
        log::info!(
            "SPI speed: requested {} Hz, set to {} Hz",
            hz,
            resp.actual_hz
        );

        Ok(resp.actual_hz)
    }

    /// Select a chip select line
    pub fn set_cs(&mut self, cs: u8) -> Result<()> {
        if cs >= self.info.num_cs {
            return Err(Error::InvalidCs {
                cs,
                num_cs: self.info.num_cs,
            });
        }

        // Use batch operation to set CS
        let mut batch = BatchBuilder::new();
        batch.set_cs(cs);
        self.execute_batch(batch)?;

        self.info.current_cs = cs;
        log::debug!("Selected CS{}", cs);

        Ok(())
    }

    /// Execute a batch of SPI transactions
    ///
    /// This is the most efficient way to perform multiple SPI operations,
    /// as it sends all operations in a single USB transfer and receives
    /// all results in a single response.
    ///
    /// Each transaction in the batch is a complete SPI command with
    /// automatic CS handling, making this compatible with hardware
    /// controllers that don't support arbitrary CS manipulation.
    ///
    /// # Arguments
    /// * `batch` - A `BatchBuilder` containing the operations to execute
    ///
    /// # Returns
    /// A `BatchResult` containing the results of all operations
    ///
    /// # Example
    /// ```ignore
    /// // Read JEDEC ID with a batch operation
    /// let mut batch = BatchBuilder::new();
    /// batch.cmd_read(0x9F, 3);  // RDID: read 3 bytes
    ///
    /// let result = programmer.execute_batch(batch)?;
    /// let jedec_id = result.get_read(0).unwrap();
    ///
    /// // Page program with status polling
    /// let mut batch = BatchBuilder::new();
    /// batch.cmd(0x06)                           // WREN
    ///      .cmd_write_addr3(0x02, addr, &data)  // Page Program
    ///      .poll(0x05, 0x01, 0x00, 5000);       // Wait for WIP=0
    ///
    /// programmer.execute_batch(batch)?;
    /// ```
    pub fn execute_batch(&mut self, batch: BatchBuilder) -> Result<BatchResult> {
        let request = batch.build();

        let resp: BatchResponse = block_on(async {
            self.client
                .send_resp::<BatchEndpoint>(&request)
                .await
                .map_err(|e| Error::RpcError(format!("{:?}", e)))
        })?;

        Ok(BatchResult { inner: resp })
    }

    /// Execute a batch and wait for a status register condition
    ///
    /// This is a convenience method for common flash operations that
    /// need to poll a status register until an operation completes.
    ///
    /// # Arguments
    /// * `cmd` - Command byte to send (usually read status register)
    /// * `mask` - Mask to apply to the status byte
    /// * `expected` - Expected value after masking (e.g., 0 for WIP bit clear)
    /// * `timeout_ms` - Maximum time to wait in milliseconds
    ///
    /// # Returns
    /// The final status byte, or an error if timeout occurred
    pub fn poll_status(&mut self, cmd: u8, mask: u8, expected: u8, timeout_ms: u16) -> Result<u8> {
        let mut batch = BatchBuilder::new();
        batch.poll(cmd, mask, expected, timeout_ms);

        let result = self.execute_batch(batch)?;

        match result.inner.results.first() {
            Some(BatchOpResult::PollOk(status)) => Ok(*status),
            Some(BatchOpResult::PollTimeout(status)) => Err(Error::RpcError(format!(
                "Poll timeout, last status: 0x{:02x}",
                status
            ))),
            _ => Err(Error::RpcError("Unexpected batch result".into())),
        }
    }
}

/// Builder for batch operations
///
/// Use this to construct a sequence of SPI transactions that will be
/// executed efficiently in a single USB transfer. Each transaction is
/// a complete SPI command with automatic CS handling.
///
/// # Example
///
/// ```ignore
/// use rflasher_postcard_spi::{BatchBuilder, AddressWidth};
///
/// // Read JEDEC ID
/// let mut batch = BatchBuilder::new();
/// batch.cmd_read(0x9F, 3);  // RDID: opcode 0x9F, read 3 bytes
///
/// let result = programmer.execute_batch(batch)?;
/// let jedec_id = result.get_read(0).unwrap();
///
/// // Page program with status polling (3-byte address mode)
/// let mut batch = BatchBuilder::new();
/// batch.cmd(0x06)                                              // WREN
///      .cmd_write_addr(0x02, addr, AddressWidth::ThreeByte, &data)  // Page Program
///      .poll(0x05, 0x01, 0x00, 5000);                          // Wait for WIP=0
///
/// programmer.execute_batch(batch)?;
/// ```
#[derive(Debug, Clone, Default)]
pub struct BatchBuilder {
    ops: heapless::Vec<BatchOp, MAX_BATCH_OPS>,
}

impl BatchBuilder {
    /// Create a new batch builder
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a simple command (opcode only, no address or data)
    ///
    /// Examples: WREN (0x06), WRDI (0x04), chip erase (0xC7)
    pub fn cmd(&mut self, opcode: u8) -> &mut Self {
        let _ = self
            .ops
            .push(BatchOp::Transact(SpiTransaction::cmd(opcode)));
        self
    }

    /// Add a read command (opcode + read data, no address)
    ///
    /// Examples: RDID (0x9F), RDSR (0x05)
    pub fn cmd_read(&mut self, opcode: u8, len: u8) -> &mut Self {
        let _ = self
            .ops
            .push(BatchOp::Transact(SpiTransaction::read(opcode, len)));
        self
    }

    /// Add a write command (opcode + write data, no address)
    ///
    /// Examples: WRSR (0x01)
    pub fn cmd_write(&mut self, opcode: u8, data: &[u8]) -> &mut Self {
        let _ = self
            .ops
            .push(BatchOp::Transact(SpiTransaction::write(opcode, data)));
        self
    }

    /// Add a command with address but no data (e.g., sector erase)
    ///
    /// Examples: Sector Erase (0x20), Block Erase (0xD8)
    pub fn cmd_addr(&mut self, opcode: u8, addr: u32, width: AddressWidth) -> &mut Self {
        let tx = SpiTransaction::cmd(opcode).with_addr(addr, width);
        let _ = self.ops.push(BatchOp::Transact(tx));
        self
    }

    /// Add a read command with address
    ///
    /// Examples: READ (0x03), FAST_READ (0x0B - use `transaction()` for dummy cycles)
    pub fn cmd_read_addr(
        &mut self,
        opcode: u8,
        addr: u32,
        width: AddressWidth,
        len: u8,
    ) -> &mut Self {
        let tx = SpiTransaction::read(opcode, len).with_addr(addr, width);
        let _ = self.ops.push(BatchOp::Transact(tx));
        self
    }

    /// Add a write command with address
    ///
    /// Examples: Page Program (0x02)
    pub fn cmd_write_addr(
        &mut self,
        opcode: u8,
        addr: u32,
        width: AddressWidth,
        data: &[u8],
    ) -> &mut Self {
        let tx = SpiTransaction::write(opcode, data).with_addr(addr, width);
        let _ = self.ops.push(BatchOp::Transact(tx));
        self
    }

    /// Add a custom transaction with full control
    ///
    /// Use this for commands that need dummy cycles, specific I/O modes, etc.
    pub fn transaction(&mut self, tx: SpiTransaction) -> &mut Self {
        let _ = self.ops.push(BatchOp::Transact(tx));
        self
    }

    /// Add a delay between transactions (microseconds)
    pub fn delay_us(&mut self, us: u32) -> &mut Self {
        let _ = self.ops.push(BatchOp::DelayUs(us));
        self
    }

    /// Poll status register until condition is met
    ///
    /// Executes complete transactions (with CS) repeatedly:
    /// send `cmd`, read 1 byte, check if `(value & mask) == expected`.
    /// Times out after `timeout_ms` milliseconds.
    ///
    /// Common usage: `poll(0x05, 0x01, 0x00, 5000)` waits for WIP bit to clear
    pub fn poll(&mut self, cmd: u8, mask: u8, expected: u8, timeout_ms: u16) -> &mut Self {
        let _ = self.ops.push(BatchOp::Poll {
            cmd,
            mask,
            expected,
            timeout_ms,
        });
        self
    }

    /// Switch to a different chip select for subsequent operations
    pub fn set_cs(&mut self, cs: u8) -> &mut Self {
        let _ = self.ops.push(BatchOp::SetCs(cs));
        self
    }

    /// Build the batch request
    pub fn build(self) -> BatchRequest {
        BatchRequest { ops: self.ops }
    }

    /// Get the number of operations in the batch
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Check if the batch is empty
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Result of a batch operation
#[derive(Debug)]
pub struct BatchResult {
    inner: BatchResponse,
}

impl BatchResult {
    /// Check if all operations completed successfully
    pub fn success(&self) -> bool {
        self.inner.success
    }

    /// Get the number of operations that completed
    pub fn ops_completed(&self) -> u8 {
        self.inner.ops_completed
    }

    /// Get a specific read result by index
    ///
    /// Index refers to the nth result that returned data (not the nth operation).
    pub fn get_read(&self, index: usize) -> Option<&[u8]> {
        match self.inner.results.get(index)? {
            BatchOpResult::Data(data) => Some(data.as_slice()),
            _ => None,
        }
    }

    /// Get all results
    pub fn results(&self) -> &[BatchOpResult] {
        &self.inner.results
    }

    /// Iterate over all read data results
    pub fn reads(&self) -> impl Iterator<Item = &[u8]> {
        self.inner.results.iter().filter_map(|r| match r {
            BatchOpResult::Data(data) => Some(data.as_slice()),
            _ => None,
        })
    }
}

// Conversion helpers between core types and ICD types

fn core_io_mode_to_icd(mode: CoreIoMode) -> IoMode {
    match mode {
        CoreIoMode::Single => IoMode::Single,
        CoreIoMode::DualOut => IoMode::DualOut,
        CoreIoMode::DualIo => IoMode::DualIo,
        CoreIoMode::QuadOut => IoMode::QuadOut,
        CoreIoMode::QuadIo => IoMode::QuadIo,
        CoreIoMode::Qpi => IoMode::Qpi,
    }
}

fn core_addr_width_to_icd(width: CoreAddressWidth) -> AddressWidth {
    match width {
        CoreAddressWidth::None => AddressWidth::None,
        CoreAddressWidth::ThreeByte => AddressWidth::ThreeByte,
        CoreAddressWidth::FourByte => AddressWidth::FourByte,
    }
}

impl SpiMaster for PostcardSpi {
    fn features(&self) -> SpiFeatures {
        let mut features = SpiFeatures::FOUR_BYTE_ADDR;
        let modes = &self.info.supported_modes;

        if modes.contains(IoModeFlags::DUAL_OUT) {
            features |= SpiFeatures::DUAL_IN;
        }
        if modes.contains(IoModeFlags::DUAL_IO) {
            features |= SpiFeatures::DUAL_IO;
        }
        if modes.contains(IoModeFlags::QUAD_OUT) {
            features |= SpiFeatures::QUAD_IN;
        }
        if modes.contains(IoModeFlags::QUAD_IO) {
            features |= SpiFeatures::QUAD_IO;
        }
        if modes.contains(IoModeFlags::QPI) {
            features |= SpiFeatures::QPI;
        }

        features
    }

    fn max_read_len(&self) -> usize {
        self.info.max_transfer_size as usize
    }

    fn max_write_len(&self) -> usize {
        self.info.max_transfer_size as usize
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Check that the requested I/O mode is supported
        let icd_mode = core_io_mode_to_icd(cmd.io_mode);
        if !self.info.supported_modes.supports(icd_mode) {
            return Err(CoreError::IoModeNotSupported);
        }

        // Build a SpiTransaction
        let mut write_data = heapless::Vec::<u8, MAX_BATCH_TX_DATA>::new();
        write_data
            .extend_from_slice(cmd.write_data)
            .map_err(|_| CoreError::ProgrammerError)?;

        let tx = SpiTransaction {
            opcode: cmd.opcode,
            address: cmd.address,
            address_width: core_addr_width_to_icd(cmd.address_width),
            io_mode: icd_mode,
            dummy_cycles: cmd.dummy_cycles,
            write_data,
            read_len: cmd.read_buf.len() as u8,
        };

        // Execute via batch (single transaction)
        let mut batch = BatchBuilder::new();
        batch.transaction(tx);

        let result = self
            .execute_batch(batch)
            .map_err(|_| CoreError::ProgrammerError)?;

        // Copy read data if any
        if let Some(data) = result.get_read(0) {
            let copy_len = data.len().min(cmd.read_buf.len());
            cmd.read_buf[..copy_len].copy_from_slice(&data[..copy_len]);
        }

        Ok(())
    }

    fn delay_us(&mut self, us: u32) {
        let mut batch = BatchBuilder::new();
        batch.delay_us(us);
        let _ = self.execute_batch(batch);
    }
}

/// Options for opening a PostcardSpi device
#[derive(Debug, Clone, Default)]
pub struct PostcardSpiOptions {
    /// USB serial number to match (if any)
    pub serial: Option<String>,
    /// USB VID to match (defaults to ICD VID)
    pub vid: Option<u16>,
    /// USB PID to match (defaults to ICD PID)
    pub pid: Option<u16>,
    /// Initial SPI speed in Hz
    pub speed_hz: Option<u32>,
    /// Initial chip select
    pub cs: Option<u8>,
}

/// Parse options from key=value pairs
///
/// Supported options:
/// - `serial=<string>` - USB serial number
/// - `vid=<hex>` - USB Vendor ID (e.g., `vid=0x1234`)
/// - `pid=<hex>` - USB Product ID
/// - `spispeed=<hz>` - SPI clock speed in Hz (or with k/M suffix)
/// - `cs=<n>` - Chip select to use
pub fn parse_options(opts: &[(&str, &str)]) -> Result<PostcardSpiOptions> {
    let mut options = PostcardSpiOptions::default();

    for (key, value) in opts {
        match *key {
            "serial" => options.serial = Some(value.to_string()),
            "vid" => {
                let v = if let Some(hex) = value
                    .strip_prefix("0x")
                    .or_else(|| value.strip_prefix("0X"))
                {
                    u16::from_str_radix(hex, 16)
                } else {
                    value.parse()
                };
                options.vid = Some(
                    v.map_err(|_| Error::ConnectionFailed(format!("Invalid VID: {}", value)))?,
                );
            }
            "pid" => {
                let v = if let Some(hex) = value
                    .strip_prefix("0x")
                    .or_else(|| value.strip_prefix("0X"))
                {
                    u16::from_str_radix(hex, 16)
                } else {
                    value.parse()
                };
                options.pid = Some(
                    v.map_err(|_| Error::ConnectionFailed(format!("Invalid PID: {}", value)))?,
                );
            }
            "spispeed" => {
                let speed = parse_speed(value)?;
                options.speed_hz = Some(speed);
            }
            "cs" => {
                options.cs = Some(
                    value
                        .parse()
                        .map_err(|_| Error::ConnectionFailed(format!("Invalid CS: {}", value)))?,
                );
            }
            _ => return Err(Error::ConnectionFailed(format!("Unknown option: {}", key))),
        }
    }

    Ok(options)
}

/// Parse a speed value with optional suffix (k for kHz, M for MHz)
fn parse_speed(s: &str) -> Result<u32> {
    let s = s.trim().to_lowercase();

    if let Some(num) = s.strip_suffix('m') {
        let val: f64 = num
            .trim()
            .parse()
            .map_err(|_| Error::ConnectionFailed(format!("Invalid speed: {}", s)))?;
        return Ok((val * 1_000_000.0) as u32);
    }

    if let Some(num) = s.strip_suffix('k') {
        let val: f64 = num
            .trim()
            .parse()
            .map_err(|_| Error::ConnectionFailed(format!("Invalid speed: {}", s)))?;
        return Ok((val * 1_000.0) as u32);
    }

    s.parse()
        .map_err(|_| Error::ConnectionFailed(format!("Invalid speed: {}", s)))
}

/// Open a PostcardSpi device with the given options
pub fn open_with_options(options: &PostcardSpiOptions) -> Result<PostcardSpi> {
    let mut programmer = if let Some(ref serial) = options.serial {
        PostcardSpi::open_by_serial(serial)?
    } else {
        let vid = options.vid.unwrap_or(USB_VID);
        let pid = options.pid.unwrap_or(USB_PID);
        PostcardSpi::open_by_vid_pid(vid, pid)?
    };

    if let Some(speed) = options.speed_hz {
        programmer.set_speed(speed)?;
    }

    if let Some(cs) = options.cs {
        programmer.set_cs(cs)?;
    }

    Ok(programmer)
}

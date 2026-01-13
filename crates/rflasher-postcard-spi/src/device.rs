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

        block_on(async {
            self.client
                .send_resp::<SetCsEndpoint>(&SetCsReq { cs })
                .await
                .map_err(|e| Error::RpcError(format!("{:?}", e)))
        })?;

        self.info.current_cs = cs;
        log::debug!("Selected CS{}", cs);

        Ok(())
    }

    /// Execute an SPI transfer
    fn do_transfer(
        &mut self,
        req: SpiTransferReq,
        write_data: &[u8],
        read_buf: &mut [u8],
    ) -> Result<()> {
        // Check transfer size
        if write_data.len() > MAX_INLINE_DATA {
            return Err(Error::TransferTooLarge {
                requested: write_data.len(),
                max: MAX_INLINE_DATA,
            });
        }

        // Convert write_data to heapless::Vec
        let mut write_vec = heapless::Vec::<u8, MAX_INLINE_DATA>::new();
        write_vec
            .extend_from_slice(write_data)
            .map_err(|_| Error::TransferTooLarge {
                requested: write_data.len(),
                max: MAX_INLINE_DATA,
            })?;

        // Use the transfer_data endpoint which includes inline data
        let req_with_data = SpiTransferReqWithData {
            req,
            write_data: write_vec,
        };

        let resp: SpiTransferRespWithData = block_on(async {
            self.client
                .send_resp::<SpiTransferDataEndpoint>(&req_with_data)
                .await
                .map_err(|e| Error::RpcError(format!("{:?}", e)))
        })?;

        if !resp.resp.success {
            return Err(Error::RpcError("Transfer failed".into()));
        }

        // Copy read data
        let copy_len = resp.read_data.len().min(read_buf.len());
        read_buf[..copy_len].copy_from_slice(&resp.read_data[..copy_len]);

        Ok(())
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

        // Build the transfer request
        let req = SpiTransferReq {
            opcode: cmd.opcode,
            address: cmd.address,
            address_width: core_addr_width_to_icd(cmd.address_width),
            io_mode: icd_mode,
            dummy_cycles: cmd.dummy_cycles,
            write_len: cmd.write_data.len() as u16,
            read_len: cmd.read_buf.len() as u16,
        };

        // Execute the transfer
        self.do_transfer(req, cmd.write_data, cmd.read_buf)
            .map_err(|_| CoreError::ProgrammerError)?;

        Ok(())
    }

    fn delay_us(&mut self, us: u32) {
        let _ = block_on(async {
            self.client
                .send_resp::<DelayEndpoint>(&DelayReq { us })
                .await
        });
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

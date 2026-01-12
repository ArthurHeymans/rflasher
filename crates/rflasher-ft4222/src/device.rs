//! FT4222H device implementation
//!
//! This module provides the main `Ft4222` struct that implements USB
//! communication with the FT4222H SPI master and the `SpiMaster` trait.
//!
//! The FT4222H uses a vendor-specific USB protocol (not MPSSE/libftdi).
//! This implementation is based on flashprog's ft4222_spi.c which uses
//! raw libusb without the proprietary LibFT4222.

use std::time::Duration;

use nusb::transfer::{Buffer, Bulk, ControlIn, ControlOut, ControlType, In, Out, Recipient};
use nusb::{Endpoint, Interface, MaybeFuture};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};

use crate::error::{Ft4222Error, Result};
use crate::protocol::*;

/// FT4222H USB SPI Master programmer
///
/// This struct represents a connection to an FT4222H USB device and implements
/// the `SpiMaster` trait for communicating with SPI flash chips.
///
/// The FT4222H supports:
/// - Single, Dual, and Quad SPI modes
/// - SPI speeds from ~47 kHz to 40 MHz
/// - Up to 4 chip select lines (depending on mode)
/// - 4-byte addressing (software handled)
///
/// # Features
///
/// - High-speed USB 2.0 (480 Mbps)
/// - Configurable SPI clock from system clocks (60/24/48/80 MHz) with divisors
/// - Multiple I/O modes: single (1-1-1), dual (1-1-2, 1-2-2), quad (1-1-4, 1-4-4, 4-4-4)
/// - Pure USB implementation (no vendor library required)
pub struct Ft4222 {
    /// USB interface
    interface: Interface,
    /// Current SPI configuration
    config: SpiConfig,
    /// Selected clock configuration
    clock_config: ClockConfig,
    /// Control interface index (from USB descriptor)
    control_index: u8,
    /// Bulk IN endpoint address
    in_ep: u8,
    /// Bulk OUT endpoint address
    out_ep: u8,
    /// Current I/O lines mode
    io_lines: u8,
}

impl Ft4222 {
    /// Open an FT4222H device with default configuration
    ///
    /// Searches for an FT4222H device (VID:0403 PID:601c) and opens it.
    /// Returns an error if no device is found or if the device cannot be opened.
    pub fn open() -> Result<Self> {
        Self::open_with_config(SpiConfig::default())
    }

    /// Open an FT4222H device with custom configuration
    pub fn open_with_config(config: SpiConfig) -> Result<Self> {
        Self::open_nth_with_config(0, config)
    }

    /// Open the nth FT4222H device (0-indexed) with default configuration
    ///
    /// Useful when multiple FT4222H devices are connected.
    pub fn open_nth(index: usize) -> Result<Self> {
        Self::open_nth_with_config(index, SpiConfig::default())
    }

    /// Open the nth FT4222H device with custom configuration
    pub fn open_nth_with_config(index: usize, config: SpiConfig) -> Result<Self> {
        // Find FT4222H devices
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| Ft4222Error::OpenFailed(e.to_string()))?
            .filter(|d| d.vendor_id() == FTDI_VID && d.product_id() == FT4222H_PID)
            .collect();

        let device_info = devices.get(index).ok_or(Ft4222Error::DeviceNotFound)?;

        log::info!(
            "Opening FT4222H device at bus {} address {}",
            device_info.busnum(),
            device_info.device_address()
        );

        let device = device_info
            .open()
            .wait()
            .map_err(|e| Ft4222Error::OpenFailed(e.to_string()))?;

        log::debug!(
            "Device: VID={:04X} PID={:04X}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        // Get configuration descriptor to find endpoints
        let config_desc = device
            .active_configuration()
            .map_err(|e| Ft4222Error::OpenFailed(format!("Failed to get config: {}", e)))?;

        // Find the vendor-specific interface for SPI
        let mut spi_interface: Option<u8> = None;
        let mut in_ep: Option<u8> = None;
        let mut out_ep: Option<u8> = None;

        for iface in config_desc.interface_alt_settings() {
            // Look for vendor-specific class (0xFF) or interface 0
            if iface.class() == 0xFF || iface.interface_number() == 0 {
                for ep in iface.endpoints() {
                    if ep.transfer_type() == nusb::descriptors::TransferType::Bulk {
                        if ep.direction() == nusb::transfer::Direction::In {
                            in_ep = Some(ep.address());
                        } else {
                            out_ep = Some(ep.address());
                        }
                    }
                }
                if in_ep.is_some() && out_ep.is_some() {
                    spi_interface = Some(iface.interface_number());
                    break;
                }
            }
        }

        let iface_num = spi_interface.ok_or_else(|| {
            Ft4222Error::OpenFailed("Could not find suitable USB interface".to_string())
        })?;
        let in_ep = in_ep
            .ok_or_else(|| Ft4222Error::OpenFailed("Could not find IN endpoint".to_string()))?;
        let out_ep = out_ep
            .ok_or_else(|| Ft4222Error::OpenFailed("Could not find OUT endpoint".to_string()))?;

        log::debug!(
            "Using interface {}, IN EP 0x{:02X}, OUT EP 0x{:02X}",
            iface_num,
            in_ep,
            out_ep
        );

        // Claim interface
        let interface = device
            .claim_interface(iface_num)
            .wait()
            .map_err(|e| Ft4222Error::ClaimFailed(e.to_string()))?;

        // Calculate clock configuration
        let clock_config = find_clock_config(config.speed_khz);

        // Determine control index based on number of interfaces (matching flashprog)
        // LibFT4222 sets control_index = 1 if there are multiple interfaces, 0 otherwise
        let num_interfaces = config_desc.num_interfaces();
        let control_index = if num_interfaces > 1 { 1 } else { 0 };

        log::debug!(
            "Number of interfaces: {}, control_index: {}",
            num_interfaces,
            control_index
        );

        let mut ft4222 = Self {
            interface,
            config,
            clock_config,
            control_index,
            in_ep,
            out_ep,
            io_lines: 1, // Start with single I/O
        };

        // Initialize the device
        ft4222.init()?;

        Ok(ft4222)
    }

    /// Initialize the FT4222H for SPI master mode
    fn init(&mut self) -> Result<()> {
        // Query version info
        let (chip_version, version2, version3) = self.get_version()?;
        log::info!(
            "FT4222H version: chip=0x{:08X} (0x{:08X} 0x{:08X})",
            chip_version,
            version2,
            version3
        );

        // Query number of channels
        let channels = self.get_num_channels()?;
        log::debug!("FT4222H channels: {}", channels);

        // Validate CS selection
        if self.config.cs >= channels {
            return Err(Ft4222Error::InvalidParameter(format!(
                "CS{} not available (device has {} channels)",
                self.config.cs, channels
            )));
        }

        // Reset the device
        self.reset()?;

        // Set system clock
        self.set_sys_clock(self.clock_config.sys_clock)?;

        // Configure SPI master mode
        self.configure_spi_master()?;

        log::info!(
            "FT4222H configured: SPI clock = {} kHz, CS = {}, I/O mode = {:?}",
            self.clock_config.spi_clock_khz(),
            self.config.cs,
            self.config.io_mode
        );

        Ok(())
    }

    /// Get device version information (matching flashprog's ft4222_get_version)
    ///
    /// Returns (chip_version, version2, version3) - flashprog reads 12 bytes
    fn get_version(&self) -> Result<(u32, u32, u32)> {
        let data = self
            .interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: FT4222_INFO_REQUEST,
                    value: FT4222_GET_VERSION,
                    index: self.control_index as u16,
                    length: 12, // flashprog requests 12 bytes
                },
                Duration::from_secs(5),
            )
            .wait()
            .map_err(|e| Ft4222Error::TransferFailed(format!("Failed to get version: {}", e)))?;

        if data.len() < 12 {
            return Err(Ft4222Error::InvalidResponse(format!(
                "Version response too short: {} < 12",
                data.len()
            )));
        }

        // flashprog reads these as big-endian
        let chip_version = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let version2 = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let version3 = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

        Ok((chip_version, version2, version3))
    }

    /// Get number of CS channels available (matching flashprog's ft4222_get_num_channels)
    ///
    /// This queries GET_CONFIG and parses the mode byte to determine channel count.
    fn get_num_channels(&self) -> Result<u8> {
        let data = self
            .interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: FT4222_INFO_REQUEST,
                    value: FT4222_GET_CONFIG,
                    index: self.control_index as u16,
                    length: 13,
                },
                Duration::from_secs(5),
            )
            .wait()
            .map_err(|e| Ft4222Error::TransferFailed(format!("Failed to get config: {}", e)))?;

        if data.is_empty() {
            return Err(Ft4222Error::InvalidResponse(
                "Empty response for config".into(),
            ));
        }

        // Parse mode byte to determine number of channels (matching flashprog)
        let channels = match data[0] {
            0 => 1, // Mode 0
            1 => 3, // Mode 1
            2 => 4, // Mode 2
            3 => 1, // Mode 3
            mode => {
                return Err(Ft4222Error::InvalidResponse(format!(
                    "Unknown mode byte: 0x{:02x}",
                    mode
                )))
            }
        };

        log::debug!("FT4222H mode: {}, channels: {}", data[0], channels);
        Ok(channels)
    }

    /// Reset the device (matching flashprog's ft4222_reset)
    fn reset(&self) -> Result<()> {
        // Reset SIO - note: wIndex = 0, not control_index
        self.control_out_with_index(FT4222_RESET_REQUEST, FT4222_RESET_SIO, 0, &[])?;

        // Flush output buffer multiple times (flashprog does this 6 times)
        self.flush()?;

        log::debug!("FT4222H reset complete");
        Ok(())
    }

    /// Flush buffers (matching flashprog's ft4222_flush)
    fn flush(&self) -> Result<()> {
        // Flush output buffer 6 times (as flashprog does)
        for _ in 0..6 {
            if let Err(e) = self.control_out(FT4222_RESET_REQUEST, FT4222_OUTPUT_FLUSH, &[]) {
                log::warn!("FT4222 output flush failed: {}", e);
                break;
            }
        }

        // Flush input buffer once
        if let Err(e) = self.control_out(FT4222_RESET_REQUEST, FT4222_INPUT_FLUSH, &[]) {
            log::warn!("FT4222 input flush failed: {}", e);
        }

        Ok(())
    }

    /// Set system clock
    fn set_sys_clock(&self, clock: SystemClock) -> Result<()> {
        self.config_request(FT4222_SET_CLOCK, clock.index() as u8)?;
        log::debug!("Set system clock to {} MHz", clock.to_khz() / 1000);
        Ok(())
    }

    /// Configure SPI master mode (matching flashprog's ft4222_configure_spi_master)
    fn configure_spi_master(&mut self) -> Result<()> {
        let cs = self.config.cs;

        // Reset transaction for this CS (idx => cs)
        self.config_request(FT4222_SPI_RESET_TRANSACTION, cs)?;

        // Set I/O lines (start with single)
        self.io_lines = 1;
        self.config_request(FT4222_SPI_SET_IO_LINES, 1)?;

        // Set clock divisor
        self.config_request(
            FT4222_SPI_SET_CLK_DIV,
            self.clock_config.divisor.value() as u8,
        )?;

        // Set clock polarity (idle low for SPI mode 0)
        self.config_request(FT4222_SPI_SET_CLK_IDLE, FT4222_CLK_IDLE_LOW)?;

        // Set clock phase (capture on leading edge for SPI mode 0)
        self.config_request(FT4222_SPI_SET_CAPTURE, FT4222_CLK_CAPTURE_LEADING)?;

        // Set CS polarity (active low)
        self.config_request(FT4222_SPI_SET_CS_ACTIVE, FT4222_CS_ACTIVE_LOW)?;

        // Set CS mask for selected chip select
        self.config_request(FT4222_SPI_SET_CS_MASK, 1 << cs)?;

        // Set mode to SPI Master
        self.config_request(FT4222_SET_MODE, FT4222_MODE_SPI_MASTER)?;

        Ok(())
    }

    /// Set I/O lines mode (matching flashprog's ft4222_spi_set_io_lines)
    fn set_io_lines(&mut self, lines: u8) -> Result<()> {
        if lines != self.io_lines {
            self.config_request(FT4222_SPI_SET_IO_LINES, lines)?;
            // Reset line number after changing I/O lines
            self.config_request(FT4222_SPI_RESET, FT4222_SPI_RESET_LINE_NUM)?;
            self.io_lines = lines;
            log::trace!("Set I/O lines to {}", lines);
        }
        Ok(())
    }

    /// Send a control OUT transfer with default control_index
    fn control_out(&self, request: u8, value: u16, data: &[u8]) -> Result<()> {
        self.control_out_with_index(request, value, self.control_index as u16, data)
    }

    /// Send a control OUT transfer with explicit index
    fn control_out_with_index(
        &self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<()> {
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value,
                    index,
                    data,
                },
                Duration::from_secs(5),
            )
            .wait()
            .map_err(|e| Ft4222Error::TransferFailed(format!("Control transfer failed: {}", e)))?;

        Ok(())
    }

    /// Send a config request (matching flashprog's ft4222_config_request)
    ///
    /// The FT4222H encodes config commands with:
    /// - wValue = (data << 8) | cmd
    /// - wIndex = control_index
    fn config_request(&self, cmd: u8, data: u8) -> Result<()> {
        let value = ((data as u16) << 8) | (cmd as u16);
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: FT4222_CONFIG_REQUEST,
                    value,
                    index: self.control_index as u16,
                    data: &[],
                },
                Duration::from_secs(5),
            )
            .wait()
            .map_err(|e| Ft4222Error::TransferFailed(format!("Control transfer failed: {}", e)))?;

        Ok(())
    }

    /// Write data to bulk OUT endpoint
    ///
    /// For large transfers, we split into smaller chunks to avoid USB stalls.
    fn bulk_write(&mut self, data: &[u8]) -> Result<()> {
        let mut out_ep: Endpoint<Bulk, Out> = self
            .interface
            .endpoint(self.out_ep)
            .map_err(|e| Ft4222Error::TransferFailed(e.to_string()))?;

        // For empty packet (CS deassert), just send it directly
        if data.is_empty() {
            let out_buf = Buffer::new(0);
            let completion = out_ep.transfer_blocking(out_buf, Duration::from_secs(30));
            completion
                .into_result()
                .map_err(|e| Ft4222Error::TransferFailed(format!("Empty packet failed: {}", e)))?;
            log::trace!("Bulk write empty packet (CS deassert)");
            return Ok(());
        }

        // Split large transfers into chunks (max 2048 bytes per transfer)
        const MAX_CHUNK: usize = 2048;
        let mut offset = 0;

        while offset < data.len() {
            let chunk_len = std::cmp::min(MAX_CHUNK, data.len() - offset);
            let chunk = &data[offset..offset + chunk_len];

            let mut out_buf = Buffer::new(chunk_len);
            out_buf.extend_from_slice(chunk);

            // Use longer timeout (matching flashprog's 16*2000ms = 32s)
            let completion = out_ep.transfer_blocking(out_buf, Duration::from_secs(30));

            completion.into_result().map_err(|e| {
                Ft4222Error::TransferFailed(format!(
                    "Bulk write failed at offset {}: {}",
                    offset, e
                ))
            })?;

            offset += chunk_len;
        }

        log::trace!("Bulk write {} bytes", data.len());
        Ok(())
    }

    /// Read data from bulk IN endpoint
    fn bulk_read(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut in_ep: Endpoint<Bulk, In> = self
            .interface
            .endpoint(self.in_ep)
            .map_err(|e| Ft4222Error::TransferFailed(e.to_string()))?;

        let max_packet_size = in_ep.max_packet_size();
        let mut result = Vec::new();
        let mut remaining = len;

        while remaining > 0 {
            let request_len = std::cmp::min(remaining + MODEM_STATUS_SIZE, READ_BUFFER_SIZE);
            // Request length must be multiple of max packet size
            let aligned_len = request_len.div_ceil(max_packet_size) * max_packet_size;

            let mut in_buf = Buffer::new(aligned_len);
            in_buf.set_requested_len(aligned_len);

            // Use longer timeout (matching flashprog's approach)
            let completion = in_ep.transfer_blocking(in_buf, Duration::from_secs(30));

            let data = completion
                .into_result()
                .map_err(|e| Ft4222Error::TransferFailed(format!("Bulk read failed: {}", e)))?;

            if data.len() < MODEM_STATUS_SIZE {
                return Err(Ft4222Error::InvalidResponse("Response too short".into()));
            }

            // Skip modem status bytes
            let payload = &data[MODEM_STATUS_SIZE..];
            let to_copy = std::cmp::min(payload.len(), remaining);
            result.extend_from_slice(&payload[..to_copy]);
            remaining -= to_copy;
        }

        log::trace!("Bulk read {} bytes", result.len());
        Ok(result)
    }

    /// Perform a single-I/O SPI transfer (full duplex)
    ///
    /// This is used for standard 1-1-1 mode transfers.
    fn spi_transfer_single(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        // Set to single I/O mode
        self.set_io_lines(1)?;

        // Total transfer length (we need to clock out dummy bytes for read)
        let total_len = write_data.len() + read_len;

        if total_len == 0 {
            return Ok(Vec::new());
        }

        // Build output buffer: write data + dummy bytes for read
        let mut out_buf = Vec::with_capacity(total_len);
        out_buf.extend_from_slice(write_data);
        out_buf.resize(total_len, 0x00); // Dummy bytes for read phase

        // Write data
        self.bulk_write(&out_buf)?;

        // Send empty packet to deassert CS
        self.bulk_write(&[])?;

        // Read response (includes bytes shifted during write phase)
        let response = self.bulk_read(total_len)?;

        // Skip bytes received during write phase, return only read data
        if response.len() >= total_len {
            Ok(response[write_data.len()..].to_vec())
        } else {
            Err(Ft4222Error::InvalidResponse(format!(
                "Expected {} bytes, got {}",
                total_len,
                response.len()
            )))
        }
    }

    /// Perform a multi-I/O SPI transfer (half duplex)
    ///
    /// This is used for dual/quad mode transfers with separate write and read phases.
    /// Format: | single-I/O phase | multi-I/O write phase | multi-I/O read phase |
    #[allow(dead_code)]
    fn spi_transfer_multi(
        &mut self,
        single_data: &[u8],
        multi_write_data: &[u8],
        multi_read_len: usize,
        io_lines: u8,
    ) -> Result<Vec<u8>> {
        // Validate lengths
        if single_data.len() > MULTI_IO_MAX_SINGLE {
            return Err(Ft4222Error::InvalidParameter(format!(
                "Single phase too long: {} > {}",
                single_data.len(),
                MULTI_IO_MAX_SINGLE
            )));
        }
        if multi_write_data.len() > MULTI_IO_MAX_DATA {
            return Err(Ft4222Error::InvalidParameter(format!(
                "Multi-write phase too long: {} > {}",
                multi_write_data.len(),
                MULTI_IO_MAX_DATA
            )));
        }
        if multi_read_len > MULTI_IO_MAX_DATA {
            return Err(Ft4222Error::InvalidParameter(format!(
                "Multi-read phase too long: {} > {}",
                multi_read_len, MULTI_IO_MAX_DATA
            )));
        }

        // Set I/O lines for multi-I/O phase
        self.set_io_lines(io_lines)?;

        // Build multi-I/O header
        // Format: | 0x8 | single_len (4 bits) | multi_write_len (16 bits) | multi_read_len (16 bits) |
        let mut header = [0u8; MULTI_IO_HEADER_SIZE];
        header[0] = MULTI_IO_MAGIC | (single_data.len() as u8 & 0x0F);
        header[1] = (multi_write_data.len() & 0xFF) as u8;
        header[2] = ((multi_write_data.len() >> 8) & 0xFF) as u8;
        header[3] = (multi_read_len & 0xFF) as u8;
        header[4] = ((multi_read_len >> 8) & 0xFF) as u8;

        // Build complete output buffer
        let mut out_buf =
            Vec::with_capacity(MULTI_IO_HEADER_SIZE + single_data.len() + multi_write_data.len());
        out_buf.extend_from_slice(&header);
        out_buf.extend_from_slice(single_data);
        out_buf.extend_from_slice(multi_write_data);

        // Write data
        self.bulk_write(&out_buf)?;

        // Send empty packet to deassert CS
        self.bulk_write(&[])?;

        // Read response
        if multi_read_len > 0 {
            self.bulk_read(multi_read_len)
        } else {
            Ok(Vec::new())
        }
    }

    /// List all connected FT4222H devices
    pub fn list_devices() -> Result<Vec<Ft4222DeviceInfo>> {
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| Ft4222Error::OpenFailed(e.to_string()))?
            .filter(|d| d.vendor_id() == FTDI_VID && d.product_id() == FT4222H_PID)
            .map(|d| Ft4222DeviceInfo {
                bus: d.busnum(),
                address: d.device_address(),
            })
            .collect();

        Ok(devices)
    }

    /// Get the current SPI configuration
    pub fn config(&self) -> &SpiConfig {
        &self.config
    }

    /// Get the actual SPI clock speed in kHz
    pub fn actual_speed_khz(&self) -> u32 {
        self.clock_config.spi_clock_khz()
    }
}

impl SpiMaster for Ft4222 {
    fn features(&self) -> SpiFeatures {
        // FT4222H supports 4-byte addressing (software handled)
        // Dual I/O is supported but we only implement single mode for now
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        // Limit to smaller chunks to avoid buffer overflow in single-I/O mode.
        // In single-I/O (full-duplex), every byte sent also receives a byte.
        // The FT4222's internal RX buffer can overflow if we send too much
        // without reading. Using smaller transfers works around this.
        // flashprog uses async transfers to interleave read/write, but we
        // use blocking transfers, so we need smaller chunks.
        256
    }

    fn max_write_len(&self) -> usize {
        // For writes (like page program), we also need smaller chunks
        256
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Check that the requested I/O mode is supported
        check_io_mode_supported(cmd.io_mode, self.features())?;

        // Build the command bytes to send
        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];

        // Encode opcode + address + dummy bytes
        cmd.encode_header(&mut write_data);

        // Append write data (for write commands)
        write_data[header_len..].copy_from_slice(cmd.write_data);

        // Perform the transfer using single I/O mode
        let read_len = cmd.read_buf.len();
        let result = self
            .spi_transfer_single(&write_data, read_len)
            .map_err(|_e| CoreError::ProgrammerError)?;

        // Copy read data back
        if !result.is_empty() {
            cmd.read_buf.copy_from_slice(&result);
        }

        Ok(())
    }

    fn delay_us(&mut self, us: u32) {
        // Simple sleep-based delay
        if us > 0 {
            std::thread::sleep(Duration::from_micros(us as u64));
        }
    }
}

/// Information about a connected FT4222H device
#[derive(Debug, Clone)]
pub struct Ft4222DeviceInfo {
    /// USB bus number
    pub bus: u8,
    /// USB device address
    pub address: u8,
}

impl std::fmt::Display for Ft4222DeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FT4222H at bus {} address {}", self.bus, self.address)
    }
}

/// Parse programmer options for FT4222
///
/// Supported options:
/// - `spispeed=<khz>`: Target SPI clock speed in kHz (default: 10000)
/// - `cs=<0-3>`: Which chip select to use (default: 0)
/// - `iomode=<single|dual|quad>`: I/O mode (default: single)
///
/// # Example
///
/// ```ignore
/// let options = [("spispeed", "30000"), ("cs", "1")];
/// let config = parse_options(&options)?;
/// ```
pub fn parse_options(options: &[(&str, &str)]) -> Result<SpiConfig> {
    let mut config = SpiConfig::default();

    for (key, value) in options {
        match *key {
            "spispeed" => {
                let khz: u32 = value.parse().map_err(|_| {
                    Ft4222Error::InvalidParameter(format!("Invalid spispeed value: {}", value))
                })?;
                config.speed_khz = khz;
                log::debug!("Setting target SPI speed to {} kHz", khz);
            }
            "cs" => {
                let cs: u8 = value.parse().map_err(|_| {
                    Ft4222Error::InvalidParameter(format!("Invalid cs value: {}", value))
                })?;
                if cs > 3 {
                    return Err(Ft4222Error::InvalidParameter(format!(
                        "Invalid cs: {} (must be 0-3)",
                        cs
                    )));
                }
                config.cs = cs;
            }
            "iomode" => {
                config.io_mode = IoMode::parse(value).ok_or_else(|| {
                    Ft4222Error::InvalidParameter(format!(
                        "Invalid iomode: {} (must be single, dual, or quad)",
                        value
                    ))
                })?;
            }
            _ => {
                log::warn!("Unknown FT4222 option: {}={}", key, value);
            }
        }
    }

    Ok(config)
}

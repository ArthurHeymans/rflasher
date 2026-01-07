//! CH347 device implementation
//!
//! This module provides the main `Ch347` struct that implements USB
//! communication with the CH347 programmer and the `SpiMaster` trait.

use std::time::Duration;

use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::{Endpoint, MaybeFuture};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};

use crate::error::{Ch347Error, Result};
use crate::protocol::*;

/// CH347 USB programmer
///
/// This struct represents a connection to a CH347 USB device and implements
/// the `SpiMaster` trait for communicating with SPI flash chips.
///
/// The CH347 is a high-speed USB 2.0 to SPI/I2C/JTAG/UART bridge that supports
/// SPI clock speeds up to 60 MHz and has two chip select lines.
///
/// # Features
///
/// - High-speed USB 2.0 (480 Mbps)
/// - SPI speeds from 468.75 kHz to 60 MHz
/// - Two chip select lines (CS0, CS1)
/// - SPI modes 0-3
/// - 4-byte address support (software handled)
/// - Standard single-bit SPI only (dual/quad modes not supported)
pub struct Ch347 {
    /// Bulk OUT endpoint for writes
    out_ep: Endpoint<Bulk, Out>,
    /// Bulk IN endpoint for reads
    in_ep: Endpoint<Bulk, In>,
    /// Current SPI configuration
    config: SpiConfig,
    /// Device variant (T or F)
    variant: Ch347Variant,
}

impl Ch347 {
    /// Open a CH347 device with default configuration
    ///
    /// Searches for a CH347 device (VID:1a86 PID:55db or 55de) and opens it.
    /// Returns an error if no device is found or if the device cannot be opened.
    pub fn open() -> Result<Self> {
        Self::open_with_config(SpiConfig::default())
    }

    /// Open a CH347 device with custom configuration
    pub fn open_with_config(config: SpiConfig) -> Result<Self> {
        Self::open_nth_with_config(0, config)
    }

    /// Open the nth CH347 device (0-indexed) with default configuration
    ///
    /// Useful when multiple CH347 devices are connected.
    pub fn open_nth(index: usize) -> Result<Self> {
        Self::open_nth_with_config(index, SpiConfig::default())
    }

    /// Open the nth CH347 device with custom configuration
    pub fn open_nth_with_config(index: usize, config: SpiConfig) -> Result<Self> {
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| Ch347Error::OpenFailed(e.to_string()))?
            .filter(|d| {
                d.vendor_id() == CH347_USB_VENDOR
                    && (d.product_id() == CH347T_USB_PRODUCT
                        || d.product_id() == CH347F_USB_PRODUCT)
            })
            .collect();

        let device_info = devices.get(index).ok_or(Ch347Error::DeviceNotFound)?;
        let variant = Ch347Variant::from_product_id(device_info.product_id())
            .ok_or(Ch347Error::DeviceNotFound)?;

        log::info!(
            "Opening CH347{} device at bus {} address {}",
            if variant == Ch347Variant::Ch347T {
                "T"
            } else {
                "F"
            },
            device_info.busnum(),
            device_info.device_address()
        );

        let device = device_info
            .open()
            .wait()
            .map_err(|e| Ch347Error::OpenFailed(e.to_string()))?;

        // Get device descriptor for version info
        let desc = device_info;
        log::debug!(
            "Device: VID={:04X} PID={:04X}",
            desc.vendor_id(),
            desc.product_id()
        );

        // Find the vendor-specific interface for SPI
        // CH347T uses interface 2, CH347F uses interface 4
        let config_desc = device
            .active_configuration()
            .map_err(|e| Ch347Error::OpenFailed(format!("Failed to get config: {}", e)))?;

        let mut spi_interface: Option<u8> = None;
        for iface in config_desc.interface_alt_settings() {
            if iface.class() == 0xFF {
                // LIBUSB_CLASS_VENDOR_SPEC
                spi_interface = Some(iface.interface_number());
                break;
            }
        }

        let iface_num = spi_interface.ok_or_else(|| {
            Ch347Error::OpenFailed("Could not find vendor-specific interface".to_string())
        })?;

        log::debug!("Using interface {}", iface_num);

        // Claim interface
        let interface = device
            .claim_interface(iface_num)
            .wait()
            .map_err(|e| Ch347Error::ClaimFailed(e.to_string()))?;

        // Open bulk endpoints
        let out_ep = interface
            .endpoint::<Bulk, Out>(WRITE_EP)
            .map_err(|e| Ch347Error::ClaimFailed(e.to_string()))?;
        let in_ep = interface
            .endpoint::<Bulk, In>(READ_EP)
            .map_err(|e| Ch347Error::ClaimFailed(e.to_string()))?;

        let mut ch347 = Self {
            out_ep,
            in_ep,
            config,
            variant,
        };

        // Configure the device for SPI mode
        ch347.configure()?;

        Ok(ch347)
    }

    /// List all connected CH347 devices
    pub fn list_devices() -> Result<Vec<Ch347DeviceInfo>> {
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| Ch347Error::OpenFailed(e.to_string()))?
            .filter(|d| {
                d.vendor_id() == CH347_USB_VENDOR
                    && (d.product_id() == CH347T_USB_PRODUCT
                        || d.product_id() == CH347F_USB_PRODUCT)
            })
            .map(|d| {
                let variant =
                    Ch347Variant::from_product_id(d.product_id()).unwrap_or(Ch347Variant::Ch347T);
                Ch347DeviceInfo {
                    bus: d.busnum(),
                    address: d.device_address(),
                    variant,
                }
            })
            .collect();

        Ok(devices)
    }

    /// Get the current SPI configuration
    pub fn config(&self) -> &SpiConfig {
        &self.config
    }

    /// Get the device variant
    pub fn variant(&self) -> Ch347Variant {
        self.variant
    }

    /// Update SPI configuration
    ///
    /// This sends the new configuration to the device.
    pub fn set_config(&mut self, config: SpiConfig) -> Result<()> {
        self.config = config;
        self.configure()
    }

    /// Set the SPI clock speed
    pub fn set_speed(&mut self, speed: SpiSpeed) -> Result<()> {
        self.config.speed = speed;
        self.configure()
    }

    /// Set which chip select to use
    pub fn set_cs(&mut self, cs: ChipSelect) -> Result<()> {
        self.config.cs = cs;
        self.configure()
    }

    /// Set the SPI mode
    pub fn set_mode(&mut self, mode: SpiMode) -> Result<()> {
        self.config.mode = mode;
        self.configure()
    }

    /// Configure the CH347 for SPI mode
    fn configure(&mut self) -> Result<()> {
        let config_buf = self.config.build_config_buffer();

        // Send configuration
        self.usb_write(&config_buf)?;

        // Read response (the device echoes back the config)
        let mut response = vec![0u8; 29];
        self.usb_read(&mut response)?;

        log::info!(
            "CH347 configured: speed={}kHz, mode={}, cs={}",
            self.config.speed.to_khz(),
            self.config.mode as u8,
            self.config.cs as u8
        );

        Ok(())
    }

    /// Control chip select lines
    fn cs_control(&mut self, assert: bool) -> Result<()> {
        let cs_value = if assert {
            CH347_CS_ASSERT | CH347_CS_CHANGE
        } else {
            CH347_CS_DEASSERT | CH347_CS_CHANGE
        };

        // Build CS control command
        // Format: [cmd, len_lo, len_hi, cs1_ctrl, 0, 0, 0, 0, cs2_ctrl, 0, 0, 0, 0]
        let mut cmd = [0u8; 13];
        cmd[0] = CH347_CMD_SPI_CS_CTRL;
        cmd[1] = 10; // payload length (low byte)
        cmd[2] = 0; // payload length (high byte)

        match self.config.cs {
            ChipSelect::CS0 => {
                cmd[3] = cs_value;
                cmd[8] = CH347_CS_IGNORE;
            }
            ChipSelect::CS1 => {
                cmd[3] = CH347_CS_IGNORE;
                cmd[8] = cs_value;
            }
        }

        self.usb_write(&cmd)?;

        Ok(())
    }

    /// Write data via SPI (CS must already be asserted)
    fn spi_write(&mut self, data: &[u8]) -> Result<()> {
        let mut bytes_written = 0;
        let mut resp_buf = [0u8; 4];

        while bytes_written < data.len() {
            let chunk_len = std::cmp::min(CH347_MAX_DATA_LEN, data.len() - bytes_written);
            let packet_len = chunk_len + 3;

            let mut buffer = vec![0u8; packet_len];
            buffer[0] = CH347_CMD_SPI_OUT;
            buffer[1] = (chunk_len & 0xFF) as u8;
            buffer[2] = ((chunk_len >> 8) & 0xFF) as u8;
            buffer[3..3 + chunk_len]
                .copy_from_slice(&data[bytes_written..bytes_written + chunk_len]);

            self.usb_write(&buffer)?;

            // Read acknowledgment
            self.usb_read(&mut resp_buf)?;

            bytes_written += chunk_len;
        }

        Ok(())
    }

    /// Read data via SPI (CS must already be asserted)
    fn spi_read(&mut self, data: &mut [u8]) -> Result<()> {
        let readcnt = data.len();

        // Build read command
        // Format: [cmd, len_lo, len_hi, count_b0, count_b1, count_b2, count_b3]
        let command_buf = [
            CH347_CMD_SPI_IN,
            4,
            0,
            (readcnt & 0xFF) as u8,
            ((readcnt >> 8) & 0xFF) as u8,
            ((readcnt >> 16) & 0xFF) as u8,
            ((readcnt >> 24) & 0xFF) as u8,
        ];

        self.usb_write(&command_buf)?;

        // Read response packets
        let mut bytes_read = 0;
        let mut buffer = vec![0u8; CH347_PACKET_SIZE];

        while bytes_read < readcnt {
            let received = self.usb_read(&mut buffer)?;

            if received < 3 {
                return Err(Ch347Error::InvalidResponse(
                    "Response too short".to_string(),
                ));
            }

            // Response format: [cmd, len_lo, len_hi, data...]
            let data_len = (buffer[1] as usize) | ((buffer[2] as usize) << 8);

            if received < 3 + data_len {
                return Err(Ch347Error::InvalidResponse(format!(
                    "Incomplete response: got {} bytes, expected {}",
                    received,
                    3 + data_len
                )));
            }

            let to_copy = std::cmp::min(data_len, readcnt - bytes_read);
            data[bytes_read..bytes_read + to_copy].copy_from_slice(&buffer[3..3 + to_copy]);
            bytes_read += to_copy;
        }

        Ok(())
    }

    /// Perform an SPI transfer (write then read)
    fn spi_transfer(&mut self, write_data: &[u8], read_buf: &mut [u8]) -> Result<()> {
        // Assert CS
        self.cs_control(true)?;

        // Write phase
        if !write_data.is_empty() {
            self.spi_write(write_data)?;
        }

        // Read phase
        if !read_buf.is_empty() {
            self.spi_read(read_buf)?;
        }

        // Deassert CS
        self.cs_control(false)?;

        Ok(())
    }

    /// Write data to USB endpoint (blocking)
    fn usb_write(&mut self, data: &[u8]) -> Result<()> {
        let mut buf = Buffer::new(data.len());
        buf.extend_from_slice(data);

        let completion = self.out_ep.transfer_blocking(buf, Duration::from_secs(5));

        completion
            .into_result()
            .map_err(|e| Ch347Error::TransferFailed(e.to_string()))?;

        log::trace!("USB write {} bytes", data.len());
        Ok(())
    }

    /// Read data from USB endpoint (blocking)
    fn usb_read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let max_packet_size = self.in_ep.max_packet_size();
        // Request length must be multiple of max packet size
        let request_len = buffer.len().div_ceil(max_packet_size) * max_packet_size;
        let mut in_buf = Buffer::new(request_len);
        in_buf.set_requested_len(request_len);

        let completion = self.in_ep.transfer_blocking(in_buf, Duration::from_secs(5));

        let data = completion
            .into_result()
            .map_err(|e| Ch347Error::TransferFailed(e.to_string()))?;

        let received = std::cmp::min(data.len(), buffer.len());
        buffer[..received].copy_from_slice(&data[..received]);

        log::trace!("USB read {} bytes", received);
        Ok(received)
    }
}

impl SpiMaster for Ch347 {
    fn features(&self) -> SpiFeatures {
        // CH347 supports 4-byte addressing (software handled)
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        // CH347 can handle large transfers
        // The protocol supports reading up to 2^32-1 bytes in one go
        64 * 1024
    }

    fn max_write_len(&self) -> usize {
        // CH347 can handle large transfers
        64 * 1024
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

        // Perform the transfer
        self.spi_transfer(&write_data, cmd.read_buf)
            .map_err(|_e| CoreError::ProgrammerError)?;

        Ok(())
    }

    fn delay_us(&mut self, us: u32) {
        // Simple sleep-based delay
        // The CH347 doesn't have a built-in delay command like the CH341A
        if us > 0 {
            std::thread::sleep(Duration::from_micros(us as u64));
        }
    }
}

/// Information about a connected CH347 device
#[derive(Debug, Clone)]
pub struct Ch347DeviceInfo {
    /// USB bus number
    pub bus: u8,
    /// USB device address
    pub address: u8,
    /// Device variant
    pub variant: Ch347Variant,
}

impl std::fmt::Display for Ch347DeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CH347{} at bus {} address {}",
            if self.variant == Ch347Variant::Ch347T {
                "T"
            } else {
                "F"
            },
            self.bus,
            self.address
        )
    }
}

/// Parse programmer options for CH347
///
/// Supported options:
/// - `spispeed=<khz>`: SPI clock speed in kHz (default: 7500)
/// - `spimode=<0-3>`: SPI mode (default: 0)
/// - `cs=<0|1>`: Which chip select to use (default: 0)
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
                    Ch347Error::ConfigError(format!("Invalid spispeed value: {}", value))
                })?;
                config.speed = SpiSpeed::from_khz(khz);
                log::debug!(
                    "Setting SPI speed to {}kHz (actual: {}kHz)",
                    khz,
                    config.speed.to_khz()
                );
            }
            "spimode" => {
                let mode: u8 = value.parse().map_err(|_| {
                    Ch347Error::ConfigError(format!("Invalid spimode value: {}", value))
                })?;
                config.mode = match mode {
                    0 => SpiMode::Mode0,
                    1 => SpiMode::Mode1,
                    2 => SpiMode::Mode2,
                    3 => SpiMode::Mode3,
                    _ => {
                        return Err(Ch347Error::ConfigError(format!(
                            "Invalid spimode: {} (must be 0-3)",
                            mode
                        )))
                    }
                };
            }
            "cs" => {
                let cs: u8 = value
                    .parse()
                    .map_err(|_| Ch347Error::ConfigError(format!("Invalid cs value: {}", value)))?;
                config.cs = match cs {
                    0 => ChipSelect::CS0,
                    1 => ChipSelect::CS1,
                    _ => {
                        return Err(Ch347Error::ConfigError(format!(
                            "Invalid cs: {} (must be 0 or 1)",
                            cs
                        )))
                    }
                };
            }
            _ => {
                log::warn!("Unknown CH347 option: {}={}", key, value);
            }
        }
    }

    Ok(config)
}

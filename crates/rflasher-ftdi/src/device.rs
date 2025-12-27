//! FTDI MPSSE device implementation
//!
//! This module provides the main `Ftdi` struct that implements SPI
//! communication using FTDI's MPSSE engine and the `SpiMaster` trait.

use std::io::{Read, Write};
use std::time::Duration;

use ftdi::{find_by_vid_pid, BitMode, Device, Interface};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};

use crate::error::{FtdiError, Result};
use crate::protocol::*;

/// Configuration for opening an FTDI device
#[derive(Debug, Clone)]
pub struct FtdiConfig {
    /// Device type (determines VID/PID and pin configuration)
    pub device_type: FtdiDeviceType,
    /// Interface/channel to use (A, B, C, D)
    pub interface: FtdiInterface,
    /// Clock divisor (2-131072, must be even)
    /// SPI clock = base_clock / divisor
    /// Base clock is 60 MHz for 'H' devices
    pub divisor: u16,
    /// CS pin bits (which pins are CS)
    pub cs_bits: u8,
    /// Auxiliary bits (extra pins to set high)
    pub aux_bits: u8,
    /// Pin direction (which pins are outputs)
    pub pindir: u8,
    /// High-byte pin direction
    pub pindir_high: u8,
    /// High-byte auxiliary bits
    pub aux_bits_high: u8,
    /// USB serial number filter (optional)
    pub serial: Option<String>,
    /// USB description filter (optional)
    pub description: Option<String>,
}

impl Default for FtdiConfig {
    fn default() -> Self {
        let device_type = FtdiDeviceType::default();
        FtdiConfig {
            device_type,
            interface: FtdiInterface::default(),
            divisor: device_type.default_divisor(),
            cs_bits: device_type.default_cs_bits(),
            aux_bits: device_type.default_aux_bits(),
            pindir: device_type.default_pindir(),
            pindir_high: device_type.default_pindir_high(),
            aux_bits_high: 0,
            serial: None,
            description: None,
        }
    }
}

impl FtdiConfig {
    /// Create a new config for a specific device type
    pub fn for_device(device_type: FtdiDeviceType) -> Self {
        FtdiConfig {
            device_type,
            interface: FtdiInterface::default(),
            divisor: device_type.default_divisor(),
            cs_bits: device_type.default_cs_bits(),
            aux_bits: device_type.default_aux_bits(),
            pindir: device_type.default_pindir(),
            pindir_high: device_type.default_pindir_high(),
            aux_bits_high: 0,
            serial: None,
            description: None,
        }
    }

    /// Set the interface/channel
    pub fn interface(mut self, interface: FtdiInterface) -> Result<Self> {
        // Validate that the interface is available on this device
        let max_channel = self.device_type.channel_count();
        if interface.index() >= max_channel {
            return Err(FtdiError::InvalidChannel(format!(
                "Channel {} not available on {} (max: {})",
                interface.letter(),
                self.device_type.name(),
                (b'A' + max_channel - 1) as char
            )));
        }
        self.interface = interface;
        Ok(self)
    }

    /// Set the clock divisor
    pub fn divisor(mut self, divisor: u16) -> Result<Self> {
        if divisor < 2 || !divisor.is_multiple_of(2) {
            return Err(FtdiError::InvalidParameter(format!(
                "Invalid divisor {}: must be even, between 2 and 65534",
                divisor
            )));
        }
        self.divisor = divisor;
        Ok(self)
    }

    /// Set a GPIOL pin mode
    ///
    /// `pin` is 0-3 (GPIOL0-GPIOL3)
    /// `mode` is 'H' (high output), 'L' (low output), 'C' (CS output), or 'I' (input)
    pub fn gpiol(mut self, pin: u8, mode: char) -> Result<Self> {
        if pin > 3 {
            return Err(FtdiError::InvalidParameter(format!(
                "Invalid GPIOL pin {}: must be 0-3",
                pin
            )));
        }

        // Check if pin is reserved on this device type
        let reserved = self.device_type.default_pindir() & 0xF0;
        let bit = 1 << (pin + 4);
        if reserved & bit != 0 {
            return Err(FtdiError::InvalidParameter(format!(
                "GPIOL{} is reserved on {}",
                pin,
                self.device_type.name()
            )));
        }

        match mode.to_ascii_uppercase() {
            'H' => {
                // Output high
                self.aux_bits |= bit;
                self.pindir |= bit;
            }
            'L' => {
                // Output low
                self.aux_bits &= !bit;
                self.pindir |= bit;
            }
            'C' => {
                // CS output
                self.cs_bits |= bit;
                self.pindir |= bit;
            }
            'I' => {
                // Input (default)
                self.aux_bits &= !bit;
                self.cs_bits &= !bit;
                self.pindir &= !bit;
            }
            _ => {
                return Err(FtdiError::InvalidParameter(format!(
                    "Invalid GPIOL mode '{}': must be H, L, C, or I",
                    mode
                )));
            }
        }

        Ok(self)
    }

    /// Calculate the SPI clock frequency in MHz
    pub fn spi_clock_mhz(&self) -> f64 {
        let base_clock = if self.device_type.is_high_speed() {
            60.0
        } else {
            12.0
        };
        base_clock / self.divisor as f64
    }
}

/// FTDI MPSSE programmer
///
/// This struct represents a connection to an FTDI device using the MPSSE
/// engine for SPI communication.
pub struct Ftdi {
    /// libftdi device context
    device: Device,
    /// Current CS bits state
    cs_bits: u8,
    /// Auxiliary bits
    aux_bits: u8,
    /// Pin direction
    pindir: u8,
}

impl Ftdi {
    /// Open an FTDI device with the given configuration
    pub fn open(config: &FtdiConfig) -> Result<Self> {
        log::info!(
            "Opening FTDI {} channel {}",
            config.device_type.name(),
            config.interface.letter()
        );

        // Set interface
        let interface = match config.interface {
            FtdiInterface::A => Interface::A,
            FtdiInterface::B => Interface::B,
            FtdiInterface::C => Interface::C,
            FtdiInterface::D => Interface::D,
        };

        // Open by VID/PID
        let vid = config.device_type.vendor_id();
        let pid = config.device_type.product_id();

        log::debug!("Looking for FTDI device VID={:04X} PID={:04X}", vid, pid);

        let mut device = find_by_vid_pid(vid, pid)
            .interface(interface)
            .open()
            .map_err(|e| FtdiError::OpenFailed(format!("{}", e)))?;

        log::debug!("Opened FTDI device VID={:04X} PID={:04X}", vid, pid);

        // Reset USB device
        device
            .usb_reset()
            .map_err(|e| FtdiError::ConfigFailed(format!("USB reset failed: {}", e)))?;

        // Set latency timer (2ms for best performance)
        device
            .set_latency_timer(2)
            .map_err(|e| FtdiError::ConfigFailed(format!("Set latency timer failed: {}", e)))?;

        // Set MPSSE bitbang mode
        device
            .set_bitmode(0x00, BitMode::Mpsse)
            .map_err(|e| FtdiError::ConfigFailed(format!("Set MPSSE mode failed: {}", e)))?;

        let mut ftdi = Ftdi {
            device,
            cs_bits: config.cs_bits,
            aux_bits: config.aux_bits,
            pindir: config.pindir,
        };

        // Initialize MPSSE
        ftdi.init_mpsse(config)?;

        log::info!(
            "FTDI configured for SPI at {:.2} MHz",
            config.spi_clock_mhz()
        );

        Ok(ftdi)
    }

    /// Open the first available FTDI device
    pub fn open_first() -> Result<Self> {
        Self::open(&FtdiConfig::default())
    }

    /// Open a specific device type
    pub fn open_device(device_type: FtdiDeviceType) -> Result<Self> {
        Self::open(&FtdiConfig::for_device(device_type))
    }

    /// Initialize the MPSSE engine
    fn init_mpsse(&mut self, config: &FtdiConfig) -> Result<()> {
        let mut buf = Vec::with_capacity(32);

        // Disable divide-by-5 prescaler for 60 MHz base clock (H devices)
        if config.device_type.is_high_speed() {
            log::debug!("Disabling divide-by-5 prescaler for 60 MHz clock");
            buf.push(DIS_DIV_5);
        }

        // Set clock divisor
        // Divisor value for MPSSE is (divisor / 2 - 1)
        let divisor_val = config.divisor / 2 - 1;
        log::debug!(
            "Setting clock divisor to {} (SPI clock: {:.2} MHz)",
            config.divisor,
            config.spi_clock_mhz()
        );
        buf.push(TCK_DIVISOR);
        buf.push((divisor_val & 0xFF) as u8);
        buf.push(((divisor_val >> 8) & 0xFF) as u8);

        // Disconnect loopback
        log::debug!("Disabling loopback");
        buf.push(LOOPBACK_END);

        // Set initial data bits (low byte)
        log::debug!(
            "Setting data bits: cs_bits=0x{:02X} aux_bits=0x{:02X} pindir=0x{:02X}",
            config.cs_bits,
            config.aux_bits,
            config.pindir
        );
        buf.push(SET_BITS_LOW);
        buf.push(config.cs_bits | config.aux_bits);
        buf.push(config.pindir);

        // Set high byte pins if needed
        if config.pindir_high != 0 {
            log::debug!(
                "Setting high byte pins: aux_bits_high=0x{:02X} pindir_high=0x{:02X}",
                config.aux_bits_high,
                config.pindir_high
            );
            buf.push(SET_BITS_HIGH);
            buf.push(config.aux_bits_high);
            buf.push(config.pindir_high);
        }

        self.send(&buf)?;

        Ok(())
    }

    /// Send data to the FTDI device
    fn send(&mut self, data: &[u8]) -> Result<()> {
        self.device
            .write_all(data)
            .map_err(|e| FtdiError::TransferFailed(format!("Write failed: {}", e)))?;
        log::trace!("Sent {} bytes", data.len());
        Ok(())
    }

    /// Receive data from the FTDI device
    fn recv(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        let mut total = 0;

        while total < len {
            match self.device.read(&mut buf[total..]) {
                Ok(0) => {
                    // No data available, wait a bit
                    std::thread::sleep(Duration::from_micros(100));
                }
                Ok(n) => {
                    total += n;
                }
                Err(e) => {
                    return Err(FtdiError::TransferFailed(format!("Read failed: {}", e)));
                }
            }
        }

        log::trace!("Received {} bytes", total);
        Ok(buf)
    }

    /// Perform an SPI transfer
    fn spi_transfer(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        let writecnt = write_data.len();
        let readcnt = read_len;

        // Validate lengths
        if writecnt > 65536 || readcnt > 65536 {
            return Err(FtdiError::TransferFailed(
                "Transfer length exceeds 64KB limit".to_string(),
            ));
        }

        // Build command buffer
        let mut buf = Vec::with_capacity(FTDI_HW_BUFFER_SIZE);

        // Assert CS
        buf.push(SET_BITS_LOW);
        buf.push(self.aux_bits);
        buf.push(self.pindir);

        // Write command (WREN, opcode, address, data)
        if writecnt > 0 {
            buf.push(MPSSE_DO_WRITE | MPSSE_WRITE_NEG);
            buf.push(((writecnt - 1) & 0xFF) as u8);
            buf.push((((writecnt - 1) >> 8) & 0xFF) as u8);
            buf.extend_from_slice(write_data);
        }

        // Read command
        if readcnt > 0 {
            buf.push(MPSSE_DO_READ);
            buf.push(((readcnt - 1) & 0xFF) as u8);
            buf.push((((readcnt - 1) >> 8) & 0xFF) as u8);
        }

        // Deassert CS
        buf.push(SET_BITS_LOW);
        buf.push(self.cs_bits | self.aux_bits);
        buf.push(self.pindir);

        // Send immediate to flush
        buf.push(SEND_IMMEDIATE);

        // Send command
        self.send(&buf)?;

        // Read response if needed
        if readcnt > 0 {
            self.recv(readcnt)
        } else {
            Ok(Vec::new())
        }
    }

    /// Release I/O pins (set all as inputs)
    fn release_pins(&mut self) -> Result<()> {
        let buf = [SET_BITS_LOW, 0x00, 0x00];
        self.send(&buf)
    }

    /// List available FTDI devices
    pub fn list_devices() -> Result<Vec<FtdiDeviceInfo>> {
        let mut devices = Vec::new();

        for dev in nusb::list_devices()? {
            let vid = dev.vendor_id();
            let pid = dev.product_id();

            if let Some(info) = get_device_info(vid, pid) {
                devices.push(FtdiDeviceInfo {
                    bus: dev.bus_number(),
                    address: dev.device_address(),
                    vendor_id: vid,
                    product_id: pid,
                    vendor_name: info.vendor_name,
                    device_name: info.device_name,
                    serial: None, // Would need to open device to get this
                });
            }
        }

        Ok(devices)
    }
}

impl Drop for Ftdi {
    fn drop(&mut self) {
        // Release I/O pins on close
        if let Err(e) = self.release_pins() {
            log::warn!("Failed to release pins on close: {}", e);
        }
        // Device will be closed automatically when dropped
    }
}

impl SpiMaster for Ftdi {
    fn features(&self) -> SpiFeatures {
        // FTDI MPSSE supports 4-byte addressing (software handled)
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        // FTDI can handle 64KB per transfer, but we chunk for responsiveness
        64 * 1024
    }

    fn max_write_len(&self) -> usize {
        // Page program is typically 256 bytes
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

        // Perform the transfer
        let read_len = cmd.read_buf.len();
        let result = self
            .spi_transfer(&write_data, read_len)
            .map_err(|_e| CoreError::ProgrammerError)?;

        // Copy read data back
        cmd.read_buf.copy_from_slice(&result);

        Ok(())
    }

    fn delay_us(&mut self, us: u32) {
        // For FTDI, we just use a regular sleep
        // The MPSSE doesn't have built-in delay commands for arbitrary times
        std::thread::sleep(Duration::from_micros(us as u64));
    }
}

/// Information about a connected FTDI device
#[derive(Debug, Clone)]
pub struct FtdiDeviceInfo {
    /// USB bus number
    pub bus: u8,
    /// USB device address
    pub address: u8,
    /// Vendor ID
    pub vendor_id: u16,
    /// Product ID
    pub product_id: u16,
    /// Vendor name
    pub vendor_name: &'static str,
    /// Device name
    pub device_name: &'static str,
    /// Serial number (if available)
    pub serial: Option<String>,
}

impl std::fmt::Display for FtdiDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {} at bus {} address {} ({:04X}:{:04X})",
            self.vendor_name,
            self.device_name,
            self.bus,
            self.address,
            self.vendor_id,
            self.product_id
        )
    }
}

/// Parse programmer options from a string
///
/// Format: "type=<type>,port=<A|B|C|D>,divisor=<N>,serial=<serial>,gpiol0=<H|L|C>"
pub fn parse_options(options: &[(&str, &str)]) -> Result<FtdiConfig> {
    let mut config = FtdiConfig::default();

    for (key, value) in options {
        match *key {
            "type" => {
                config.device_type = FtdiDeviceType::parse(value).ok_or_else(|| {
                    FtdiError::InvalidDeviceType(format!(
                        "Unknown device type '{}'. Valid types: 2232h, 4232h, 232h, 4233h, \
                         jtagkey, tumpa, tumpalite, picotap, busblaster, flyswatter, \
                         arm-usb-ocd, arm-usb-tiny, arm-usb-ocd-h, arm-usb-tiny-h, \
                         google-servo, google-servo-v2, kt-link",
                        value
                    ))
                })?;
                // Update defaults for new device type
                config.cs_bits = config.device_type.default_cs_bits();
                config.aux_bits = config.device_type.default_aux_bits();
                config.pindir = config.device_type.default_pindir();
                config.pindir_high = config.device_type.default_pindir_high();
                config.divisor = config.device_type.default_divisor();
            }
            "port" | "channel" => {
                if value.len() != 1 {
                    return Err(FtdiError::InvalidChannel(format!(
                        "Invalid channel '{}': must be A, B, C, or D",
                        value
                    )));
                }
                let interface = FtdiInterface::from_char(value.chars().next().unwrap())
                    .ok_or_else(|| {
                        FtdiError::InvalidChannel(format!(
                            "Invalid channel '{}': must be A, B, C, or D",
                            value
                        ))
                    })?;
                config = config.interface(interface)?;
            }
            "divisor" => {
                let divisor: u16 = value.parse().map_err(|_| {
                    FtdiError::InvalidParameter(format!("Invalid divisor '{}'", value))
                })?;
                config = config.divisor(divisor)?;
            }
            "serial" => {
                config.serial = Some(value.to_string());
            }
            "description" => {
                config.description = Some(value.to_string());
            }
            key if key.starts_with("gpiol") => {
                let pin: u8 = key[5..].parse().map_err(|_| {
                    FtdiError::InvalidParameter(format!("Invalid GPIOL pin '{}'", key))
                })?;
                if value.len() != 1 {
                    return Err(FtdiError::InvalidParameter(format!(
                        "Invalid GPIOL mode '{}': must be H, L, C, or I",
                        value
                    )));
                }
                config = config.gpiol(pin, value.chars().next().unwrap())?;
            }
            _ => {
                log::warn!("Unknown FTDI option: {}={}", key, value);
            }
        }
    }

    Ok(config)
}

//! Dediprog device implementation
//!
//! This module provides the main `Dediprog` struct that implements USB
//! communication with Dediprog SF100/SF200/SF600/SF700 programmers.

use std::time::Duration;

use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::{Endpoint, Interface, MaybeFuture};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::default_execute_with_vec;
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::SpiCommand;

use crate::error::{DediprogError, Result};
use crate::protocol::*;

/// Configuration options for opening a Dediprog device
#[derive(Debug, Clone)]
pub struct DediprogConfig {
    /// Device index (when multiple devices are connected)
    pub device_index: usize,
    /// Device ID to search for (e.g., "SF123456")
    pub device_id: Option<String>,
    /// Target flash (1 or 2 for dual-chip programmers)
    pub target: Target,
    /// SPI speed index (0=24MHz, 1=12MHz, etc.)
    pub spi_speed_index: usize,
    /// Voltage in millivolts (0, 1800, 2500, 3500)
    pub voltage_mv: u16,
    /// I/O mode (Single, Dual, Quad)
    pub io_mode: DpIoMode,
}

impl Default for DediprogConfig {
    fn default() -> Self {
        Self {
            device_index: 0,
            device_id: None,
            target: Target::ApplicationFlash1,
            spi_speed_index: DEFAULT_SPI_SPEED_INDEX,
            voltage_mv: DEFAULT_VOLTAGE_MV,
            io_mode: DpIoMode::Single,
        }
    }
}

/// Parse options from key=value pairs
pub fn parse_options(options: &[(&str, &str)]) -> Result<DediprogConfig> {
    let mut config = DediprogConfig::default();

    for (key, value) in options {
        match *key {
            "device" | "index" => {
                config.device_index = value
                    .parse()
                    .map_err(|_| DediprogError::InvalidParameter(format!("device: {}", value)))?;
            }
            "id" => {
                config.device_id = Some(value.to_string());
            }
            "target" => {
                let t: u8 = value
                    .parse()
                    .map_err(|_| DediprogError::InvalidParameter(format!("target: {}", value)))?;
                config.target = Target::from_value(t)
                    .ok_or_else(|| DediprogError::InvalidParameter(format!("target: {}", value)))?;
            }
            "spispeed" => {
                config.spi_speed_index = parse_spi_speed(value).ok_or_else(|| {
                    DediprogError::InvalidParameter(format!("spispeed: {}", value))
                })?;
            }
            "voltage" => {
                config.voltage_mv = parse_voltage(value).ok_or_else(|| {
                    DediprogError::InvalidParameter(format!("voltage: {}", value))
                })?;
            }
            "iomode" => match value.to_lowercase().as_str() {
                "single" | "1" => config.io_mode = DpIoMode::Single,
                "dual" | "2" => config.io_mode = DpIoMode::DualIo,
                "quad" | "4" => config.io_mode = DpIoMode::QuadIo,
                _ => {
                    return Err(DediprogError::InvalidParameter(format!(
                        "iomode: {}",
                        value
                    )));
                }
            },
            _ => {
                return Err(DediprogError::InvalidParameter(format!(
                    "unknown option: {}",
                    key
                )));
            }
        }
    }

    Ok(config)
}

/// Dediprog USB programmer
///
/// Supports SF100, SF200, SF600, SF600PG2, and SF700 programmers.
pub struct Dediprog {
    /// USB interface
    interface: Interface,
    /// Bulk IN endpoint
    in_endpoint: u8,
    /// Bulk OUT endpoint
    out_endpoint: u8,
    /// Device type
    device_type: DeviceType,
    /// Firmware version (encoded as major<<16 | minor<<8 | patch)
    firmware_version: u32,
    /// Device string (e.g., "SF600 V:7.2.0")
    device_string: String,
    /// Protocol version
    protocol: Protocol,
    /// Current I/O mode
    io_mode: DpIoMode,
    /// Configured maximum I/O mode
    max_io_mode: DpIoMode,
}

impl Dediprog {
    /// Open the first available Dediprog device
    pub fn open() -> Result<Self> {
        Self::open_with_config(DediprogConfig::default())
    }

    /// Open a Dediprog device with the specified configuration
    pub fn open_with_config(config: DediprogConfig) -> Result<Self> {
        // Find matching devices
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| DediprogError::OpenFailed(e.to_string()))?
            .filter(|d| {
                d.vendor_id() == DEDIPROG_USB_VENDOR && d.product_id() == DEDIPROG_USB_PRODUCT
            })
            .collect();

        if devices.is_empty() {
            return Err(DediprogError::DeviceNotFound);
        }

        // If searching by ID, try each device
        if let Some(ref target_id) = config.device_id {
            for device_info in &devices {
                match Self::try_open_device(device_info, &config) {
                    Ok(mut dediprog) => {
                        // Read device ID and check
                        if let Ok(id) = dediprog.read_device_id() {
                            let id_str = format!("SF{:06}", id);
                            if id_str.contains(target_id) || target_id.contains(&id_str) {
                                log::info!("Found Dediprog with ID {}", id_str);
                                return Ok(dediprog);
                            }
                        }
                        // Close and try next
                        drop(dediprog);
                    }
                    Err(_) => continue,
                }
            }
            return Err(DediprogError::DeviceNotFound);
        }

        // Open by index
        let device_info = devices
            .get(config.device_index)
            .ok_or(DediprogError::DeviceNotFound)?;

        Self::try_open_device(device_info, &config)
    }

    /// Try to open a specific USB device
    fn try_open_device(device_info: &nusb::DeviceInfo, config: &DediprogConfig) -> Result<Self> {
        log::info!(
            "Opening Dediprog at bus {} address {}",
            device_info.busnum(),
            device_info.device_address()
        );

        let device = device_info
            .open()
            .wait()
            .map_err(|e| DediprogError::OpenFailed(e.to_string()))?;

        // Claim interface 0
        let interface = device
            .claim_interface(0)
            .wait()
            .map_err(|e| DediprogError::ClaimFailed(e.to_string()))?;

        let mut dediprog = Self {
            interface,
            in_endpoint: BULK_IN_EP,
            out_endpoint: BULK_OUT_EP_SF100, // Will be updated based on device type
            device_type: DeviceType::Unknown,
            firmware_version: 0,
            device_string: String::new(),
            protocol: Protocol::Unknown,
            io_mode: DpIoMode::Single,
            max_io_mode: config.io_mode,
        };

        // Try to read device string (may need set_voltage first for old devices)
        if dediprog.read_device_string().is_err() {
            // Try set_voltage for old firmware and retry
            dediprog.set_voltage_old()?;
            dediprog.read_device_string()?;
        }

        // Update endpoints based on device type
        if dediprog.device_type.is_sf600_class() {
            dediprog.out_endpoint = BULK_OUT_EP_SF600;
        }

        // Determine protocol version
        dediprog.protocol =
            Protocol::from_device_firmware(dediprog.device_type, dediprog.firmware_version);

        if dediprog.protocol == Protocol::Unknown {
            return Err(DediprogError::FirmwareError(
                "Unable to determine protocol version".to_string(),
            ));
        }

        log::info!(
            "Dediprog {}: firmware {:X}.{:X}.{:X}, protocol {:?}",
            dediprog.device_type,
            (dediprog.firmware_version >> 16) & 0xFF,
            (dediprog.firmware_version >> 8) & 0xFF,
            dediprog.firmware_version & 0xFF,
            dediprog.protocol
        );

        // Initialize the device
        dediprog.set_leds(Led::All)?;

        // Set target, speed, and voltage
        dediprog.set_target(config.target)?;
        dediprog.set_spi_speed(config.spi_speed_index)?;
        dediprog.set_voltage(config.voltage_mv)?;

        // Leave standalone mode if SF600
        if dediprog.device_type == DeviceType::SF600 {
            dediprog.leave_standalone_mode()?;
        }

        // Determine multi-I/O support
        if dediprog.device_type.is_sf600_class() && dediprog.protocol >= Protocol::V2 {
            dediprog.max_io_mode = config.io_mode;
        } else {
            dediprog.max_io_mode = DpIoMode::Single;
        }

        dediprog.set_leds(Led::None)?;

        Ok(dediprog)
    }

    /// List all connected Dediprog devices
    pub fn list_devices() -> Result<Vec<DediprogDeviceInfo>> {
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| DediprogError::OpenFailed(e.to_string()))?
            .filter(|d| {
                d.vendor_id() == DEDIPROG_USB_VENDOR && d.product_id() == DEDIPROG_USB_PRODUCT
            })
            .map(|d| DediprogDeviceInfo {
                bus: d.busnum(),
                address: d.device_address(),
            })
            .collect();

        Ok(devices)
    }

    /// Read the device string and parse device type/firmware
    fn read_device_string(&mut self) -> Result<()> {
        let mut buf = [0u8; 33];
        let len = self.control_read(Command::ReadProgInfo, 0, 0, &mut buf)?;

        if len < 16 {
            return Err(DediprogError::InvalidResponse(
                "Device string too short".to_string(),
            ));
        }

        self.device_string = String::from_utf8_lossy(&buf[..len])
            .trim_end_matches('\0')
            .to_string();

        log::debug!("Device string: {}", self.device_string);

        // Parse device type
        self.device_type = DeviceType::from_device_string(&self.device_string);
        if self.device_type == DeviceType::Unknown {
            return Err(DediprogError::UnknownDevice(self.device_string.clone()));
        }

        // Parse firmware version (format: "SFXXX V:X.X.X")
        if let Some(version_str) = self.device_string.split("V:").nth(1) {
            let parts: Vec<&str> = version_str.split('.').collect();
            if parts.len() >= 3 {
                let major: u32 = parts[0].parse().unwrap_or(0);
                let minor: u32 = parts[1].parse().unwrap_or(0);
                let patch: u32 = parts[2]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0);
                self.firmware_version = firmware_version(major, minor, patch);
            }
        }

        // Verify firmware version is in expected range
        let major = (self.firmware_version >> 16) & 0xFF;
        match self.device_type {
            DeviceType::SF600PG2 if major > 1 => {
                return Err(DediprogError::FirmwareError(format!(
                    "Unexpected firmware version for SF600PG2: {}",
                    self.device_string
                )));
            }
            DeviceType::SF700 if major != 4 => {
                return Err(DediprogError::FirmwareError(format!(
                    "Unexpected firmware version for SF700: {}",
                    self.device_string
                )));
            }
            DeviceType::SF100 | DeviceType::SF200 | DeviceType::SF600
                if !(2..=7).contains(&major) =>
            {
                return Err(DediprogError::FirmwareError(format!(
                    "Unexpected firmware version: {}",
                    self.device_string
                )));
            }
            _ => {}
        }

        Ok(())
    }

    /// Read the device ID (serial number from sticker)
    fn read_device_id(&mut self) -> Result<u32> {
        if self.device_type >= DeviceType::SF600PG2 {
            // Newer protocol for SF600PG2/SF700
            let out = [0x00, 0x00, 0x00, 0x02, 0x00, 0x00];
            self.control_write_raw(0x71, 0, 0, &out)?;

            let mut buf = [0u8; 512];
            let len = self.bulk_read(&mut buf)?;
            if len >= 3 {
                return Ok((buf[2] as u32) << 16 | (buf[1] as u32) << 8 | (buf[0] as u32));
            }
        } else if self.device_type.is_sf600_class() {
            // SF600 uses CMD_READ_EEPROM
            let mut buf = [0u8; 16];
            let len = self.control_read(Command::ReadEeprom, 0, 0, &mut buf)?;
            if len >= 3 {
                return Ok((buf[0] as u32) << 16 | (buf[1] as u32) << 8 | (buf[2] as u32));
            }
        } else {
            // SF100/SF200 use a different request
            let mut buf = [0u8; 3];
            let len = self.control_read_raw(REQTYPE_OTHER_IN, 0x07, 0, 0xEF00, &mut buf)?;
            if len >= 3 {
                return Ok((buf[0] as u32) << 16 | (buf[1] as u32) << 8 | (buf[2] as u32));
            }
        }

        Err(DediprogError::InvalidResponse(
            "Failed to read device ID".to_string(),
        ))
    }

    /// Set voltage for old firmware (< 6.0.0)
    fn set_voltage_old(&mut self) -> Result<()> {
        let mut buf = [0u8; 1];
        let ret =
            self.control_read_raw(REQTYPE_OTHER_IN, Command::SetVoltage as u8, 0, 0, &mut buf)?;
        if ret != 1 || buf[0] != 0x6f {
            return Err(DediprogError::InvalidResponse(
                "Unexpected response to set_voltage".to_string(),
            ));
        }
        Ok(())
    }

    /// Set the LED state
    fn set_leds(&mut self, led: Led) -> Result<()> {
        if self.protocol >= Protocol::V2 {
            // New protocol: value contains LED state
            let leds = ((led as u8) ^ 7) as u16;
            self.control_write(Command::SetIoLed, leds << 8, 0, &[])?;
        } else {
            // Old protocol: index contains LED state
            let leds = if self.firmware_version < firmware_version(5, 0, 0) {
                // Very old firmware has different LED mapping
                let l = led as u8;
                ((l & 4) >> 2) | ((l & 1) << 2)
            } else {
                led as u8
            };
            let target_leds = leds ^ 7;
            self.control_write(Command::SetIoLed, 0x9, target_leds as u16, &[])?;
        }
        Ok(())
    }

    /// Set the target flash
    fn set_target(&mut self, target: Target) -> Result<()> {
        self.control_write(Command::SetTarget, target as u16, 0, &[])?;
        Ok(())
    }

    /// Set the SPI clock speed
    fn set_spi_speed(&mut self, speed_index: usize) -> Result<()> {
        if self.device_type < DeviceType::SF600PG2
            && self.firmware_version < firmware_version(5, 0, 0)
        {
            log::warn!("Skipping SPI speed setting for old firmware");
            return Ok(());
        }

        let speed = SPI_SPEEDS.get(speed_index).ok_or_else(|| {
            DediprogError::InvalidParameter("Invalid SPI speed index".to_string())
        })?;

        log::debug!("Setting SPI speed to {}", speed.name);
        self.control_write(Command::SetSpiClk, speed.value as u16, 0, &[])?;
        Ok(())
    }

    /// Set the SPI voltage
    fn set_voltage(&mut self, millivolt: u16) -> Result<()> {
        let selector = voltage_selector(millivolt)
            .ok_or_else(|| DediprogError::InvalidParameter(format!("voltage: {}", millivolt)))?;

        log::debug!(
            "Setting SPI voltage to {}.{:03}V",
            millivolt / 1000,
            millivolt % 1000
        );

        if selector == 0 {
            // Delay before turning off voltage
            std::thread::sleep(Duration::from_millis(200));
        }

        self.control_write(Command::SetVcc, selector, 0, &[])?;

        if selector != 0 {
            // Delay after turning on voltage
            std::thread::sleep(Duration::from_millis(200));
        }

        Ok(())
    }

    /// Leave standalone mode (SF600 only)
    fn leave_standalone_mode(&mut self) -> Result<()> {
        if self.device_type != DeviceType::SF600 {
            return Ok(());
        }

        log::debug!("Leaving standalone mode");
        self.control_write(Command::SetStandalone, StandaloneMode::Leave as u16, 0, &[])?;
        Ok(())
    }

    /// Set the I/O mode for multi-I/O operations
    fn set_io_mode(&mut self, mode: DpIoMode) -> Result<()> {
        if !self.device_type.is_sf600_class() {
            return Ok(());
        }

        if self.io_mode == mode {
            return Ok(());
        }

        log::trace!("Setting I/O mode to {:?}", mode);
        self.control_write(Command::IoMode, mode as u16, 0, &[])?;
        self.io_mode = mode;
        Ok(())
    }

    /// USB control read
    fn control_read(
        &mut self,
        cmd: Command,
        value: u16,
        index: u16,
        buf: &mut [u8],
    ) -> Result<usize> {
        self.control_read_raw(REQTYPE_EP_IN, cmd as u8, value, index, buf)
    }

    /// USB control read (raw)
    fn control_read_raw(
        &mut self,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        buf: &mut [u8],
    ) -> Result<usize> {
        let recipient = if request_type & 0x03 == 0x02 {
            nusb::transfer::Recipient::Endpoint
        } else {
            nusb::transfer::Recipient::Other
        };

        let data = self
            .interface
            .control_in(
                nusb::transfer::ControlIn {
                    control_type: nusb::transfer::ControlType::Vendor,
                    recipient,
                    request,
                    value,
                    index,
                    length: buf.len() as u16,
                },
                Duration::from_secs(5),
            )
            .wait()
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        Ok(len)
    }

    /// USB control write
    fn control_write(&mut self, cmd: Command, value: u16, index: u16, data: &[u8]) -> Result<()> {
        self.control_write_raw(cmd as u8, value, index, data)
    }

    /// USB control write (raw)
    fn control_write_raw(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<()> {
        self.interface
            .control_out(
                nusb::transfer::ControlOut {
                    control_type: nusb::transfer::ControlType::Vendor,
                    recipient: nusb::transfer::Recipient::Endpoint,
                    request,
                    value,
                    index,
                    data,
                },
                Duration::from_secs(5),
            )
            .wait()
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        Ok(())
    }

    /// Bulk read
    fn bulk_read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut in_ep: Endpoint<Bulk, In> = self
            .interface
            .endpoint(self.in_endpoint)
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        let max_packet_size = in_ep.max_packet_size();
        let request_len = buf.len().div_ceil(max_packet_size) * max_packet_size;
        let mut in_buf = Buffer::new(request_len);
        in_buf.set_requested_len(request_len);

        let completion = in_ep.transfer_blocking(in_buf, Duration::from_secs(5));
        let data = completion
            .into_result()
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        Ok(len)
    }

    /// Bulk write
    #[allow(dead_code)]
    fn bulk_write(&mut self, data: &[u8]) -> Result<()> {
        let mut out_ep: Endpoint<Bulk, Out> = self
            .interface
            .endpoint(self.out_endpoint)
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        let mut out_buf = Buffer::new(data.len());
        out_buf.extend_from_slice(data);

        let completion = out_ep.transfer_blocking(out_buf, Duration::from_secs(5));
        completion
            .into_result()
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        Ok(())
    }

    /// Send a transceive command (generic SPI command)
    fn spi_transceive(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        // Set to single I/O mode for generic commands
        self.set_io_mode(DpIoMode::Single)?;

        // Build command
        let (value, index) = if self.protocol >= Protocol::V2 {
            // New protocol: value indicates if we need a read
            (if read_len > 0 { 0x1 } else { 0x0 }, 0)
        } else {
            // Old protocol: index indicates if we need a read
            (0, if read_len > 0 { 0x1 } else { 0x0 })
        };

        // Send command
        self.control_write(Command::Transceive, value, index, write_data)?;

        if read_len == 0 {
            return Ok(Vec::new());
        }

        // Read response
        let mut buf = vec![0u8; read_len];
        let mut total_read = 0;

        while total_read < read_len {
            let to_read = (read_len - total_read).min(64);

            let data = self
                .interface
                .control_in(
                    nusb::transfer::ControlIn {
                        control_type: nusb::transfer::ControlType::Vendor,
                        recipient: nusb::transfer::Recipient::Endpoint,
                        request: Command::Transceive as u8,
                        value: 0,
                        index: 0,
                        length: to_read as u16,
                    },
                    Duration::from_secs(5),
                )
                .wait()
                .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

            let len = data.len().min(to_read);
            buf[total_read..total_read + len].copy_from_slice(&data[..len]);
            total_read += len;

            if data.len() < to_read {
                break;
            }
        }

        Ok(buf)
    }

    /// Get the device type
    pub fn device_type(&self) -> DeviceType {
        self.device_type
    }

    /// Get the device string
    pub fn device_string(&self) -> &str {
        &self.device_string
    }

    /// Get the firmware version (encoded)
    pub fn firmware_version(&self) -> u32 {
        self.firmware_version
    }

    /// Get the protocol version
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }
}

impl Drop for Dediprog {
    fn drop(&mut self) {
        // Reset I/O mode
        let _ = self.set_io_mode(DpIoMode::Single);
        // Turn off voltage
        let _ = self.set_voltage(0);
    }
}

impl SpiMaster for Dediprog {
    fn features(&self) -> SpiFeatures {
        let mut features = SpiFeatures::empty();

        // 4BA support depends on protocol version
        if self.protocol >= Protocol::V2 {
            features |= SpiFeatures::FOUR_BYTE_ADDR;
        }

        // Multi-I/O support for SF600 class with protocol V2+
        if self.device_type.is_sf600_class() && self.protocol >= Protocol::V2 {
            match self.max_io_mode {
                DpIoMode::DualOut | DpIoMode::DualIo => {
                    features |= SpiFeatures::DUAL_IN;
                    // V2 has issues with DUAL_IO, V3 works
                    if self.protocol >= Protocol::V3 {
                        features |= SpiFeatures::DUAL_IO;
                    }
                }
                DpIoMode::QuadOut | DpIoMode::QuadIo | DpIoMode::Qpi => {
                    features |= SpiFeatures::DUAL_IN | SpiFeatures::QUAD_IN;
                    if self.protocol >= Protocol::V3 {
                        features |= SpiFeatures::DUAL_IO | SpiFeatures::QUAD_IO;
                    }
                }
                _ => {}
            }
        }

        // Some protocol versions have restrictions on 4BA modes
        if self.protocol == Protocol::V1
            && (self.device_type == DeviceType::SF100 || self.device_type.is_sf600_class())
        {
            // V1 on SF100 or SF600 class doesn't have 4BA mode restrictions
        } else if self.protocol < Protocol::V2 {
            features |= SpiFeatures::NO_4BA_MODES;
        }

        features
    }

    fn max_read_len(&self) -> usize {
        // Maximum data read in a single transceive command
        16
    }

    fn max_write_len(&self) -> usize {
        // Maximum data write in a single transceive command (minus 5 for cmd/addr)
        16 - 5
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        default_execute_with_vec(cmd, self.features(), |write_data, read_len| {
            self.spi_transceive(write_data, read_len)
                .map_err(|_| CoreError::ProgrammerError)
        })
    }

    fn delay_us(&mut self, us: u32) {
        std::thread::sleep(Duration::from_micros(us as u64));
    }
}

/// Information about a connected Dediprog device
#[derive(Debug, Clone)]
pub struct DediprogDeviceInfo {
    /// USB bus number
    pub bus: u8,
    /// USB device address
    pub address: u8,
}

impl std::fmt::Display for DediprogDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Dediprog at bus {} address {}", self.bus, self.address)
    }
}

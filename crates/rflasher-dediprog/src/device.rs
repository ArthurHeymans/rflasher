//! Dediprog device implementation
//!
//! This module provides the main `Dediprog` struct that implements USB
//! communication with Dediprog SF100/SF200/SF600/SF700 programmers.

use std::time::Duration;

use maybe_async::maybe_async;
use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::{Endpoint, Interface, MaybeFuture};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};

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
    /// Current I/O mode (None = unknown/uninitialized, matching flashprog's -1 sentinel)
    io_mode: Option<DpIoMode>,
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
            io_mode: None,
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
            // Always query the id twice as the endpoint can lock up
            // in mysterious ways otherwise (matches flashprog behavior).
            let out = [0x00, 0x00, 0x00, 0x02, 0x00, 0x00];
            let mut buf = [0u8; 512];
            let mut len = 0;
            for _ in 0..2 {
                self.control_write_raw(0x71, 0, 0, &out)?;
                len = self.bulk_read(&mut buf)?;
            }
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

        if self.io_mode == Some(mode) {
            return Ok(());
        }

        log::trace!("Setting I/O mode to {:?}", mode);
        self.control_write(Command::IoMode, mode as u16, 0, &[])?;
        self.io_mode = Some(mode);
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

    // -----------------------------------------------------------------------
    // Bulk read/write command packet preparation (matches flashprog exactly)
    // -----------------------------------------------------------------------

    /// Build the common 5-byte header shared by all protocol versions.
    /// Returns 5 on success, or an error if count exceeds MAX_BLOCK_COUNT.
    fn prepare_rw_cmd_common(
        cmd_buf: &mut [u8; MAX_CMD_SIZE],
        dp_spi_cmd: u8,
        count: u32,
    ) -> Result<usize> {
        if count > MAX_BLOCK_COUNT as u32 {
            return Err(DediprogError::InvalidParameter(format!(
                "Unsupported transfer length of {} blocks",
                count
            )));
        }
        cmd_buf[0] = (count & 0xff) as u8;
        cmd_buf[1] = ((count >> 8) & 0xff) as u8;
        cmd_buf[2] = 0; // RFU
        cmd_buf[3] = dp_spi_cmd; // Read/Write Mode
        cmd_buf[4] = 0; // "Opcode"
        Ok(5)
    }

    /// Protocol V1 command packet: address in wValue/wIndex, 5-byte packet.
    fn prepare_rw_cmd_v1(
        cmd_buf: &mut [u8; MAX_CMD_SIZE],
        dp_spi_cmd: u8,
        start: u32,
        block_count: u32,
    ) -> Result<(usize, u16, u16)> {
        let cmd_len = Self::prepare_rw_cmd_common(cmd_buf, dp_spi_cmd, block_count)?;

        if start >> 24 != 0 {
            return Err(DediprogError::InvalidParameter(
                "Can't handle 4-byte address with dediprog V1 protocol".to_string(),
            ));
        }

        let value = (start & 0xffff) as u16;
        let idx = ((start >> 16) & 0xff) as u16;
        Ok((cmd_len, value, idx))
    }

    /// Protocol V2 command packet: address in bytes 6-9, 10-byte packet.
    fn prepare_rw_cmd_v2(
        cmd_buf: &mut [u8; MAX_CMD_SIZE],
        is_read: bool,
        dp_spi_cmd: u8,
        start: u32,
        block_count: u32,
        read_op: Option<&BulkReadOp>,
    ) -> Result<(usize, u16, u16)> {
        Self::prepare_rw_cmd_common(cmd_buf, dp_spi_cmd, block_count)?;

        if is_read {
            if let Some(op) = read_op {
                if op.native_4ba {
                    cmd_buf[3] = ReadMode::FourByteAddrFast0x0C as u8;
                } else if op.opcode != 0x03 {
                    // JEDEC_READ = 0x03
                    cmd_buf[3] = ReadMode::Fast as u8;
                }

                if op.opcode == 0x13 {
                    // JEDEC_READ_4BA
                    cmd_buf[4] = 0x0C; // JEDEC_FAST_READ_4BA
                } else {
                    cmd_buf[4] = op.opcode;
                }
            }
        } else {
            // Write: check for 4BA write support
            if dp_spi_cmd == WriteMode::PagePgm as u8 {
                if let Some(op) = read_op {
                    if op.native_4ba {
                        cmd_buf[3] = WriteMode::FourByteAddr256BPagePgm0x12 as u8;
                        cmd_buf[4] = 0x12; // JEDEC_BYTE_PROGRAM_4BA
                    }
                }
            }
        }

        cmd_buf[5] = 0; // RFU
        cmd_buf[6] = (start >> 0) as u8;
        cmd_buf[7] = (start >> 8) as u8;
        cmd_buf[8] = (start >> 16) as u8;
        cmd_buf[9] = (start >> 24) as u8;

        Ok((10, 0, 0))
    }

    /// Protocol V3 command packet: fully configurable, 12 bytes for read, 14 for write.
    fn prepare_rw_cmd_v3(
        cmd_buf: &mut [u8; MAX_CMD_SIZE],
        is_read: bool,
        dp_spi_cmd: u8,
        start: u32,
        block_count: u32,
        read_op: Option<&BulkReadOp>,
        in_4ba_mode: bool,
    ) -> Result<(usize, u16, u16)> {
        Self::prepare_rw_cmd_common(cmd_buf, dp_spi_cmd, block_count)?;

        cmd_buf[5] = 0; // RFU
        cmd_buf[6] = (start >> 0) as u8;
        cmd_buf[7] = (start >> 8) as u8;
        cmd_buf[8] = (start >> 16) as u8;
        cmd_buf[9] = (start >> 24) as u8;

        if is_read {
            let op = read_op.ok_or_else(|| {
                DediprogError::InvalidParameter("read_op required for V3 read".to_string())
            })?;
            cmd_buf[3] = ReadMode::Configurable as u8;
            cmd_buf[4] = op.opcode;
            cmd_buf[10] = if op.native_4ba || in_4ba_mode { 4 } else { 3 };
            cmd_buf[11] = op.dummy_cycles / 2;
            Ok((12, 0, 0))
        } else {
            if dp_spi_cmd == WriteMode::PagePgm as u8 {
                if let Some(op) = read_op {
                    if op.native_4ba {
                        cmd_buf[3] = WriteMode::FourByteAddr256BPagePgm as u8;
                        cmd_buf[4] = 0x12; // JEDEC_BYTE_PROGRAM_4BA
                    } else if in_4ba_mode {
                        cmd_buf[3] = WriteMode::FourByteAddr256BPagePgm as u8;
                        cmd_buf[4] = 0x02; // JEDEC_BYTE_PROGRAM
                    }
                }
            }
            // Page size: 256 bytes (little-endian u32)
            // FIXME: This assumes page size of 256 (same as flashprog).
            cmd_buf[10] = 0x00;
            cmd_buf[11] = 0x01;
            cmd_buf[12] = 0x00;
            cmd_buf[13] = 0x00;
            Ok((14, 0, 0))
        }
    }

    /// Prepare a read/write command packet, dispatching to the correct protocol version.
    fn prepare_rw_cmd(
        &self,
        is_read: bool,
        dp_spi_cmd: u8,
        start: u32,
        block_count: u32,
        read_op: Option<&BulkReadOp>,
        in_4ba_mode: bool,
    ) -> Result<([u8; MAX_CMD_SIZE], usize, u16, u16)> {
        let mut cmd_buf = [0u8; MAX_CMD_SIZE];
        let (len, value, idx) = match self.protocol {
            Protocol::V1 => Self::prepare_rw_cmd_v1(&mut cmd_buf, dp_spi_cmd, start, block_count)?,
            Protocol::V2 => Self::prepare_rw_cmd_v2(
                &mut cmd_buf,
                is_read,
                dp_spi_cmd,
                start,
                block_count,
                read_op,
            )?,
            Protocol::V3 => Self::prepare_rw_cmd_v3(
                &mut cmd_buf,
                is_read,
                dp_spi_cmd,
                start,
                block_count,
                read_op,
                in_4ba_mode,
            )?,
            Protocol::Unknown => {
                return Err(DediprogError::FirmwareError(
                    "Unknown protocol version".to_string(),
                ));
            }
        };
        Ok((cmd_buf, len, value, idx))
    }

    // -----------------------------------------------------------------------
    // Bulk SPI read (async ring buffer, matches flashprog)
    // -----------------------------------------------------------------------

    /// Bulk read from SPI flash. Both `start` and `len` must be 512-byte aligned.
    /// Uses async queued bulk IN transfers for maximum throughput.
    ///
    /// If `read_op` is provided (for protocol V2/V3), the firmware will use the
    /// specified opcode, I/O mode, and dummy cycles. Otherwise defaults to
    /// standard read mode with single I/O.
    pub fn spi_bulk_read(
        &mut self,
        buf: &mut [u8],
        start: u32,
        len: u32,
        read_op: Option<&BulkReadOp>,
        in_4ba_mode: bool,
    ) -> Result<()> {
        if len == 0 {
            return Ok(());
        }

        let chunksize: u32 = BULK_CHUNK_SIZE as u32;
        if (start % chunksize) != 0 || (len % chunksize) != 0 {
            return Err(DediprogError::InvalidParameter(format!(
                "Unaligned start=0x{:x}, len=0x{:x}",
                start, len
            )));
        }

        let count = len / chunksize;

        // Set IO mode: use read_op's mode if provided, otherwise single
        if let Some(op) = read_op {
            self.set_io_mode(op.io_mode)?;
        } else {
            self.set_io_mode(DpIoMode::Single)?;
        }

        let (cmd_buf, cmd_len, value, idx) = self.prepare_rw_cmd(
            true,
            ReadMode::Std as u8,
            start,
            count,
            read_op,
            in_4ba_mode,
        )?;

        // Send CMD_READ control transfer to initiate the bulk read
        self.control_write_raw(Command::Read as u8, value, idx, &cmd_buf[..cmd_len])?;

        // Open the IN endpoint for bulk transfers
        let mut in_ep: Endpoint<Bulk, In> = self
            .interface
            .endpoint(self.in_endpoint)
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        let max_packet_size = in_ep.max_packet_size();

        // Pre-submit up to ASYNC_TRANSFERS parallel IN transfers (ring buffer)
        let total_chunks = count as usize;
        let max_pending = ASYNC_TRANSFERS.min(total_chunks);

        let mut queued_idx: usize = 0;
        let mut finished_idx: usize = 0;
        let mut error = false;

        // Calculate the request length rounded up to max_packet_size
        let request_len =
            (BULK_CHUNK_SIZE + max_packet_size - 1) / max_packet_size * max_packet_size;

        // Initial submission: fill the ring buffer
        while queued_idx < total_chunks && (queued_idx - finished_idx) < max_pending {
            let mut transfer_buf = Buffer::new(request_len);
            transfer_buf.set_requested_len(request_len);
            in_ep.submit(transfer_buf);
            queued_idx += 1;
        }

        // Reap completions and submit new transfers
        while finished_idx < total_chunks && !error {
            let completion = match in_ep
                .wait_next_complete(Duration::from_millis(DEFAULT_TIMEOUT_MS + 7000))
            {
                Some(c) => c,
                None => {
                    log::error!(
                        "SPI bulk read timed out at chunk {}/{}",
                        finished_idx,
                        total_chunks
                    );
                    error = true;
                    break;
                }
            };

            match completion.status {
                Ok(()) => {
                    let offset = finished_idx * BULK_CHUNK_SIZE;
                    let copy_len = BULK_CHUNK_SIZE.min(buf.len() - offset);
                    buf[offset..offset + copy_len].copy_from_slice(&completion.buffer[..copy_len]);
                }
                Err(e) => {
                    log::error!("SPI bulk read failed at chunk {}: {:?}", finished_idx, e);
                    error = true;
                    break;
                }
            }
            finished_idx += 1;

            // Submit next transfer if there are more chunks
            if queued_idx < total_chunks {
                let mut transfer_buf = Buffer::new(request_len);
                transfer_buf.set_requested_len(request_len);
                in_ep.submit(transfer_buf);
                queued_idx += 1;
            }
        }

        // Drain any remaining pending transfers on error
        if error {
            in_ep.cancel_all();
            while in_ep.pending() > 0 {
                let _ = in_ep.wait_next_complete(Duration::from_secs(1));
            }
            return Err(DediprogError::TransferFailed(
                "SPI bulk read failed".to_string(),
            ));
        }

        Ok(())
    }

    /// Read SPI flash with automatic alignment handling.
    /// Handles unaligned start/len by using slow (transceive) reads for residue,
    /// and bulk reads for the aligned middle portion.
    pub fn spi_read(
        &mut self,
        buf: &mut [u8],
        start: u32,
        len: u32,
        read_op: Option<&BulkReadOp>,
        in_4ba_mode: bool,
    ) -> Result<()> {
        if len == 0 {
            return Ok(());
        }

        let chunksize: u32 = BULK_CHUNK_SIZE as u32;
        let residue = if start % chunksize != 0 {
            (len).min(chunksize - (start % chunksize))
        } else {
            0
        };

        self.set_leds(Led::Busy)?;

        // Read unaligned head via slow transceive
        if residue > 0 {
            if let Err(e) = self.slow_read(&mut buf[..residue as usize], start, residue, in_4ba_mode) {
                self.set_leds(Led::Error)?;
                return Err(e);
            }
        }

        // Bulk read the aligned middle
        let bulk_len = ((len - residue) / chunksize) * chunksize;
        if bulk_len > 0 {
            let offset = residue as usize;
            if let Err(e) = self.spi_bulk_read(
                &mut buf[offset..offset + bulk_len as usize],
                start + residue,
                bulk_len,
                read_op,
                in_4ba_mode,
            ) {
                self.set_leds(Led::Error)?;
                return Err(e);
            }
        }

        // Read unaligned tail via slow transceive
        let tail_len = len - residue - bulk_len;
        if tail_len > 0 {
            let offset = (residue + bulk_len) as usize;
            if let Err(e) = self.slow_read(
                &mut buf[offset..offset + tail_len as usize],
                start + residue + bulk_len,
                tail_len,
                in_4ba_mode,
            ) {
                self.set_leds(Led::Error)?;
                return Err(e);
            }
        }

        self.set_leds(Led::Pass)?;
        Ok(())
    }

    /// Slow read via individual transceive commands (for unaligned residue).
    /// Uses 4-byte address commands when `in_4ba_mode` is true.
    fn slow_read(&mut self, buf: &mut [u8], start: u32, len: u32, in_4ba_mode: bool) -> Result<()> {
        log::debug!(
            "Slow read for partial block from 0x{:x}, length 0x{:x}",
            start,
            len
        );

        let max_read = 16; // max_transceive_read
        let mut offset: u32 = 0;
        while offset < len {
            let chunk = ((len - offset) as usize).min(max_read);
            let addr = start + offset;

            let result = if in_4ba_mode || addr > 0xFFFFFF {
                // Use 4-byte address read (0x13 = JEDEC_READ_4BA)
                let cmd_data = [
                    0x13,
                    (addr >> 24) as u8,
                    (addr >> 16) as u8,
                    (addr >> 8) as u8,
                    addr as u8,
                ];
                self.spi_transceive(&cmd_data, chunk)?
            } else {
                // Use standard 3-byte address read (0x03 = JEDEC_READ)
                let cmd_data = [
                    0x03,
                    (addr >> 16) as u8,
                    (addr >> 8) as u8,
                    addr as u8,
                ];
                self.spi_transceive(&cmd_data, chunk)?
            };

            buf[offset as usize..offset as usize + chunk].copy_from_slice(&result[..chunk]);
            offset += chunk as u32;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Bulk SPI write (synchronous, matches flashprog)
    // -----------------------------------------------------------------------

    /// Bulk write to SPI flash. Both `start` and `len` must be `chunksize`-byte aligned.
    /// chunksize must be 256 (page size). Each 256-byte chunk is zero-padded to 512 bytes
    /// with 0xFF fill, matching flashprog behavior.
    pub fn spi_bulk_write(
        &mut self,
        buf: &[u8],
        chunksize: u32,
        start: u32,
        len: u32,
        write_mode: u8,
        in_4ba_mode: bool,
    ) -> Result<()> {
        if chunksize != 256 {
            return Err(DediprogError::InvalidParameter(format!(
                "Chunk sizes other than 256 bytes are unsupported, chunksize={}",
                chunksize
            )));
        }

        if len == 0 {
            return Ok(());
        }

        if (start % chunksize) != 0 || (len % chunksize) != 0 {
            return Err(DediprogError::InvalidParameter(format!(
                "Unaligned start=0x{:x}, len=0x{:x}",
                start, len
            )));
        }

        let count = len / chunksize;

        // Writes always use single I/O mode
        self.set_io_mode(DpIoMode::Single)?;

        let (cmd_buf, cmd_len, value, idx) =
            self.prepare_rw_cmd(false, write_mode, start, count, None, in_4ba_mode)?;

        // Send CMD_WRITE control transfer to initiate the bulk write
        self.control_write_raw(Command::Write as u8, value, idx, &cmd_buf[..cmd_len])?;

        // Open the OUT endpoint for bulk transfers
        let mut out_ep: Endpoint<Bulk, Out> = self
            .interface
            .endpoint(self.out_endpoint)
            .map_err(|e| DediprogError::TransferFailed(e.to_string()))?;

        // Write each chunk: 256 bytes of data + 256 bytes of 0xFF padding = 512 bytes
        for i in 0..count as usize {
            let mut usbbuf = Buffer::new(BULK_CHUNK_SIZE);
            let data_offset = i * chunksize as usize;
            let data_end = data_offset + chunksize as usize;
            usbbuf.extend_from_slice(&buf[data_offset..data_end]);
            // Pad remaining bytes with 0xFF
            let padding = BULK_CHUNK_SIZE - chunksize as usize;
            usbbuf.extend_fill(padding, 0xFF);

            let completion = out_ep.transfer_blocking(usbbuf, Duration::from_secs(5));
            match completion.status {
                Ok(()) if completion.actual_len == BULK_CHUNK_SIZE => {}
                Ok(()) => {
                    return Err(DediprogError::TransferFailed(format!(
                        "SPI bulk write short transfer: expected {}, got {}",
                        BULK_CHUNK_SIZE, completion.actual_len
                    )));
                }
                Err(e) => {
                    return Err(DediprogError::TransferFailed(format!(
                        "SPI bulk write failed at chunk {}: {:?}",
                        i, e
                    )));
                }
            }
        }

        Ok(())
    }

    /// Write SPI flash using standard page program (256-byte pages).
    /// Handles alignment and splits large writes that exceed MAX_BLOCK_COUNT pages.
    pub fn spi_write_256(
        &mut self,
        buf: &[u8],
        start: u32,
        len: u32,
        in_4ba_mode: bool,
    ) -> Result<()> {
        self.spi_write_chunked(buf, start, len, WriteMode::PagePgm as u8, in_4ba_mode)
    }

    /// Write SPI flash using AAI (Auto Address Increment) mode for SST chips.
    pub fn spi_write_aai(
        &mut self,
        buf: &[u8],
        start: u32,
        len: u32,
        in_4ba_mode: bool,
    ) -> Result<()> {
        self.spi_write_chunked(buf, start, len, WriteMode::TwoByteAai as u8, in_4ba_mode)
    }

    /// Chunked write: splits writes exceeding page_size * MAX_BLOCK_COUNT.
    fn spi_write_chunked(
        &mut self,
        buf: &[u8],
        mut start: u32,
        mut len: u32,
        write_mode: u8,
        in_4ba_mode: bool,
    ) -> Result<()> {
        let page_size: u32 = 256;
        let max_per_op = page_size * MAX_BLOCK_COUNT as u32;
        let mut offset: usize = 0;

        while len > 0 {
            let len_here = len.min(max_per_op);
            self.spi_write_single(
                &buf[offset..offset + len_here as usize],
                start,
                len_here,
                write_mode,
                in_4ba_mode,
            )?;
            start += len_here;
            offset += len_here as usize;
            len -= len_here;
        }
        Ok(())
    }

    /// Write SPI flash with alignment handling (single operation, bounded by MAX_BLOCK_COUNT).
    fn spi_write_single(
        &mut self,
        buf: &[u8],
        start: u32,
        len: u32,
        write_mode: u8,
        in_4ba_mode: bool,
    ) -> Result<()> {
        if len == 0 {
            return Ok(());
        }

        let chunksize: u32 = 256; // page size
        let residue = if start % chunksize != 0 {
            (chunksize - (start % chunksize)).min(len)
        } else {
            0
        };

        self.set_leds(Led::Busy)?;

        // Write unaligned head via slow (transceive-based) writes
        if residue > 0 {
            if let Err(e) = self.slow_write(&buf[..residue as usize], start, residue, in_4ba_mode)
            {
                self.set_leds(Led::Error)?;
                return Err(e);
            }
        }

        // Bulk write the aligned middle
        let bulk_len = ((len - residue) / chunksize) * chunksize;
        if bulk_len > 0 {
            let offset = residue as usize;
            if let Err(e) = self.spi_bulk_write(
                &buf[offset..offset + bulk_len as usize],
                chunksize,
                start + residue,
                bulk_len,
                write_mode,
                in_4ba_mode,
            ) {
                self.set_leds(Led::Error)?;
                return Err(e);
            }
        }

        // Write unaligned tail via slow writes
        let tail_len = len - residue - bulk_len;
        if tail_len > 0 {
            let offset = (residue + bulk_len) as usize;
            if let Err(e) = self.slow_write(
                &buf[offset..offset + tail_len as usize],
                start + residue + bulk_len,
                tail_len,
                in_4ba_mode,
            ) {
                self.set_leds(Led::Error)?;
                return Err(e);
            }
        }

        self.set_leds(Led::Pass)?;
        Ok(())
    }

    /// Slow write via individual transceive commands (for unaligned residue).
    /// Implements page-program in max_write_len-byte chunks via WREN + PP + WIP polling.
    /// Uses 4-byte address commands when `in_4ba_mode` is true.
    fn slow_write(&mut self, buf: &[u8], start: u32, len: u32, in_4ba_mode: bool) -> Result<()> {
        use rflasher_core::spi::opcodes;

        log::debug!(
            "Slow write for partial block from 0x{:x}, length 0x{:x}",
            start,
            len
        );

        let use_4ba = in_4ba_mode || start.checked_add(len).is_none_or(|end| end > 0xFFFFFF);
        // 4BA commands use 5-byte header (opcode + 4 addr), 3BA uses 4-byte header
        let header_len: usize = if use_4ba { 5 } else { 4 };
        let max_write = 16 - header_len; // max_data_write = MAX_CMD_SIZE - header

        let mut offset: u32 = 0;
        while offset < len {
            let chunk = ((len - offset) as usize).min(max_write);
            let addr = start + offset;

            // Send WREN
            let wren = [opcodes::WREN];
            self.spi_transceive(&wren, 0)?;

            // Send Page Program with address + data
            let mut cmd_data = vec![0u8; header_len + chunk];
            if use_4ba {
                cmd_data[0] = opcodes::PP_4B; // 0x12 = JEDEC_BYTE_PROGRAM_4BA
                cmd_data[1] = (addr >> 24) as u8;
                cmd_data[2] = (addr >> 16) as u8;
                cmd_data[3] = (addr >> 8) as u8;
                cmd_data[4] = addr as u8;
                cmd_data[5..5 + chunk]
                    .copy_from_slice(&buf[offset as usize..offset as usize + chunk]);
            } else {
                cmd_data[0] = opcodes::PP; // 0x02 = JEDEC_BYTE_PROGRAM
                cmd_data[1] = (addr >> 16) as u8;
                cmd_data[2] = (addr >> 8) as u8;
                cmd_data[3] = addr as u8;
                cmd_data[4..4 + chunk]
                    .copy_from_slice(&buf[offset as usize..offset as usize + chunk]);
            }
            self.spi_transceive(&cmd_data, 0)?;

            // Poll WIP (Write In Progress) bit with timeout
            // Typical page program is <5ms; 5 seconds is extremely generous.
            let wip_deadline =
                std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let rdsr = [opcodes::RDSR];
                let status = self.spi_transceive(&rdsr, 1)?;
                if status[0] & opcodes::SR1_WIP == 0 {
                    break;
                }
                if std::time::Instant::now() >= wip_deadline {
                    return Err(DediprogError::TransferFailed(
                        "Timed out waiting for flash WIP to clear".to_string(),
                    ));
                }
                std::thread::sleep(Duration::from_micros(50));
            }

            offset += chunk as u32;
        }
        Ok(())
    }
}

impl Drop for Dediprog {
    fn drop(&mut self) {
        // Reset I/O mode to single
        let _ = self.set_io_mode(DpIoMode::Single);
        // Turn off voltage
        let _ = self.set_voltage(0);
    }
}

#[maybe_async(AFIT)]
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

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Check I/O mode support
        check_io_mode_supported(cmd.io_mode, self.features())?;

        // For simple commands, use transceive
        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];
        cmd.encode_header(&mut write_data);
        write_data[header_len..].copy_from_slice(cmd.write_data);

        let read_len = cmd.read_buf.len();
        let result = self
            .spi_transceive(&write_data, read_len)
            .map_err(|_e| CoreError::ProgrammerError)?;

        cmd.read_buf
            .copy_from_slice(&result[..read_len.min(result.len())]);

        Ok(())
    }

    async fn delay_us(&mut self, us: u32) {
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

//! Raiden Debug SPI device implementation
//!
//! This module provides the main `RaidenDebugSpi` struct that implements USB
//! communication with Chrome OS EC USB SPI bridges and the `SpiMaster` trait.

use std::time::Duration;

use nusb::transfer::{Queue, RequestBuffer};
use nusb::{Device, Interface};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::SpiCommand;

use crate::error::{RaidenError, Result};
use crate::protocol::*;

/// Configuration options for opening a Raiden device
#[derive(Debug, Clone, Default)]
pub struct RaidenConfig {
    /// USB serial number to match (None = use first device found)
    pub serial: Option<String>,
    /// Target to enable (AP, EC, H1)
    pub target: Target,
}

/// Raiden Debug SPI programmer
///
/// This struct represents a connection to a Chrome OS EC USB SPI bridge
/// (Raiden Debug SPI) and implements the `SpiMaster` trait for communicating
/// with SPI flash chips.
///
/// Supported devices include:
/// - SuzyQable (USB-C debug cable)
/// - Servo V4
/// - C2D2
/// - uServo
/// - Servo Micro
pub struct RaidenDebugSpi {
    /// USB device handle
    _device: Device,
    /// USB interface
    interface: Interface,
    /// Interface number (for control transfers)
    interface_num: u8,
    /// IN endpoint address
    in_ep: u8,
    /// OUT endpoint address
    out_ep: u8,
    /// Protocol version (1 or 2)
    protocol_version: u8,
    /// Maximum SPI write count (V2 only)
    max_spi_write: u16,
    /// Maximum SPI read count (V2 only)
    max_spi_read: u16,
    /// Whether full duplex is supported (V2 only)
    supports_full_duplex: bool,
}

impl RaidenDebugSpi {
    /// Open a Raiden Debug SPI device with default configuration
    ///
    /// Uses the first device found with the default target (AP).
    pub fn open() -> Result<Self> {
        Self::open_with_config(&RaidenConfig::default())
    }

    /// Open a Raiden Debug SPI device with specific configuration
    pub fn open_with_config(config: &RaidenConfig) -> Result<Self> {
        // Find matching devices
        let devices = Self::find_devices(config.serial.as_deref())?;

        if devices.is_empty() {
            return Err(RaidenError::DeviceNotFound);
        }

        if devices.len() > 1 && config.serial.is_none() {
            return Err(RaidenError::MultipleDevicesFound(devices.len()));
        }

        let device_info = &devices[0];

        log::info!(
            "Opening Raiden Debug SPI device at bus {} address {} (protocol v{})",
            device_info.bus,
            device_info.address,
            device_info.protocol_version,
        );

        let device = device_info
            .info
            .open()
            .map_err(|e| RaidenError::OpenFailed(e.to_string()))?;

        // Claim the interface
        let interface = device
            .claim_interface(device_info.interface_num)
            .map_err(|e| RaidenError::ClaimFailed(e.to_string()))?;

        let mut raiden = Self {
            _device: device,
            interface,
            interface_num: device_info.interface_num,
            in_ep: device_info.in_ep,
            out_ep: device_info.out_ep,
            protocol_version: device_info.protocol_version,
            max_spi_write: V1_MAX_PAYLOAD as u16,
            max_spi_read: V1_MAX_PAYLOAD as u16,
            supports_full_duplex: false,
        };

        // Enable the SPI bridge for the selected target
        raiden.enable_target(config.target)?;

        // For V2, query device configuration
        if raiden.protocol_version >= PROTOCOL_V2 {
            raiden.configure_v2()?;
        }

        Ok(raiden)
    }

    /// Find all Raiden Debug SPI devices
    fn find_devices(serial_filter: Option<&str>) -> Result<Vec<RaidenDeviceInfo>> {
        let mut devices = Vec::new();

        for dev_info in nusb::list_devices()? {
            // Check vendor ID
            if dev_info.vendor_id() != GOOGLE_VID {
                continue;
            }

            // Check serial number filter
            if let Some(filter) = serial_filter {
                if let Some(serial) = dev_info.serial_number() {
                    if !serial.contains(filter) {
                        continue;
                    }
                } else {
                    continue;
                }
            }

            // Check interfaces for Raiden SPI subclass using DeviceInfo
            for iface_info in dev_info.interfaces() {
                if iface_info.class() == 0xFF
                    && iface_info.subclass() == RAIDEN_SPI_SUBCLASS
                    && (iface_info.protocol() == PROTOCOL_V1
                        || iface_info.protocol() == PROTOCOL_V2)
                {
                    // Need to open device to get endpoint addresses
                    let device = match dev_info.open() {
                        Ok(d) => d,
                        Err(e) => {
                            log::debug!("Failed to open device for endpoint discovery: {}", e);
                            continue;
                        }
                    };

                    // Get endpoint addresses from the active configuration
                    let mut in_ep = None;
                    let mut out_ep = None;

                    if let Ok(config) = device.active_configuration() {
                        for iface in config.interface_alt_settings() {
                            if iface.interface_number() == iface_info.interface_number() {
                                for ep in iface.endpoints() {
                                    match ep.direction() {
                                        nusb::transfer::Direction::In => {
                                            if in_ep.is_none() {
                                                in_ep = Some(ep.address());
                                            }
                                        }
                                        nusb::transfer::Direction::Out => {
                                            if out_ep.is_none() {
                                                out_ep = Some(ep.address());
                                            }
                                        }
                                    }
                                }
                                break;
                            }
                        }
                    }

                    if let (Some(in_ep), Some(out_ep)) = (in_ep, out_ep) {
                        devices.push(RaidenDeviceInfo {
                            info: dev_info.clone(),
                            bus: dev_info.bus_number(),
                            address: dev_info.device_address(),
                            serial: dev_info.serial_number().map(|s| s.to_string()),
                            interface_num: iface_info.interface_number(),
                            in_ep,
                            out_ep,
                            protocol_version: iface_info.protocol(),
                        });
                    }

                    // Only use first matching interface per device
                    break;
                }
            }
        }

        Ok(devices)
    }

    /// List all connected Raiden Debug SPI devices
    pub fn list_devices() -> Result<Vec<RaidenDeviceInfo>> {
        Self::find_devices(None)
    }

    /// Enable the SPI bridge for a specific target
    fn enable_target(&mut self, target: Target) -> Result<()> {
        let request = target.enable_request();

        log::debug!(
            "Enabling SPI bridge for target: {} (interface {})",
            target,
            self.interface_num
        );

        // Send USB control transfer to enable the target
        // bmRequestType: 0x41 = Host-to-device, Vendor, Interface
        // bRequest: the request type (enable AP/EC/H1)
        // wValue: 0
        // wIndex: interface number
        let result = futures_lite::future::block_on(self.interface.control_out(
            nusb::transfer::ControlOut {
                control_type: nusb::transfer::ControlType::Vendor,
                recipient: nusb::transfer::Recipient::Interface,
                request: request as u8,
                value: 0,
                index: self.interface_num as u16,
                data: &[],
            },
        ));

        result
            .status
            .map_err(|e| RaidenError::EnableFailed(e.to_string()))?;

        // Wait for target to stabilize
        std::thread::sleep(Duration::from_millis(ENABLE_DELAY_MS));

        log::info!("SPI bridge enabled for target: {}", target);
        Ok(())
    }

    /// Disable the SPI bridge
    fn disable(&mut self) -> Result<()> {
        log::debug!("Disabling SPI bridge (interface {})", self.interface_num);

        let result = futures_lite::future::block_on(self.interface.control_out(
            nusb::transfer::ControlOut {
                control_type: nusb::transfer::ControlType::Vendor,
                recipient: nusb::transfer::Recipient::Interface,
                request: ControlRequest::Disable as u8,
                value: 0,
                index: self.interface_num as u16,
                data: &[],
            },
        ));

        result
            .status
            .map_err(|e| RaidenError::EnableFailed(e.to_string()))?;

        Ok(())
    }

    /// Configure V2 protocol parameters
    fn configure_v2(&mut self) -> Result<()> {
        log::debug!("Querying V2 device configuration");

        for retry in 0..WRITE_RETRIES {
            // Send get config command
            let cmd = CommandV2GetConfig::default();
            self.write_packet(&cmd.to_bytes())?;

            // Read response
            let rsp_buf = self.read_packet()?;
            let rsp = ResponseV2Config::from_bytes(&rsp_buf);

            // Validate response
            if rsp.packet_id == PacketId::RspUsbSpiConfig as u16 {
                self.max_spi_write = rsp.max_write_count;
                self.max_spi_read = rsp.max_read_count;
                self.supports_full_duplex = rsp.supports_full_duplex();

                log::info!(
                    "V2 config: max_write={}, max_read={}, full_duplex={}",
                    self.max_spi_write,
                    self.max_spi_read,
                    self.supports_full_duplex
                );

                return Ok(());
            }

            log::warn!(
                "Invalid config response (attempt {}), retrying...",
                retry + 1
            );
            std::thread::sleep(Duration::from_millis(RETRY_DELAY_MS));
        }

        Err(RaidenError::InvalidResponse(
            "Failed to get V2 configuration".into(),
        ))
    }

    /// Send a packet to the device
    fn write_packet(&mut self, data: &[u8]) -> Result<()> {
        let mut queue: Queue<Vec<u8>> = self.interface.bulk_out_queue(self.out_ep);
        queue.submit(data.to_vec());

        let completion = futures_lite::future::block_on(async { queue.next_complete().await });

        completion
            .status
            .map_err(|e| RaidenError::TransferFailed(e.to_string()))?;

        log::trace!("USB write {} bytes", data.len());
        Ok(())
    }

    /// Read a packet from the device
    fn read_packet(&mut self) -> Result<Vec<u8>> {
        let mut queue: Queue<RequestBuffer> = self.interface.bulk_in_queue(self.in_ep);
        queue.submit(RequestBuffer::new(USB_PACKET_SIZE));

        let completion = futures_lite::future::block_on(async { queue.next_complete().await });

        completion
            .status
            .map_err(|e| RaidenError::TransferFailed(e.to_string()))?;

        log::trace!("USB read {} bytes", completion.data.len());
        Ok(completion.data)
    }

    /// Execute an SPI transaction using V1 protocol
    fn spi_transfer_v1(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        if write_data.len() > V1_MAX_PAYLOAD {
            return Err(RaidenError::InvalidParameter(format!(
                "Write length {} exceeds V1 max {}",
                write_data.len(),
                V1_MAX_PAYLOAD
            )));
        }
        if read_len > V1_MAX_PAYLOAD {
            return Err(RaidenError::InvalidParameter(format!(
                "Read length {} exceeds V1 max {}",
                read_len, V1_MAX_PAYLOAD
            )));
        }

        // Build and send command
        let cmd = CommandV1::new(write_data, read_len as u8);
        let packet = cmd.to_bytes();
        log::debug!(
            "V1 command: write_count={}, read_count={}, packet_len={}, data={:02X?}",
            cmd.write_count,
            cmd.read_count,
            packet.len(),
            &packet[..]
        );
        self.write_packet(&packet)?;

        // Read response
        let rsp_buf = self.read_packet()?;
        let rsp = ResponseV1::from_bytes(&rsp_buf);

        // Check status
        let status = rsp.status();
        log::debug!(
            "V1 response: status_code=0x{:04X} ({:?})",
            rsp.status_code,
            status
        );
        if !status.is_success() {
            return Err(RaidenError::ProtocolError(rsp.status_code));
        }

        // Extract read data
        Ok(rsp.data[..read_len].to_vec())
    }

    /// Execute an SPI transaction using V2 protocol
    fn spi_transfer_v2(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        let write_len = write_data.len();

        if write_len > self.max_spi_write as usize {
            return Err(RaidenError::InvalidParameter(format!(
                "Write length {} exceeds V2 max {}",
                write_len, self.max_spi_write
            )));
        }
        if read_len > self.max_spi_read as usize {
            return Err(RaidenError::InvalidParameter(format!(
                "Read length {} exceeds V2 max {}",
                read_len, self.max_spi_read
            )));
        }

        // Send start command with first chunk of data
        let first_chunk_len = std::cmp::min(write_len, V2_START_PAYLOAD);
        let cmd = CommandV2Start::new(
            write_len as u16,
            read_len as u16,
            &write_data[..first_chunk_len],
        );
        self.write_packet(&cmd.to_bytes())?;

        // Send continue packets for remaining write data
        let mut write_offset = first_chunk_len;
        while write_offset < write_len {
            let chunk_len = std::cmp::min(write_len - write_offset, V2_CONTINUE_PAYLOAD);
            let cmd = CommandV2Continue::new(
                write_offset as u16,
                &write_data[write_offset..write_offset + chunk_len],
            );
            self.write_packet(&cmd.to_bytes())?;
            write_offset += chunk_len;
        }

        // Read start response
        let rsp_buf = self.read_packet()?;
        let rsp = ResponseV2Start::from_bytes(&rsp_buf);

        // Check packet ID
        if rsp.packet_id != PacketId::RspTransferStart as u16 {
            return Err(RaidenError::InvalidResponse(format!(
                "Expected RspTransferStart, got {}",
                rsp.packet_id
            )));
        }

        // Check status
        let status = rsp.status();
        if !status.is_success() {
            return Err(RaidenError::ProtocolError(rsp.status_code));
        }

        // Collect read data
        let mut result = Vec::with_capacity(read_len);
        let first_read_len = std::cmp::min(read_len, V2_CONTINUE_PAYLOAD);
        result.extend_from_slice(&rsp.data[..first_read_len]);

        // Read continue packets for remaining data
        while result.len() < read_len {
            let rsp_buf = self.read_packet()?;
            let rsp = ResponseV2Continue::from_bytes(&rsp_buf);

            // Validate packet
            if rsp.packet_id != PacketId::RspTransferContinue as u16 {
                return Err(RaidenError::InvalidResponse(format!(
                    "Expected RspTransferContinue, got {}",
                    rsp.packet_id
                )));
            }

            // Validate data index
            if rsp.data_index != result.len() as u16 {
                return Err(RaidenError::InvalidResponse(format!(
                    "Data index mismatch: expected {}, got {}",
                    result.len(),
                    rsp.data_index
                )));
            }

            let chunk_len = std::cmp::min(read_len - result.len(), V2_CONTINUE_PAYLOAD);
            result.extend_from_slice(&rsp.data[..chunk_len]);
        }

        Ok(result)
    }

    /// Execute an SPI transaction
    fn spi_transfer(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        for retry in 0..WRITE_RETRIES {
            let result = if self.protocol_version >= PROTOCOL_V2 {
                self.spi_transfer_v2(write_data, read_len)
            } else {
                self.spi_transfer_v1(write_data, read_len)
            };

            match result {
                Ok(data) => return Ok(data),
                Err(RaidenError::ProtocolError(code)) if code == StatusCode::Busy as u16 => {
                    log::warn!("SPI busy (attempt {}), retrying...", retry + 1);
                    std::thread::sleep(Duration::from_millis(RETRY_DELAY_MS));
                }
                Err(e) => return Err(e),
            }
        }

        Err(RaidenError::Timeout)
    }
}

impl Drop for RaidenDebugSpi {
    fn drop(&mut self) {
        if let Err(e) = self.disable() {
            log::warn!("Failed to disable SPI bridge on close: {}", e);
        }
    }
}

impl SpiMaster for RaidenDebugSpi {
    fn features(&self) -> SpiFeatures {
        // Raiden supports 4-byte addressing
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        self.max_spi_read as usize
    }

    fn max_write_len(&self) -> usize {
        self.max_spi_write as usize
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Build the command bytes to send
        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];

        // Encode opcode + address + dummy bytes
        cmd.encode_header(&mut write_data);

        // Append write data (for write commands)
        write_data[header_len..].copy_from_slice(cmd.write_data);

        // Perform the transfer
        let read_len = cmd.read_buf.len();
        log::debug!(
            "SPI execute: opcode=0x{:02X}, write_len={}, read_len={}",
            cmd.opcode,
            write_data.len(),
            read_len
        );
        let result = self.spi_transfer(&write_data, read_len).map_err(|e| {
            log::error!("SPI transfer failed: {}", e);
            CoreError::ProgrammerError
        })?;

        // Copy read data back
        cmd.read_buf.copy_from_slice(&result);

        Ok(())
    }

    fn delay_us(&mut self, us: u32) {
        if us > 0 {
            std::thread::sleep(Duration::from_micros(us as u64));
        }
    }
}

/// Information about a connected Raiden Debug SPI device
#[derive(Debug, Clone)]
pub struct RaidenDeviceInfo {
    /// nusb device info
    pub(crate) info: nusb::DeviceInfo,
    /// USB bus number
    pub bus: u8,
    /// USB device address
    pub address: u8,
    /// Device serial number (if available)
    pub serial: Option<String>,
    /// Interface number
    pub(crate) interface_num: u8,
    /// IN endpoint address
    pub(crate) in_ep: u8,
    /// OUT endpoint address
    pub(crate) out_ep: u8,
    /// Protocol version
    pub protocol_version: u8,
}

impl std::fmt::Display for RaidenDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Raiden Debug SPI at bus {} address {} (v{})",
            self.bus, self.address, self.protocol_version
        )?;
        if let Some(ref serial) = self.serial {
            write!(f, " serial={}", serial)?;
        }
        Ok(())
    }
}

/// Parse programmer options from key-value pairs
///
/// Supported options:
/// - `serial=<serial>` - USB serial number to match
/// - `target=<ap|ec|h1|ap_custom>` - Target to enable
pub fn parse_options(options: &[(&str, &str)]) -> Result<RaidenConfig> {
    let mut config = RaidenConfig::default();

    for (key, value) in options {
        match *key {
            "serial" => {
                config.serial = Some(value.to_string());
            }
            "target" => {
                config.target = value
                    .parse()
                    .map_err(|e: String| RaidenError::InvalidParameter(e))?;
            }
            _ => {
                return Err(RaidenError::InvalidParameter(format!(
                    "Unknown option: {}",
                    key
                )));
            }
        }
    }

    Ok(config)
}

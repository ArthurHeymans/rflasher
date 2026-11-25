//! Raiden Debug SPI device implementation
//!
//! This module provides the main `RaidenDebugSpi` struct that implements USB
//! communication with Chrome OS EC USB SPI bridges and the `SpiMaster` trait.

use std::time::Duration;

use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::{Endpoint, Interface};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::SpiCommand;

use super::error::{RaidenError, Result};
use super::protocol::*;
use crate::time::delay;
use crate::usb_ep::EpWaitExt;

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

/// Whether `iface` is a Raiden Debug SPI interface.
fn is_raiden_interface(iface: &nusb::InterfaceInfo) -> bool {
    iface.class() == 0xFF
        && iface.subclass() == RAIDEN_SPI_SUBCLASS
        && (iface.protocol() == PROTOCOL_V1 || iface.protocol() == PROTOCOL_V2)
}

impl RaidenDebugSpi {
    /// Open a Raiden Debug SPI device
    ///
    /// Natively, find devices with [`crate::UsbProgrammer::candidates`]; in
    /// the browser, use `request_device`.
    pub async fn open(device_info: nusb::DeviceInfo, config: &RaidenConfig) -> Result<Self> {
        let iface_info = device_info
            .interfaces()
            .find(|iface| is_raiden_interface(iface))
            .ok_or(RaidenError::DeviceNotFound)?;

        let interface_num = iface_info.interface_number();
        let protocol_version = iface_info.protocol();

        let device = device_info
            .open()
            .await
            .map_err(|e| RaidenError::OpenFailed(e.to_string()))?;

        let config_desc = device
            .active_configuration()
            .map_err(|e| RaidenError::OpenFailed(format!("Failed to get config: {}", e)))?;

        let mut in_ep = None;
        let mut out_ep = None;
        for iface in config_desc.interface_alt_settings() {
            if iface.interface_number() != interface_num {
                continue;
            }
            for ep in iface.endpoints() {
                match ep.direction() {
                    nusb::transfer::Direction::In if in_ep.is_none() => in_ep = Some(ep.address()),
                    nusb::transfer::Direction::Out if out_ep.is_none() => {
                        out_ep = Some(ep.address())
                    }
                    _ => {}
                }
            }
            break;
        }

        let (in_ep, out_ep) = match (in_ep, out_ep) {
            (Some(in_ep), Some(out_ep)) => (in_ep, out_ep),
            _ => {
                return Err(RaidenError::OpenFailed(
                    "Failed to discover Raiden bulk endpoints".to_string(),
                ));
            }
        };

        let interface = device
            .claim_interface(interface_num)
            .await
            .map_err(|e| RaidenError::ClaimFailed(e.to_string()))?;

        let mut raiden = Self {
            interface,
            interface_num,
            in_ep,
            out_ep,
            protocol_version,
            max_spi_write: V1_MAX_PAYLOAD as u16,
            max_spi_read: V1_MAX_PAYLOAD as u16,
            supports_full_duplex: false,
        };

        raiden.enable_target(config.target).await?;

        if raiden.protocol_version >= PROTOCOL_V2 {
            raiden.configure_v2().await?;
        }

        Ok(raiden)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl crate::UsbProgrammer for RaidenDebugSpi {
    const NAME: &'static str = "Raiden Debug SPI";
    type Config = RaidenConfig;
    type Error = RaidenError;

    /// Applies the `serial` selector (substring match).
    async fn candidates(config: &RaidenConfig) -> Result<Vec<nusb::DeviceInfo>> {
        let selector = nusb::DeviceSelector::all().with_vid(GOOGLE_VID);
        Ok(crate::usb::list_devices(&[selector])
            .await?
            .into_iter()
            .filter(|info| info.interfaces().any(is_raiden_interface))
            .filter(|info| {
                config.serial.as_deref().is_none_or(|filter| {
                    info.serial_number()
                        .is_some_and(|serial| serial.contains(filter))
                })
            })
            .collect())
    }

    async fn open_device(device: nusb::DeviceInfo, config: RaidenConfig) -> Result<Self> {
        Self::open(device, &config).await
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl RaidenDebugSpi {
    /// Request a Raiden device via the WebUSB permission prompt.
    pub async fn request_device() -> Result<nusb::DeviceInfo> {
        log::info!("Requesting Raiden Debug SPI device via WebUSB picker...");

        let selector = nusb::DeviceSelector::all().with_vid(GOOGLE_VID);
        nusb::request_device(&[selector])
            .await
            .map_err(|e| RaidenError::OpenFailed(format!("WebUSB request failed: {e}")))?
            .ok_or(RaidenError::DeviceNotFound)
    }

    /// Shut down the bridge explicitly in WASM mode.
    pub async fn shutdown(&mut self) {
        if let Err(e) = self.disable().await {
            log::warn!("Failed to disable SPI bridge on shutdown: {}", e);
        }
    }
}

#[cfg_attr(any(), allow(dead_code))]
impl RaidenDebugSpi {
    /// Enable the SPI bridge for a specific target.
    async fn enable_target(&mut self, target: Target) -> Result<()> {
        let request = target.enable_request();

        log::debug!(
            "Enabling SPI bridge for target: {} (interface {})",
            target,
            self.interface_num
        );

        self.interface
            .control_out(
                nusb::transfer::ControlOut {
                    control_type: nusb::transfer::ControlType::Vendor,
                    recipient: nusb::transfer::Recipient::Interface,
                    request: request as u8,
                    value: 0,
                    index: self.interface_num as u16,
                    data: &[],
                },
                Duration::from_secs(5),
            )
            .await
            .map_err(|e| RaidenError::EnableFailed(e.to_string()))?;

        delay(Duration::from_millis(ENABLE_DELAY_MS)).await;

        log::info!("SPI bridge enabled for target: {}", target);
        Ok(())
    }

    /// Disable the SPI bridge.
    async fn disable(&mut self) -> Result<()> {
        log::debug!("Disabling SPI bridge (interface {})", self.interface_num);

        self.interface
            .control_out(
                nusb::transfer::ControlOut {
                    control_type: nusb::transfer::ControlType::Vendor,
                    recipient: nusb::transfer::Recipient::Interface,
                    request: ControlRequest::Disable as u8,
                    value: 0,
                    index: self.interface_num as u16,
                    data: &[],
                },
                Duration::from_secs(5),
            )
            .await
            .map_err(|e| RaidenError::EnableFailed(e.to_string()))?;

        Ok(())
    }

    /// Configure V2 protocol parameters.
    async fn configure_v2(&mut self) -> Result<()> {
        log::debug!("Querying V2 device configuration");

        for retry in 0..WRITE_RETRIES {
            let cmd = CommandV2GetConfig::default();
            self.write_packet(&cmd.to_bytes()).await?;

            let rsp_buf = self.read_packet().await?;
            if let Some(rsp) = ResponseV2Config::from_bytes(&rsp_buf)
                && rsp.packet_id == PacketId::RspUsbSpiConfig as u16
            {
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
            delay(Duration::from_millis(RETRY_DELAY_MS)).await;
        }

        Err(RaidenError::InvalidResponse(
            "Failed to get V2 configuration".into(),
        ))
    }

    /// Send a packet to the device.
    async fn write_packet(&mut self, data: &[u8]) -> Result<()> {
        let mut out_ep: Endpoint<Bulk, Out> = self
            .interface
            .endpoint(self.out_ep)
            .map_err(|e| RaidenError::TransferFailed(e.to_string()))?;

        out_ep.submit(Buffer::from(data.to_vec()));
        let completion = out_ep
            .next_complete_timeout(Duration::from_secs(5))
            .await
            .ok_or(RaidenError::Timeout)?;
        completion
            .status
            .map_err(|e| RaidenError::TransferFailed(e.to_string()))?;
        if completion.actual_len != data.len() {
            return Err(RaidenError::TransferFailed(format!(
                "Short USB write: expected {} bytes, wrote {}",
                data.len(),
                completion.actual_len
            )));
        }

        log::trace!("USB write {} bytes", data.len());
        Ok(())
    }

    /// Read a packet from the device.
    async fn read_packet(&mut self) -> Result<Vec<u8>> {
        let mut in_ep: Endpoint<Bulk, In> = self
            .interface
            .endpoint(self.in_ep)
            .map_err(|e| RaidenError::TransferFailed(e.to_string()))?;

        let mut buf = Buffer::new(USB_PACKET_SIZE);
        buf.set_requested_len(USB_PACKET_SIZE);
        in_ep.submit(buf);
        let completion = in_ep
            .next_complete_timeout(Duration::from_secs(5))
            .await
            .ok_or(RaidenError::Timeout)?;
        completion
            .status
            .map_err(|e| RaidenError::TransferFailed(e.to_string()))?;

        let len = completion.actual_len.min(completion.buffer.len());
        let data = completion.buffer[..len].to_vec();
        log::trace!("USB read {} bytes", data.len());
        Ok(data)
    }

    /// Execute an SPI transaction using V1 protocol.
    async fn spi_transfer_v1(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
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

        let cmd = CommandV1::new(write_data, read_len as u8);
        let packet = cmd.to_bytes();
        log::debug!(
            "V1 command: write_count={}, read_count={}, packet_len={}, data={:02X?}",
            cmd.write_count,
            cmd.read_count,
            packet.len(),
            &packet[..]
        );
        self.write_packet(&packet).await?;

        let rsp_buf = self.read_packet().await?;
        decode_v1_response(&rsp_buf, read_len)
    }

    /// Execute an SPI transaction using V2 protocol.
    async fn spi_transfer_v2(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
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

        let first_chunk_len = std::cmp::min(write_len, V2_START_PAYLOAD);
        let cmd = CommandV2Start::new(
            write_len as u16,
            read_len as u16,
            &write_data[..first_chunk_len],
        );
        self.write_packet(&cmd.to_bytes()).await?;

        let mut write_offset = first_chunk_len;
        while write_offset < write_len {
            let chunk_len = std::cmp::min(write_len - write_offset, V2_CONTINUE_PAYLOAD);
            let cmd = CommandV2Continue::new(
                write_offset as u16,
                &write_data[write_offset..write_offset + chunk_len],
            );
            self.write_packet(&cmd.to_bytes()).await?;
            write_offset += chunk_len;
        }

        let rsp_buf = self.read_packet().await?;
        let mut result = decode_v2_start_response(&rsp_buf, read_len)?;

        while result.len() < read_len {
            let rsp_buf = self.read_packet().await?;
            append_v2_continue_response(&rsp_buf, &mut result, read_len)?;
        }

        Ok(result)
    }

    /// Execute an SPI transaction.
    async fn spi_transfer(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        for retry in 0..WRITE_RETRIES {
            let result = if self.protocol_version >= PROTOCOL_V2 {
                self.spi_transfer_v2(write_data, read_len).await
            } else {
                self.spi_transfer_v1(write_data, read_len).await
            };

            match result {
                Ok(data) => return Ok(data),
                Err(RaidenError::ProtocolError(code)) if code == StatusCode::Busy as u16 => {
                    log::warn!("SPI busy (attempt {}), retrying...", retry + 1);
                    delay(Duration::from_millis(RETRY_DELAY_MS)).await;
                }
                Err(e) => return Err(e),
            }
        }

        Err(RaidenError::Timeout)
    }
}

// Native-only best-effort cleanup; WASM uses explicit shutdown instead.
#[cfg(not(target_arch = "wasm32"))]
impl Drop for RaidenDebugSpi {
    fn drop(&mut self) {
        if let Err(e) = futures_lite::future::block_on(self.disable()) {
            log::warn!("Failed to disable SPI bridge on close: {}", e);
        }
    }
}

impl SpiMaster for RaidenDebugSpi {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        self.max_spi_read as usize
    }

    fn max_write_len(&self) -> usize {
        self.max_spi_write as usize
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        log::debug!(
            "SPI execute: opcode=0x{:02X}, read_len={}",
            cmd.opcode,
            cmd.read_buf.len()
        );

        let write_data = cmd.single_io_frame()?;

        let result = self
            .spi_transfer(&write_data, cmd.read_buf.len())
            .await
            .map_err(|e| {
                log::error!("SPI transfer failed: {}", e);
                CoreError::ProgrammerError
            })?;

        cmd.read_buf.copy_from_slice(&result[..cmd.read_buf.len()]);
        Ok(())
    }

    async fn delay_us(&mut self, us: u32) {
        delay(Duration::from_micros(u64::from(us))).await;
    }
}

/// Decode a V1 response packet, validating status before payload length.
fn decode_v1_response(rsp_buf: &[u8], read_len: usize) -> Result<Vec<u8>> {
    let rsp = ResponseV1::from_bytes(rsp_buf).ok_or_else(|| {
        RaidenError::InvalidResponse(format!(
            "V1 response packet has invalid length ({} bytes)",
            rsp_buf.len()
        ))
    })?;

    let status = rsp.status();
    log::debug!(
        "V1 response: status_code=0x{:04X} ({:?}), payload={} bytes",
        rsp.status_code,
        status,
        rsp.data_len
    );
    if !status.is_success() {
        return Err(RaidenError::ProtocolError(rsp.status_code));
    }

    if rsp.data_len < read_len {
        return Err(RaidenError::InvalidResponse(format!(
            "V1 response payload truncated: expected {read_len} bytes, got {}",
            rsp.data_len
        )));
    }

    Ok(rsp.payload()[..read_len].to_vec())
}

/// Decode a V2 start response packet, validating header and status before payload length.
fn decode_v2_start_response(rsp_buf: &[u8], read_len: usize) -> Result<Vec<u8>> {
    let rsp = ResponseV2Start::from_bytes(rsp_buf).ok_or_else(|| {
        RaidenError::InvalidResponse(format!(
            "V2 start response packet has invalid length ({} bytes)",
            rsp_buf.len()
        ))
    })?;

    if rsp.packet_id != PacketId::RspTransferStart as u16 {
        return Err(RaidenError::InvalidResponse(format!(
            "Expected RspTransferStart, got {}",
            rsp.packet_id
        )));
    }

    let status = rsp.status();
    if !status.is_success() {
        return Err(RaidenError::ProtocolError(rsp.status_code));
    }

    let expected_first = std::cmp::min(read_len, V2_CONTINUE_PAYLOAD);
    if rsp.data_len < expected_first {
        return Err(RaidenError::InvalidResponse(format!(
            "V2 start response payload truncated: expected {expected_first} bytes, got {}",
            rsp.data_len
        )));
    }

    let mut result = Vec::with_capacity(read_len);
    result.extend_from_slice(&rsp.payload()[..expected_first]);
    Ok(result)
}

/// Decode and append one V2 continuation packet, validating packet ID, offset, and payload length.
fn append_v2_continue_response(
    rsp_buf: &[u8],
    result: &mut Vec<u8>,
    read_len: usize,
) -> Result<()> {
    let rsp = ResponseV2Continue::from_bytes(rsp_buf).ok_or_else(|| {
        RaidenError::InvalidResponse(format!(
            "V2 continue response packet has invalid length ({} bytes)",
            rsp_buf.len()
        ))
    })?;

    if rsp.packet_id != PacketId::RspTransferContinue as u16 {
        return Err(RaidenError::InvalidResponse(format!(
            "Expected RspTransferContinue, got {}",
            rsp.packet_id
        )));
    }

    let expected_index = u16::try_from(result.len()).map_err(|_| {
        RaidenError::InvalidResponse(format!(
            "V2 continuation offset overflow at {} bytes",
            result.len()
        ))
    })?;
    if rsp.data_index != expected_index {
        return Err(RaidenError::InvalidResponse(format!(
            "Data index mismatch: expected {}, got {}",
            expected_index, rsp.data_index
        )));
    }

    let remaining = read_len - result.len();
    let expected_chunk = std::cmp::min(remaining, V2_CONTINUE_PAYLOAD);
    if rsp.data_len == 0 || rsp.data_len < expected_chunk {
        return Err(RaidenError::InvalidResponse(format!(
            "V2 continue response payload truncated: expected {expected_chunk} bytes, got {}",
            rsp.data_len
        )));
    }

    result.extend_from_slice(&rsp.payload()[..expected_chunk]);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_response_rejects_short_header_and_truncated_payload() {
        assert!(matches!(
            decode_v1_response(&[], 0),
            Err(RaidenError::InvalidResponse(_))
        ));
        assert!(matches!(
            decode_v1_response(&[0x00], 0),
            Err(RaidenError::InvalidResponse(_))
        ));
        // Header-only error response reports ProtocolError before checking payload length.
        assert!(matches!(
            decode_v1_response(&(StatusCode::Busy as u16).to_le_bytes(), 16),
            Err(RaidenError::ProtocolError(code)) if code == StatusCode::Busy as u16
        ));
        // Success with fewer bytes than requested must not zero-fill.
        assert!(matches!(
            decode_v1_response(&[0x00, 0x00, 0xaa, 0xbb], 4),
            Err(RaidenError::InvalidResponse(_))
        ));
        // Legitimately short exact-length response succeeds.
        assert_eq!(
            decode_v1_response(&[0x00, 0x00, 0xaa, 0xbb, 0xcc], 3).unwrap(),
            vec![0xaa, 0xbb, 0xcc]
        );
        // Header-only success for read_len == 0 succeeds.
        assert!(decode_v1_response(&[0x00, 0x00], 0).unwrap().is_empty());
    }

    #[test]
    fn v2_start_and_continue_validate_headers_status_and_lengths() {
        // Short header rejected.
        assert!(matches!(
            decode_v2_start_response(&[5, 0, 0], 1),
            Err(RaidenError::InvalidResponse(_))
        ));
        // Header-only error response reports ProtocolError even when read_len > 0.
        let mut err_pkt = Vec::new();
        err_pkt.extend_from_slice(&(PacketId::RspTransferStart as u16).to_le_bytes());
        err_pkt.extend_from_slice(&(StatusCode::Timeout as u16).to_le_bytes());
        assert!(matches!(
            decode_v2_start_response(&err_pkt, 32),
            Err(RaidenError::ProtocolError(code)) if code == StatusCode::Timeout as u16
        ));
        // Success with read_len == 0 and header-only packet succeeds.
        let mut ok_empty = Vec::new();
        ok_empty.extend_from_slice(&(PacketId::RspTransferStart as u16).to_le_bytes());
        ok_empty.extend_from_slice(&(StatusCode::Success as u16).to_le_bytes());
        assert!(decode_v2_start_response(&ok_empty, 0).unwrap().is_empty());

        // Truncated start payload must fail rather than zero-fill.
        let mut short_start = ok_empty.clone();
        short_start.extend_from_slice(&[1, 2, 3]);
        assert!(matches!(
            decode_v2_start_response(&short_start, 5),
            Err(RaidenError::InvalidResponse(_))
        ));
        // Legitimately short single-packet read succeeds.
        assert_eq!(
            decode_v2_start_response(&short_start, 3).unwrap(),
            vec![1, 2, 3]
        );

        // Full multi-packet read with legitimately short final continuation packet.
        let total = V2_CONTINUE_PAYLOAD + 4;
        let mut full_start = ok_empty;
        full_start.extend(0u8..V2_CONTINUE_PAYLOAD as u8);
        let mut assembled = decode_v2_start_response(&full_start, total).unwrap();
        assert_eq!(assembled.len(), V2_CONTINUE_PAYLOAD);

        // Wrong data_index rejected.
        let mut bad_idx = Vec::new();
        bad_idx.extend_from_slice(&(PacketId::RspTransferContinue as u16).to_le_bytes());
        bad_idx.extend_from_slice(&0u16.to_le_bytes());
        bad_idx.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert!(matches!(
            append_v2_continue_response(&bad_idx, &mut assembled, total),
            Err(RaidenError::InvalidResponse(_))
        ));

        // Zero-length or truncated continuation rejected.
        let mut short_cont = Vec::new();
        short_cont.extend_from_slice(&(PacketId::RspTransferContinue as u16).to_le_bytes());
        short_cont.extend_from_slice(&(V2_CONTINUE_PAYLOAD as u16).to_le_bytes());
        assert!(matches!(
            append_v2_continue_response(&short_cont, &mut assembled, total),
            Err(RaidenError::InvalidResponse(_))
        ));
        short_cont.extend_from_slice(&[0xde, 0xad]);
        assert!(matches!(
            append_v2_continue_response(&short_cont, &mut assembled, total),
            Err(RaidenError::InvalidResponse(_))
        ));

        // Exact short final continuation succeeds.
        short_cont.extend_from_slice(&[0xbe, 0xef]);
        append_v2_continue_response(&short_cont, &mut assembled, total).unwrap();
        assert_eq!(assembled.len(), total);
        assert_eq!(&assembled[V2_CONTINUE_PAYLOAD..], &[0xde, 0xad, 0xbe, 0xef]);
    }
}

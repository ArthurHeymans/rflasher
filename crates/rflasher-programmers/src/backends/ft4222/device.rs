//! FT4222H device implementation
//!
//! This module provides the main `Ft4222` struct that implements USB
//! communication with the FT4222H SPI master and the `SpiMaster` trait.
//!
//! Async on every target: native drives the async API with a `block_on`
//! boundary in the application, WASM uses WebUSB.

use std::time::Duration;

use nusb::transfer::{Buffer, Bulk, ControlIn, ControlOut, ControlType, In, Out, Recipient};
use nusb::{Endpoint, Interface};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{IoMode as CoreIoMode, SpiCommand};

use super::error::{Ft4222Error, Result};
use super::protocol::*;
use crate::usb_ep::EpWaitExt;

// ---------------------------------------------------------------------------
// Platform-specific endpoint/future helpers
// ---------------------------------------------------------------------------

/// Wait for the next completion on an endpoint, giving up after the timeout.
/// Returns `Option<Completion>` (`None` on timeout).
macro_rules! ep_wait {
    ($ep:expr, $timeout:expr) => {
        $ep.next_complete_timeout($timeout).await
    };
}

/// Resolve an nusb `MaybeFuture` to its output.
/// In sync mode: calls `.await`.
/// In async mode: awaits the future.
macro_rules! nusb_await {
    ($expr:expr) => {{ $expr.await }};
}

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
    /// USB interface handle. In WASM this is also kept alive to maintain the claim.
    interface: Interface,
    /// Current SPI configuration.
    config: SpiConfig,
    /// Selected clock configuration.
    clock_config: ClockConfig,
    /// Control interface index (from USB descriptor).
    control_index: u8,
    /// Bulk IN endpoint address.
    in_ep: u8,
    /// Bulk OUT endpoint address.
    out_ep: u8,
    /// Current I/O lines mode.
    io_lines: u8,
    /// Cached bulk OUT endpoint.
    out_endpoint: Option<Endpoint<Bulk, Out>>,
    /// Cached bulk IN endpoint.
    in_endpoint: Option<Endpoint<Bulk, In>>,
    /// Cached `max_packet_size` for the bulk IN endpoint.
    in_max_packet_size: usize,
}

// ---------------------------------------------------------------------------
// Native-only methods (device enumeration)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "std", not(feature = "wasm")))]
impl Ft4222 {
    /// Open an FT4222H device with default configuration.
    pub async fn open() -> Result<Self> {
        Self::open_with_config(SpiConfig::default()).await
    }

    /// Open an FT4222H device with custom configuration.
    pub async fn open_with_config(config: SpiConfig) -> Result<Self> {
        Self::open_nth_with_config(0, config).await
    }

    /// Open the nth FT4222H device (0-indexed) with default configuration.
    pub async fn open_nth(index: usize) -> Result<Self> {
        Self::open_nth_with_config(index, SpiConfig::default()).await
    }

    /// Open the nth FT4222H device with custom configuration.
    pub async fn open_nth_with_config(index: usize, config: SpiConfig) -> Result<Self> {
        let devices: Vec<_> = nusb::list_devices()
            .await
            .map_err(|e| Ft4222Error::OpenFailed(e.to_string()))?
            .filter(|d| d.vendor_id() == FTDI_VID && d.product_id() == FT4222H_PID)
            .collect();

        let device_info = devices.get(index).ok_or(Ft4222Error::DeviceNotFound)?;
        Self::open_device(device_info, config).await
    }

    /// List all connected FT4222H devices.
    pub async fn list_devices() -> Result<Vec<Ft4222DeviceInfo>> {
        let devices: Vec<_> = nusb::list_devices()
            .await
            .map_err(|e| Ft4222Error::OpenFailed(e.to_string()))?
            .filter(|d| d.vendor_id() == FTDI_VID && d.product_id() == FT4222H_PID)
            .map(|d| Ft4222DeviceInfo {
                bus_id: d.bus_id().to_string(),
                address: d.device_address(),
            })
            .collect();

        Ok(devices)
    }
}

// ---------------------------------------------------------------------------
// WASM-only methods (WebUSB device picker, async open, shutdown)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl Ft4222 {
    /// Request an FT4222H device via the WebUSB permission prompt.
    ///
    /// This must be called from a user gesture (e.g., button click) in the browser.
    #[cfg(target_arch = "wasm32")]
    pub async fn request_device() -> Result<nusb::DeviceInfo> {
        log::info!("Requesting FT4222H device via WebUSB picker...");

        let selector = nusb::DeviceSelector::all().with_vid_pid(FTDI_VID, FT4222H_PID);
        let device_info = nusb::request_device(&[selector])
            .await
            .map_err(|e| Ft4222Error::OpenFailed(format!("WebUSB request failed: {e}")))?
            .ok_or(Ft4222Error::DeviceNotFound)?;

        log::info!(
            "FT4222H device selected: VID={:04X} PID={:04X}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        Ok(device_info)
    }

    /// Open an FT4222H device from a WebUSB-selected `DeviceInfo`.
    pub async fn open(device_info: nusb::DeviceInfo, config: SpiConfig) -> Result<Self> {
        Self::open_device(&device_info, config).await
    }

    /// Shutdown the device and drain pending endpoint state.
    pub async fn shutdown(&mut self) {
        let _ = self.set_io_lines(1).await;
        let _ = self.flush().await;

        if let Some(out_ep) = self.out_endpoint.as_mut() {
            #[cfg(not(target_arch = "wasm32"))]
            out_ep.cancel_all();
            while out_ep.pending() > 0 {
                let _ = ep_wait!(out_ep, Duration::from_secs(1));
            }
        }

        if let Some(in_ep) = self.in_endpoint.as_mut() {
            #[cfg(not(target_arch = "wasm32"))]
            in_ep.cancel_all();
            while in_ep.pending() > 0 {
                let _ = ep_wait!(in_ep, Duration::from_secs(1));
            }
        }

        log::info!("FT4222H shutdown complete");
    }
}

// ---------------------------------------------------------------------------
// Shared methods (async on every target)
// ---------------------------------------------------------------------------

#[cfg_attr(any(), allow(dead_code))]
impl Ft4222 {
    /// Open a specific FT4222H device.
    async fn open_device(device_info: &nusb::DeviceInfo, config: SpiConfig) -> Result<Self> {
        log::info!(
            "Opening FT4222H device VID={:04X} PID={:04X}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        let device =
            nusb_await!(device_info.open()).map_err(|e| Ft4222Error::OpenFailed(e.to_string()))?;

        log::debug!(
            "Device: VID={:04X} PID={:04X}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        let config_desc = device
            .active_configuration()
            .map_err(|e| Ft4222Error::OpenFailed(format!("Failed to get config: {}", e)))?;

        let mut spi_interface = None;
        let mut in_ep = None;
        let mut out_ep = None;

        for iface in config_desc.interface_alt_settings() {
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

        let interface = nusb_await!(device.claim_interface(iface_num))
            .map_err(|e| Ft4222Error::ClaimFailed(e.to_string()))?;

        let clock_config = find_clock_config(config.speed_khz);
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
            io_lines: 1,
            out_endpoint: None,
            in_endpoint: None,
            in_max_packet_size: 0,
        };

        let out_endpoint = ft4222
            .interface
            .endpoint::<Bulk, Out>(ft4222.out_ep)
            .map_err(|e| Ft4222Error::OpenFailed(format!("Failed to claim OUT endpoint: {e}")))?;
        let in_endpoint = ft4222
            .interface
            .endpoint::<Bulk, In>(ft4222.in_ep)
            .map_err(|e| Ft4222Error::OpenFailed(format!("Failed to claim IN endpoint: {e}")))?;
        ft4222.in_max_packet_size = in_endpoint.max_packet_size();
        ft4222.out_endpoint = Some(out_endpoint);
        ft4222.in_endpoint = Some(in_endpoint);

        ft4222.init().await?;
        Ok(ft4222)
    }

    /// Initialize the FT4222H for SPI master mode.
    async fn init(&mut self) -> Result<()> {
        let (chip_version, version2, version3) = self.get_version().await?;
        log::info!(
            "FT4222H version: chip=0x{:08X} (0x{:08X} 0x{:08X})",
            chip_version,
            version2,
            version3
        );

        let channels = self.get_num_channels().await?;
        log::debug!("FT4222H channels: {}", channels);

        if self.config.cs >= channels {
            return Err(Ft4222Error::InvalidParameter(format!(
                "CS{} not available (device has {} channels)",
                self.config.cs, channels
            )));
        }

        self.reset().await?;
        self.set_sys_clock(self.clock_config.sys_clock).await?;
        self.configure_spi_master().await?;

        log::info!(
            "FT4222H configured: SPI clock = {} kHz, CS = {}, I/O mode = {:?}",
            self.clock_config.spi_clock_khz(),
            self.config.cs,
            self.config.io_mode
        );

        Ok(())
    }

    /// Get device version information (matching flashprog's `ft4222_get_version`).
    async fn get_version(&self) -> Result<(u32, u32, u32)> {
        let data = nusb_await!(self.interface.control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: FT4222_INFO_REQUEST,
                value: FT4222_GET_VERSION,
                index: self.control_index as u16,
                length: 12,
            },
            Duration::from_secs(5),
        ))
        .map_err(|e| Ft4222Error::TransferFailed(format!("Failed to get version: {}", e)))?;

        if data.len() < 12 {
            return Err(Ft4222Error::InvalidResponse(format!(
                "Version response too short: {} < 12",
                data.len()
            )));
        }

        let chip_version = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let version2 = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let version3 = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

        Ok((chip_version, version2, version3))
    }

    /// Get the number of CS channels available.
    async fn get_num_channels(&self) -> Result<u8> {
        let data = nusb_await!(self.interface.control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: FT4222_INFO_REQUEST,
                value: FT4222_GET_CONFIG,
                index: self.control_index as u16,
                length: 13,
            },
            Duration::from_secs(5),
        ))
        .map_err(|e| Ft4222Error::TransferFailed(format!("Failed to get config: {}", e)))?;

        if data.is_empty() {
            return Err(Ft4222Error::InvalidResponse(
                "Empty response for config".into(),
            ));
        }

        let channels = match data[0] {
            0 => 1,
            1 => 3,
            2 => 4,
            3 => 1,
            mode => {
                return Err(Ft4222Error::InvalidResponse(format!(
                    "Unknown mode byte: 0x{:02x}",
                    mode
                )));
            }
        };

        log::debug!("FT4222H mode: {}, channels: {}", data[0], channels);
        Ok(channels)
    }

    /// Reset the device (matching flashprog's `ft4222_reset`).
    async fn reset(&self) -> Result<()> {
        self.control_out_with_index(FT4222_RESET_REQUEST, FT4222_RESET_SIO, 0, &[])
            .await?;
        self.flush().await?;
        log::debug!("FT4222H reset complete");
        Ok(())
    }

    /// Flush device buffers.
    async fn flush(&self) -> Result<()> {
        for _ in 0..6 {
            if let Err(e) = self
                .control_out(FT4222_RESET_REQUEST, FT4222_OUTPUT_FLUSH, &[])
                .await
            {
                log::warn!("FT4222 output flush failed: {}", e);
                break;
            }
        }

        if let Err(e) = self
            .control_out(FT4222_RESET_REQUEST, FT4222_INPUT_FLUSH, &[])
            .await
        {
            log::warn!("FT4222 input flush failed: {}", e);
        }

        Ok(())
    }

    /// Set the FT4222 system clock.
    async fn set_sys_clock(&self, clock: SystemClock) -> Result<()> {
        self.config_request(FT4222_SET_CLOCK, clock.index() as u8)
            .await?;
        log::debug!("Set system clock to {} MHz", clock.to_khz() / 1000);
        Ok(())
    }

    /// Configure the FT4222 for SPI master mode.
    async fn configure_spi_master(&mut self) -> Result<()> {
        let cs = self.config.cs;

        self.config_request(FT4222_SPI_RESET_TRANSACTION, cs)
            .await?;
        self.io_lines = 1;
        self.config_request(FT4222_SPI_SET_IO_LINES, 1).await?;
        self.config_request(
            FT4222_SPI_SET_CLK_DIV,
            self.clock_config.divisor.value() as u8,
        )
        .await?;
        self.config_request(FT4222_SPI_SET_CLK_IDLE, FT4222_CLK_IDLE_LOW)
            .await?;
        self.config_request(FT4222_SPI_SET_CAPTURE, FT4222_CLK_CAPTURE_LEADING)
            .await?;
        self.config_request(FT4222_SPI_SET_CS_ACTIVE, FT4222_CS_ACTIVE_LOW)
            .await?;
        self.config_request(FT4222_SPI_SET_CS_MASK, 1 << cs).await?;
        self.config_request(FT4222_SET_MODE, FT4222_MODE_SPI_MASTER)
            .await?;

        Ok(())
    }

    /// Change the active number of SPI I/O lines.
    async fn set_io_lines(&mut self, lines: u8) -> Result<()> {
        if lines != self.io_lines {
            self.config_request(FT4222_SPI_SET_IO_LINES, lines).await?;
            self.config_request(FT4222_SPI_RESET, FT4222_SPI_RESET_LINE_NUM)
                .await?;
            self.io_lines = lines;
            log::trace!("Set I/O lines to {}", lines);
        }
        Ok(())
    }

    /// Send a control OUT transfer with the default `control_index`.
    async fn control_out(&self, request: u8, value: u16, data: &[u8]) -> Result<()> {
        self.control_out_with_index(request, value, self.control_index as u16, data)
            .await
    }

    /// Send a control OUT transfer with an explicit index.
    async fn control_out_with_index(
        &self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<()> {
        nusb_await!(self.interface.control_out(
            ControlOut {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request,
                value,
                index,
                data,
            },
            Duration::from_secs(5),
        ))
        .map_err(|e| Ft4222Error::TransferFailed(format!("Control transfer failed: {}", e)))?;

        Ok(())
    }

    /// Send an FT4222 config request.
    async fn config_request(&self, cmd: u8, data: u8) -> Result<()> {
        let value = ((data as u16) << 8) | (cmd as u16);
        nusb_await!(self.interface.control_out(
            ControlOut {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: FT4222_CONFIG_REQUEST,
                value,
                index: self.control_index as u16,
                data: &[],
            },
            Duration::from_secs(5),
        ))
        .map_err(|e| Ft4222Error::TransferFailed(format!("Control transfer failed: {}", e)))?;

        Ok(())
    }

    /// Write data to the bulk OUT endpoint.
    async fn bulk_write(&mut self, data: &[u8]) -> Result<()> {
        let out_ep = self
            .out_endpoint
            .as_mut()
            .ok_or_else(|| Ft4222Error::TransferFailed("OUT endpoint missing".into()))?;

        if data.is_empty() {
            out_ep.submit(Buffer::new(0));
            let completion =
                ep_wait!(out_ep, Duration::from_secs(30)).ok_or(Ft4222Error::Timeout)?;
            completion
                .status
                .map_err(|e| Ft4222Error::TransferFailed(format!("Empty packet failed: {}", e)))?;
            log::trace!("Bulk write empty packet (CS deassert)");
            return Ok(());
        }

        const MAX_CHUNK: usize = 2048;
        let mut offset = 0;

        while offset < data.len() {
            let chunk_len = std::cmp::min(MAX_CHUNK, data.len() - offset);
            let chunk = &data[offset..offset + chunk_len];

            let mut out_buf = Buffer::new(chunk_len);
            out_buf.extend_from_slice(chunk);
            out_ep.submit(out_buf);

            let completion =
                ep_wait!(out_ep, Duration::from_secs(30)).ok_or(Ft4222Error::Timeout)?;
            completion.status.map_err(|e| {
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

    /// Read data from the bulk IN endpoint.
    async fn bulk_read(&mut self, len: usize) -> Result<Vec<u8>> {
        let in_ep = self
            .in_endpoint
            .as_mut()
            .ok_or_else(|| Ft4222Error::TransferFailed("IN endpoint missing".into()))?;

        let max_packet_size = in_ep.max_packet_size();
        let mut result = Vec::new();
        let mut remaining = len;

        while remaining > 0 {
            let request_len = std::cmp::min(remaining + MODEM_STATUS_SIZE, READ_BUFFER_SIZE);
            let aligned_len = request_len.div_ceil(max_packet_size) * max_packet_size;

            let mut in_buf = Buffer::new(aligned_len);
            in_buf.set_requested_len(aligned_len);
            in_ep.submit(in_buf);

            let completion =
                ep_wait!(in_ep, Duration::from_secs(30)).ok_or(Ft4222Error::Timeout)?;
            completion
                .status
                .map_err(|e| Ft4222Error::TransferFailed(format!("Bulk read failed: {}", e)))?;

            let data = &completion.buffer[..completion.actual_len];
            if data.len() < MODEM_STATUS_SIZE {
                return Err(Ft4222Error::InvalidResponse("Response too short".into()));
            }

            let payload = &data[MODEM_STATUS_SIZE..];
            let to_copy = std::cmp::min(payload.len(), remaining);
            result.extend_from_slice(&payload[..to_copy]);
            remaining -= to_copy;
        }

        log::trace!("Bulk read {} bytes", result.len());
        Ok(result)
    }

    /// Perform a single-I/O SPI transfer using pipelined USB transfers.
    async fn spi_transfer_single(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        self.set_io_lines(1).await?;

        let total_len = write_data.len() + read_len;
        if total_len == 0 {
            return Ok(Vec::new());
        }

        let max_packet_size = self.in_max_packet_size;
        let out_ep = self
            .out_endpoint
            .as_mut()
            .ok_or_else(|| Ft4222Error::TransferFailed("OUT endpoint missing".into()))?;
        let in_ep = self
            .in_endpoint
            .as_mut()
            .ok_or_else(|| Ft4222Error::TransferFailed("IN endpoint missing".into()))?;

        let mut write_buf = Buffer::new(write_data.len());
        write_buf.extend_from_slice(write_data);
        out_ep.submit(write_buf);

        if read_len > 0 {
            let mut dummy_buf = Buffer::new(read_len);
            dummy_buf.extend_fill(read_len, 0xff);
            out_ep.submit(dummy_buf);
        }

        out_ep.submit(Buffer::new(0));

        let mut raw = Vec::<u8>::with_capacity(total_len);
        let mut real_bytes = 0usize;

        while real_bytes < total_len {
            let remaining = total_len - real_bytes;
            let bytes_per_packet = max_packet_size - MODEM_STATUS_SIZE;
            let packets_needed = remaining.div_ceil(bytes_per_packet);
            let request_len = (packets_needed * max_packet_size).min(READ_BUFFER_SIZE);

            let mut in_buf = Buffer::new(request_len);
            in_buf.set_requested_len(request_len);
            in_ep.submit(in_buf);

            let completion =
                ep_wait!(in_ep, Duration::from_secs(30)).ok_or(Ft4222Error::Timeout)?;
            completion
                .status
                .map_err(|e| Ft4222Error::TransferFailed(format!("Bulk read failed: {e}")))?;

            let data = &completion.buffer[..completion.actual_len];
            for packet in data.chunks(max_packet_size) {
                if packet.len() <= MODEM_STATUS_SIZE {
                    continue;
                }
                let payload = &packet[MODEM_STATUS_SIZE..];
                let to_copy = payload.len().min(total_len - real_bytes);
                raw.extend_from_slice(&payload[..to_copy]);
                real_bytes += to_copy;
                if real_bytes >= total_len {
                    break;
                }
            }
        }

        let expected_out = if read_len > 0 { 3 } else { 2 };
        for _ in 0..expected_out {
            let completion =
                ep_wait!(out_ep, Duration::from_secs(30)).ok_or(Ft4222Error::Timeout)?;
            completion
                .status
                .map_err(|e| Ft4222Error::TransferFailed(format!("Bulk write failed: {e}")))?;
        }

        log::trace!(
            "SPI transfer: wrote {} bytes, read {} bytes (got {} payload bytes)",
            write_data.len(),
            read_len,
            raw.len()
        );

        if raw.len() >= total_len {
            Ok(raw[write_data.len()..].to_vec())
        } else {
            Err(Ft4222Error::InvalidResponse(format!(
                "Expected {} bytes, got {}",
                total_len,
                raw.len()
            )))
        }
    }

    /// Execute a command using dual, quad, or QPI framing.
    async fn execute_multi_io(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        let io_width = match cmd.io_mode {
            CoreIoMode::Single => 1,
            CoreIoMode::DualOut | CoreIoMode::DualIo => 2,
            CoreIoMode::QuadOut | CoreIoMode::QuadIo | CoreIoMode::Qpi => 4,
        };

        let opcode = [cmd.opcode];
        let addr_width = cmd.address_width.bytes() as usize;
        let mut addr_bytes = [0u8; 4];
        if let Some(addr) = cmd.address {
            cmd.address_width.encode(addr, &mut addr_bytes);
        }
        let addr = &addr_bytes[..addr_width];
        let dummy_bytes = (cmd.dummy_cycles as usize * io_width).div_ceil(8);
        let uses_mode_byte = matches!(
            cmd.io_mode,
            CoreIoMode::DualIo | CoreIoMode::QuadIo | CoreIoMode::Qpi
        );
        let mode_byte_len = usize::from(uses_mode_byte && dummy_bytes > 0);
        let high_z_bytes = dummy_bytes.saturating_sub(mode_byte_len);

        let (single, multi) = match cmd.io_mode {
            CoreIoMode::Single => (opcode.to_vec(), Vec::new()),
            CoreIoMode::DualOut | CoreIoMode::QuadOut => {
                let mut single = Vec::with_capacity(1 + addr_width + cmd.write_data.len());
                single.extend_from_slice(&opcode);
                single.extend_from_slice(addr);
                single.extend_from_slice(cmd.write_data);
                (single, Vec::new())
            }
            CoreIoMode::DualIo | CoreIoMode::QuadIo => {
                let mut multi =
                    Vec::with_capacity(addr_width + mode_byte_len + cmd.write_data.len());
                multi.extend_from_slice(addr);
                if mode_byte_len != 0 {
                    multi.push(0xff);
                }
                multi.extend_from_slice(cmd.write_data);
                (opcode.to_vec(), multi)
            }
            CoreIoMode::Qpi => {
                let mut multi =
                    Vec::with_capacity(1 + addr_width + mode_byte_len + cmd.write_data.len());
                multi.extend_from_slice(&opcode);
                multi.extend_from_slice(addr);
                if mode_byte_len != 0 {
                    multi.push(0xff);
                }
                multi.extend_from_slice(cmd.write_data);
                (Vec::new(), multi)
            }
        };

        let read_total = high_z_bytes + cmd.read_buf.len();
        let data = self
            .spi_transfer_multi(&single, &multi, read_total, io_width as u8)
            .await
            .map_err(|_| CoreError::ProgrammerError)?;
        if data.len() < read_total {
            return Err(CoreError::ProgrammerError);
        }
        cmd.read_buf
            .copy_from_slice(&data[high_z_bytes..high_z_bytes + cmd.read_buf.len()]);
        Ok(())
    }

    /// Perform a multi-I/O SPI transfer (half duplex).
    #[allow(dead_code)]
    async fn spi_transfer_multi(
        &mut self,
        single_data: &[u8],
        multi_write_data: &[u8],
        multi_read_len: usize,
        io_lines: u8,
    ) -> Result<Vec<u8>> {
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

        self.set_io_lines(io_lines).await?;

        let mut header = [0u8; MULTI_IO_HEADER_SIZE];
        header[0] = MULTI_IO_MAGIC | (single_data.len() as u8 & 0x0F);
        header[1] = ((multi_write_data.len() >> 8) & 0xFF) as u8;
        header[2] = (multi_write_data.len() & 0xFF) as u8;
        header[3] = ((multi_read_len >> 8) & 0xFF) as u8;
        header[4] = (multi_read_len & 0xFF) as u8;

        let mut out_buf =
            Vec::with_capacity(MULTI_IO_HEADER_SIZE + single_data.len() + multi_write_data.len());
        out_buf.extend_from_slice(&header);
        out_buf.extend_from_slice(single_data);
        out_buf.extend_from_slice(multi_write_data);

        self.bulk_write(&out_buf).await?;
        self.bulk_write(&[]).await?;

        if multi_read_len > 0 {
            self.bulk_read(multi_read_len).await
        } else {
            Ok(Vec::new())
        }
    }

    /// Get the current SPI configuration.
    pub fn config(&self) -> &SpiConfig {
        &self.config
    }

    /// Get the actual SPI clock speed in kHz.
    pub fn actual_speed_khz(&self) -> u32 {
        self.clock_config.spi_clock_khz()
    }
}

impl SpiMaster for Ft4222 {
    fn features(&self) -> SpiFeatures {
        let mut features = SpiFeatures::FOUR_BYTE_ADDR;
        match self.config.io_mode {
            IoMode::Single => {}
            IoMode::Dual => features |= SpiFeatures::DUAL_IN | SpiFeatures::DUAL_IO,
            IoMode::Quad => {
                features |= SpiFeatures::DUAL_IN
                    | SpiFeatures::DUAL_IO
                    | SpiFeatures::QUAD_IN
                    | SpiFeatures::QUAD_IO
                    | SpiFeatures::QPI;
            }
        }
        features
    }

    fn max_read_len(&self) -> usize {
        // The 16-bit FT4222 length includes high-Z dummy bytes.
        65530
    }

    fn max_write_len(&self) -> usize {
        // Reserve space for protocol framing, matching flashprog.
        65530
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        if cmd.io_mode != CoreIoMode::Single {
            return self.execute_multi_io(cmd).await;
        }

        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];
        cmd.encode_header(&mut write_data);
        write_data[header_len..].copy_from_slice(cmd.write_data);

        let read_len = cmd.read_buf.len();
        if read_len == 0 {
            self.spi_transfer_single(&write_data, 0)
                .await
                .map_err(|_| CoreError::ProgrammerError)?;
            return Ok(());
        }

        let result = self
            .spi_transfer_single(&write_data, read_len)
            .await
            .map_err(|_| CoreError::ProgrammerError)?;

        cmd.read_buf.copy_from_slice(&result[..read_len]);
        Ok(())
    }

    async fn delay_us(&mut self, us: u32) {
        if us == 0 {
            return;
        }

        #[cfg(not(target_arch = "wasm32"))]
        const SPIN_THRESHOLD_US: u32 = 100;
        #[cfg(target_arch = "wasm32")]
        const SPIN_THRESHOLD_US: u32 = 1_000;

        if us < SPIN_THRESHOLD_US {
            let deadline = std::time::Instant::now() + Duration::from_micros(us as u64);
            while std::time::Instant::now() < deadline {
                std::hint::spin_loop();
            }
            return;
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            std::thread::sleep(Duration::from_micros(us as u64));
        }

        #[cfg(target_arch = "wasm32")]
        {
            let delay_ms = ((us as f64) / 1000.0).ceil() as i32;
            if delay_ms > 0 {
                let promise = js_sys::Promise::new(&mut |resolve, _| {
                    let window = web_sys::window().unwrap();
                    window
                        .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, delay_ms)
                        .unwrap();
                });
                let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
            }
        }
    }
}

/// Information about a connected FT4222H device.
#[derive(Debug, Clone)]
pub struct Ft4222DeviceInfo {
    /// USB bus identifier (platform-defined; integer string on Linux)
    pub bus_id: String,
    /// USB device address.
    pub address: u8,
}

impl std::fmt::Display for Ft4222DeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FT4222H at bus {} address {}", self.bus_id, self.address)
    }
}

/// Parse programmer options for FT4222.
///
/// Supported options:
/// - `spispeed=<khz>`: Target SPI clock speed in kHz (default: 10000)
/// - `cs=<0-3>`: Which chip select to use (default: 0)
/// - `iomode=<single|dual|quad>`: I/O mode (default: single)
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

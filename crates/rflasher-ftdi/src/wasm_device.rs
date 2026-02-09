//! FTDI MPSSE device implementation using nusb (WASM/WebUSB backend)
//!
//! This module provides the `Ftdi` struct using raw USB bulk transfers via
//! `nusb` and the `maybe_async` crate. It speaks the FTDI MPSSE protocol
//! directly to the hardware, enabling both native sync and WASM async modes
//! from a single codebase.
//!
//! The MPSSE (Multi-Protocol Synchronous Serial Engine) is configured via
//! USB control transfers and bulk endpoints. This avoids the need for
//! `rs-ftdi` or `libftdi1`, making it suitable for WebUSB in the browser.

use std::time::Duration;

use maybe_async::maybe_async;
use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::Endpoint;
#[cfg(feature = "is_sync")]
use nusb::MaybeFuture;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};

use crate::protocol::*;
use crate::wasm_error::{FtdiError, Result};

// ---------------------------------------------------------------------------
// Platform-specific endpoint wait macros
// ---------------------------------------------------------------------------

/// Wait for the next completion on an endpoint, with timeout.
/// In sync mode: blocks with the given timeout.
/// In async mode: awaits indefinitely.
macro_rules! ep_wait {
    ($ep:expr, $timeout:expr) => {{
        #[cfg(feature = "is_sync")]
        {
            $ep.wait_next_complete($timeout)
        }
        #[cfg(not(feature = "is_sync"))]
        {
            Some($ep.next_complete().await)
        }
    }};
}

// ---------------------------------------------------------------------------
// FTDI USB constants for MPSSE configuration via control transfers
// ---------------------------------------------------------------------------

// FTDI USB request types
const FTDI_REQUEST_RESET: u8 = 0x00;
const FTDI_REQUEST_SET_BAUDRATE: u8 = 0x03;
const FTDI_REQUEST_SET_BITMODE: u8 = 0x0B;
const FTDI_REQUEST_SET_LATENCY: u8 = 0x09;

// FTDI reset values
const FTDI_RESET_SIO: u16 = 0;

// FTDI bitmode values
const FTDI_BITMODE_MPSSE: u8 = 0x02;

// FTDI default endpoints (interface-dependent)
const FTDI_WRITE_EP_BASE: u8 = 0x02;
const FTDI_READ_EP_BASE: u8 = 0x81;

// ---------------------------------------------------------------------------
// FTDI device struct
// ---------------------------------------------------------------------------

/// FTDI MPSSE programmer (WASM/WebUSB backend)
///
/// This struct represents a connection to an FTDI device using the MPSSE
/// engine for SPI communication. It uses raw USB bulk transfers via `nusb`
/// and supports both native sync and WASM async modes via `maybe_async`.
pub struct Ftdi {
    /// USB interface handle (used for control transfers and keeping the claim alive)
    interface: nusb::Interface,
    /// Bulk OUT endpoint for writes
    out_ep: Endpoint<Bulk, Out>,
    /// Bulk IN endpoint for reads
    in_ep: Endpoint<Bulk, In>,
    /// Current CS bits state
    cs_bits: u8,
    /// Auxiliary bits
    aux_bits: u8,
    /// Pin direction
    pindir: u8,
}

// ---------------------------------------------------------------------------
// Native-only methods (device enumeration, Drop)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "native", not(feature = "wasm")))]
impl Ftdi {
    /// Open an FTDI device with the given configuration
    pub fn open(config: &FtdiConfig) -> Result<Self> {
        log::info!(
            "Opening FTDI {} channel {} (nusb backend)",
            config.device_type.name(),
            config.interface.letter()
        );

        let vid = config.device_type.vendor_id();
        let pid = config.device_type.product_id();

        log::debug!("Looking for FTDI device VID={:04X} PID={:04X}", vid, pid);

        let device_info = nusb::list_devices()
            .wait()
            .map_err(|e| FtdiError::UsbError(e.to_string()))?
            .find(|d| d.vendor_id() == vid && d.product_id() == pid)
            .ok_or(FtdiError::DeviceNotFound)?;

        let device = device_info
            .open()
            .wait()
            .map_err(|e| FtdiError::OpenFailed(e.to_string()))?;

        let iface_num = config.interface.index();
        let interface = device
            .claim_interface(iface_num)
            .wait()
            .map_err(|e| FtdiError::ClaimFailed(e.to_string()))?;

        // Endpoint addresses depend on the interface number
        let write_ep = FTDI_WRITE_EP_BASE + iface_num * 2;
        let read_ep = FTDI_READ_EP_BASE + iface_num * 2;

        let out_ep = interface
            .endpoint::<Bulk, Out>(write_ep)
            .map_err(|e| FtdiError::ClaimFailed(e.to_string()))?;
        let in_ep = interface
            .endpoint::<Bulk, In>(read_ep)
            .map_err(|e| FtdiError::ClaimFailed(e.to_string()))?;

        let mut ftdi = Ftdi {
            interface,
            out_ep,
            in_ep,
            cs_bits: config.cs_bits,
            aux_bits: config.aux_bits,
            pindir: config.pindir,
        };

        ftdi.setup_mpsse(config)?;

        log::info!(
            "FTDI configured for SPI at {:.2} MHz (nusb backend)",
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
}

// Drop implementation only for sync mode (async requires explicit shutdown)
#[cfg(feature = "is_sync")]
impl Drop for Ftdi {
    fn drop(&mut self) {
        // Release I/O pins on close
        if let Err(e) = self.release_pins() {
            log::warn!("Failed to release pins on close: {}", e);
        }
        // Drain pending transfers
        self.drain_all_pending();
    }
}

// ---------------------------------------------------------------------------
// WASM-only methods (WebUSB device picker, async open, shutdown)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "wasm", not(feature = "is_sync")))]
impl Ftdi {
    /// Request an FTDI device via the WebUSB permission prompt
    ///
    /// This must be called from a user gesture (e.g., button click) in the browser.
    /// It shows the browser's device picker filtered to all supported FTDI devices.
    #[cfg(target_arch = "wasm32")]
    pub async fn request_device() -> Result<nusb::DeviceInfo> {
        use wasm_bindgen::JsCast;
        use wasm_bindgen_futures::JsFuture;
        use web_sys::{UsbDevice, UsbDeviceFilter, UsbDeviceRequestOptions};

        let usb = web_sys::window()
            .ok_or(FtdiError::DeviceNotFound)?
            .navigator()
            .usb();

        // Create filters for all supported FTDI devices
        let filters = js_sys::Array::new();
        for supported in SUPPORTED_DEVICES {
            let filter = UsbDeviceFilter::new();
            filter.set_vendor_id(supported.vendor_id);
            filter.set_product_id(supported.product_id);
            filters.push(&filter);
        }

        let options = UsbDeviceRequestOptions::new(&filters);

        log::info!("Requesting FTDI device via WebUSB picker...");

        let device_promise = usb.request_device(&options);
        let device_js = JsFuture::from(device_promise)
            .await
            .map_err(|e| FtdiError::OpenFailed(format!("WebUSB request failed: {:?}", e)))?;

        let device: UsbDevice = device_js
            .dyn_into()
            .map_err(|_| FtdiError::OpenFailed("Failed to get USB device".to_string()))?;

        log::info!(
            "FTDI device selected: VID={:04X} PID={:04X}",
            device.vendor_id(),
            device.product_id()
        );

        let device_info = nusb::device_info_from_webusb(device)
            .await
            .map_err(|e| FtdiError::OpenFailed(format!("Failed to get device info: {}", e)))?;

        Ok(device_info)
    }

    /// Open an FTDI device from a DeviceInfo with the given configuration
    pub async fn open(device_info: nusb::DeviceInfo, config: &FtdiConfig) -> Result<Self> {
        log::info!(
            "Opening FTDI {} channel {} VID={:04X} PID={:04X} (WebUSB)",
            config.device_type.name(),
            config.interface.letter(),
            device_info.vendor_id(),
            device_info.product_id()
        );

        let device = device_info
            .open()
            .await
            .map_err(|e| FtdiError::OpenFailed(e.to_string()))?;

        let iface_num = config.interface.index();
        let interface = device
            .claim_interface(iface_num)
            .await
            .map_err(|e| FtdiError::ClaimFailed(e.to_string()))?;

        // Endpoint addresses depend on the interface number
        let write_ep = FTDI_WRITE_EP_BASE + iface_num * 2;
        let read_ep = FTDI_READ_EP_BASE + iface_num * 2;

        let out_ep = interface
            .endpoint::<Bulk, Out>(write_ep)
            .map_err(|e| FtdiError::ClaimFailed(e.to_string()))?;
        let in_ep = interface
            .endpoint::<Bulk, In>(read_ep)
            .map_err(|e| FtdiError::ClaimFailed(e.to_string()))?;

        let mut ftdi = Ftdi {
            interface,
            out_ep,
            in_ep,
            cs_bits: config.cs_bits,
            aux_bits: config.aux_bits,
            pindir: config.pindir,
        };

        ftdi.setup_mpsse(config).await?;

        log::info!(
            "FTDI configured for SPI at {:.2} MHz (WebUSB)",
            config.spi_clock_mhz()
        );

        Ok(ftdi)
    }

    /// Shutdown: release pins and drain transfers (WASM equivalent of Drop)
    pub async fn shutdown(&mut self) {
        if let Err(e) = self.release_pins().await {
            log::warn!("Failed to release pins on shutdown: {}", e);
        }
        self.drain_all_pending().await;
        log::info!("FTDI shutdown complete");
    }
}

// ---------------------------------------------------------------------------
// Shared methods (sync or async via maybe_async)
// ---------------------------------------------------------------------------

impl Ftdi {
    /// Set up the MPSSE engine via USB control transfers and MPSSE commands
    #[maybe_async]
    async fn setup_mpsse(&mut self, config: &FtdiConfig) -> Result<()> {
        let iface_idx = config.usb_interface_index();

        // Reset the device
        self.control_out(FTDI_REQUEST_RESET, FTDI_RESET_SIO, iface_idx)
            .await?;

        // Set latency timer to 2ms for best performance
        self.control_out(FTDI_REQUEST_SET_LATENCY, 2, iface_idx)
            .await?;

        // Set MPSSE bitmode
        let bitmode_value = (FTDI_BITMODE_MPSSE as u16) << 8;
        self.control_out(FTDI_REQUEST_SET_BITMODE, bitmode_value, iface_idx)
            .await?;

        // Purge buffers by reading any stale data
        self.purge_rx().await;

        // Send MPSSE initialization commands
        self.init_mpsse(config).await?;

        Ok(())
    }

    /// Send a vendor control OUT transfer
    #[maybe_async]
    async fn control_out(&self, request: u8, value: u16, index: u16) -> Result<()> {
        use nusb::transfer::{ControlType, Recipient};

        let control = nusb::transfer::ControlOut {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request,
            value,
            index,
            data: &[],
        };

        #[cfg(feature = "is_sync")]
        {
            use nusb::MaybeFuture;
            self.interface
                .control_out(control, Duration::from_secs(5))
                .wait()
                .map_err(|e| {
                    FtdiError::ConfigFailed(format!("Control transfer failed: {}", e))
                })?;
        }
        #[cfg(not(feature = "is_sync"))]
        {
            self.interface
                .control_out(control, Duration::from_secs(5))
                .await
                .map_err(|e| {
                    FtdiError::ConfigFailed(format!("Control transfer failed: {}", e))
                })?;
        }

        Ok(())
    }

    /// Purge any stale data from the RX buffer
    #[maybe_async]
    async fn purge_rx(&mut self) {
        // Try to read any leftover data (non-blocking / short timeout)
        let buf = Buffer::new(512);
        self.in_ep.submit(buf);

        #[cfg(feature = "is_sync")]
        {
            // Short timeout to drain
            if let Some(completion) = self.in_ep.wait_next_complete(Duration::from_millis(50)) {
                log::trace!("Purged {} bytes from RX", completion.actual_len);
            }
        }
        #[cfg(not(feature = "is_sync"))]
        {
            // Just cancel it
            self.in_ep.cancel_all();
            while self.in_ep.pending() > 0 {
                let _ = self.in_ep.next_complete().await;
            }
        }
    }

    /// Initialize the MPSSE engine with MPSSE commands
    #[maybe_async]
    async fn init_mpsse(&mut self, config: &FtdiConfig) -> Result<()> {
        let mut buf = Vec::with_capacity(32);

        // Disable divide-by-5 prescaler for 60 MHz base clock (H devices)
        if config.device_type.is_high_speed() {
            log::debug!("Disabling divide-by-5 prescaler for 60 MHz clock");
            buf.push(DIS_DIV_5);
        }

        // Set clock divisor
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

        self.usb_write(&buf).await?;

        Ok(())
    }

    /// Write data to the FTDI bulk OUT endpoint
    #[maybe_async]
    async fn usb_write(&mut self, data: &[u8]) -> Result<()> {
        let buf = Buffer::from(data.to_vec());
        self.out_ep.submit(buf);

        let completion = ep_wait!(self.out_ep, Duration::from_secs(5))
            .ok_or_else(|| FtdiError::TransferFailed("USB write timed out".into()))?;

        completion
            .status
            .map_err(|e| FtdiError::TransferFailed(e.to_string()))?;

        log::trace!("USB write {} bytes", data.len());
        Ok(())
    }

    /// Read data from the FTDI bulk IN endpoint
    ///
    /// FTDI devices prepend 2 modem status bytes to every read packet.
    /// This method strips those bytes and returns only the payload data.
    #[maybe_async]
    async fn usb_read(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut result = Vec::with_capacity(len);
        let max_packet_size = self.in_ep.max_packet_size();

        // Guard against unusable packet sizes (FTDI high-speed devices use 512)
        if max_packet_size <= 2 {
            return Err(FtdiError::ConfigFailed(format!(
                "USB max packet size {} is too small for FTDI (need > 2 for status bytes)",
                max_packet_size
            )));
        }

        // Limit retries to avoid infinite loops on devices returning only status bytes
        const MAX_EMPTY_READS: usize = 128;
        let mut empty_reads = 0;

        while result.len() < len {
            // Request enough data accounting for the 2-byte FTDI status header per packet
            let remaining = len - result.len();
            let payload_per_packet = max_packet_size - 2;
            let request_len = std::cmp::max(
                max_packet_size,
                remaining.div_ceil(payload_per_packet) * max_packet_size,
            );

            let buf = Buffer::new(request_len);
            self.in_ep.submit(buf);

            let completion = ep_wait!(self.in_ep, Duration::from_secs(5))
                .ok_or_else(|| FtdiError::TransferFailed("USB read timed out".into()))?;

            completion
                .status
                .map_err(|e| FtdiError::TransferFailed(e.to_string()))?;

            let received = &completion.buffer[..completion.actual_len];
            let prev_len = result.len();

            // Strip FTDI modem status bytes (2 bytes per max_packet_size chunk)
            let mut offset = 0;
            while offset < received.len() && result.len() < len {
                if received.len() - offset < 2 {
                    break;
                }
                // Skip the 2-byte status header
                let chunk_end = std::cmp::min(offset + max_packet_size, received.len());
                let payload = &received[offset + 2..chunk_end];
                let to_take = std::cmp::min(payload.len(), len - result.len());
                result.extend_from_slice(&payload[..to_take]);
                offset += max_packet_size;
            }

            // Track empty reads (packets with only status bytes and no payload)
            if result.len() == prev_len {
                empty_reads += 1;
                if empty_reads >= MAX_EMPTY_READS {
                    return Err(FtdiError::TransferFailed(format!(
                        "USB read stalled: received {} empty packets while waiting for {} bytes",
                        empty_reads,
                        len - result.len()
                    )));
                }
            } else {
                empty_reads = 0;
            }
        }

        log::trace!("USB read {} bytes (payload)", result.len());
        Ok(result)
    }

    /// Perform an SPI transfer via MPSSE commands
    #[maybe_async]
    async fn spi_transfer(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        let writecnt = write_data.len();
        let readcnt = read_len;

        // Validate lengths
        if writecnt > 65536 || readcnt > 65536 {
            return Err(FtdiError::TransferFailed(
                "Transfer length exceeds 64KB limit".to_string(),
            ));
        }

        // Build MPSSE command buffer
        let mut buf = Vec::with_capacity(FTDI_HW_BUFFER_SIZE);

        // Assert CS
        buf.push(SET_BITS_LOW);
        buf.push(self.aux_bits);
        buf.push(self.pindir);

        // Write command (opcode + address + data)
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
        self.usb_write(&buf).await?;

        // Read response if needed
        if readcnt > 0 {
            self.usb_read(readcnt).await
        } else {
            // Even with no read data, we may need to consume the SEND_IMMEDIATE response
            // FTDI sends back modem status bytes; drain them
            Ok(Vec::new())
        }
    }

    /// Release I/O pins (set all as inputs)
    #[maybe_async]
    async fn release_pins(&mut self) -> Result<()> {
        let buf = [SET_BITS_LOW, 0x00, 0x00];
        self.usb_write(&buf).await
    }

    /// Cancel and drain all pending transfers on both endpoints.
    #[maybe_async]
    async fn drain_all_pending(&mut self) {
        self.out_ep.cancel_all();
        while self.out_ep.pending() > 0 {
            let _ = ep_wait!(self.out_ep, Duration::from_secs(1));
        }
        self.in_ep.cancel_all();
        while self.in_ep.pending() > 0 {
            let _ = ep_wait!(self.in_ep, Duration::from_secs(1));
        }
    }
}

// ---------------------------------------------------------------------------
// SpiMaster trait implementation
// ---------------------------------------------------------------------------

#[maybe_async(AFIT)]
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

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
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
            .await
            .map_err(|_e| CoreError::ProgrammerError)?;

        // Copy read data back
        cmd.read_buf.copy_from_slice(&result);

        Ok(())
    }

    async fn delay_us(&mut self, us: u32) {
        if us > 0 {
            #[cfg(feature = "is_sync")]
            {
                std::thread::sleep(Duration::from_micros(us as u64));
            }

            #[cfg(all(feature = "wasm", not(feature = "is_sync")))]
            {
                let delay_ms = ((us as f64) / 1000.0).ceil() as i32;
                if delay_ms > 0 {
                    let promise = js_sys::Promise::new(&mut |resolve, _| {
                        let window = web_sys::window().unwrap();
                        window
                            .set_timeout_with_callback_and_timeout_and_arguments_0(
                                &resolve, delay_ms,
                            )
                            .unwrap();
                    });
                    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Option parsing (native only)
// ---------------------------------------------------------------------------

/// Parse programmer options from a string
///
/// Format: "type=<type>,port=<A|B|C|D>,divisor=<N>,serial=<serial>,gpiol0=<H|L|C>"
#[cfg(all(feature = "native", not(feature = "wasm")))]
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

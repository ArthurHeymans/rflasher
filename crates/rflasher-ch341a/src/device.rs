//! CH341A device implementation
//!
//! This module provides the main `Ch341a` struct that implements USB
//! communication with the CH341A programmer and the `SpiMaster` trait.
//!
//! Uses `maybe_async` to support both sync and async modes from a single
//! codebase:
//! - With `is_sync` feature (native CLI): all async is stripped, blocking USB
//! - Without `is_sync` (WASM): full async with WebUSB

#[cfg(feature = "is_sync")]
use std::time::Duration;

use maybe_async::maybe_async;
use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::Endpoint;
#[cfg(feature = "std")]
use nusb::MaybeFuture;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};

use crate::error::{Ch341aError, Result};
use crate::protocol::*;

// ---------------------------------------------------------------------------
// Platform-specific endpoint wait macros
// ---------------------------------------------------------------------------
// These macros provide a uniform interface over nusb's blocking (native)
// and async (WASM) completion APIs. Using macros avoids the borrow issues
// that arise with &mut Endpoint in free functions, since the macro expands
// inline and Rust can split borrows on struct fields.

/// Wait for the next completion on an endpoint, with timeout.
/// In sync mode: blocks with the given timeout.
/// In async mode: awaits indefinitely (timeout is ignored — nusb's async
/// API does not support timeouts natively; the caller should add an external
/// timeout wrapper if needed).
/// Returns `Option<Completion>`.
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

/// Try to get the next completion without blocking (non-blocking poll).
/// Returns `Option<Completion>`.
macro_rules! ep_try {
    ($ep:expr) => {{
        #[cfg(feature = "is_sync")]
        {
            $ep.wait_next_complete(Duration::ZERO)
        }
        #[cfg(not(feature = "is_sync"))]
        {
            use std::task::Poll;
            std::future::poll_fn(|cx| match $ep.poll_next_complete(cx) {
                Poll::Ready(c) => Poll::Ready(Some(c)),
                Poll::Pending => Poll::Ready(None),
            })
            .await
        }
    }};
}

// ---------------------------------------------------------------------------
// CH341A device struct
// ---------------------------------------------------------------------------

/// CH341A USB programmer
///
/// This struct represents a connection to a CH341A USB device and implements
/// the `SpiMaster` trait for communicating with SPI flash chips.
///
/// On native (with `is_sync`), all methods are synchronous and blocking.
/// On WASM (without `is_sync`), methods are async and use WebUSB.
pub struct Ch341a {
    /// USB interface (kept alive to maintain device claim on WASM)
    #[cfg(feature = "wasm")]
    _interface: nusb::Interface,
    /// Bulk OUT endpoint for writes
    out_ep: Endpoint<Bulk, Out>,
    /// Bulk IN endpoint for reads
    in_ep: Endpoint<Bulk, In>,
    /// Accumulated delay for CS handling
    stored_delay_us: u32,
}

// ---------------------------------------------------------------------------
// Native-only methods (device enumeration, Drop)
// ---------------------------------------------------------------------------

#[cfg(feature = "std")]
impl Ch341a {
    /// Open a CH341A device
    ///
    /// Searches for a CH341A device (VID:1a86 PID:5512) and opens it.
    /// Returns an error if no device is found or if the device cannot be opened.
    pub fn open() -> Result<Self> {
        Self::open_nth(0)
    }

    /// Open the nth CH341A device (0-indexed)
    ///
    /// Useful when multiple CH341A devices are connected.
    pub fn open_nth(index: usize) -> Result<Self> {
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| Ch341aError::OpenFailed(e.to_string()))?
            .filter(|d| d.vendor_id() == CH341A_USB_VENDOR && d.product_id() == CH341A_USB_PRODUCT)
            .collect();

        let device_info = devices.get(index).ok_or(Ch341aError::DeviceNotFound)?;

        log::info!(
            "Opening CH341A device at bus {} address {}",
            device_info.busnum(),
            device_info.device_address()
        );

        let device = device_info
            .open()
            .wait()
            .map_err(|e| Ch341aError::OpenFailed(e.to_string()))?;

        // Get device descriptor for version info
        let desc = device_info;
        log::debug!(
            "Device: VID={:04X} PID={:04X}",
            desc.vendor_id(),
            desc.product_id()
        );

        // Claim interface 0
        let interface = device
            .claim_interface(0)
            .wait()
            .map_err(|e| Ch341aError::ClaimFailed(e.to_string()))?;

        // Open bulk endpoints
        let out_ep = interface
            .endpoint::<Bulk, Out>(WRITE_EP)
            .map_err(|e| Ch341aError::ClaimFailed(e.to_string()))?;
        let in_ep = interface
            .endpoint::<Bulk, In>(READ_EP)
            .map_err(|e| Ch341aError::ClaimFailed(e.to_string()))?;

        let mut ch341a = Self {
            #[cfg(feature = "wasm")]
            _interface: interface,
            out_ep,
            in_ep,
            stored_delay_us: 0,
        };

        // Configure the device for SPI mode
        ch341a.configure()?;

        Ok(ch341a)
    }

    /// List all connected CH341A devices
    pub fn list_devices() -> Result<Vec<Ch341aDeviceInfo>> {
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| Ch341aError::OpenFailed(e.to_string()))?
            .filter(|d| d.vendor_id() == CH341A_USB_VENDOR && d.product_id() == CH341A_USB_PRODUCT)
            .map(|d| Ch341aDeviceInfo {
                bus: d.busnum(),
                address: d.device_address(),
            })
            .collect();

        Ok(devices)
    }
}

/// Information about a connected CH341A device
#[cfg(feature = "std")]
#[derive(Debug, Clone)]
pub struct Ch341aDeviceInfo {
    /// USB bus number
    pub bus: u8,
    /// USB device address
    pub address: u8,
}

#[cfg(feature = "std")]
impl std::fmt::Display for Ch341aDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CH341A at bus {} address {}", self.bus, self.address)
    }
}

// Drop implementation only for sync mode (async requires explicit shutdown)
#[cfg(feature = "is_sync")]
impl Drop for Ch341a {
    fn drop(&mut self) {
        // Drain any pending transfers before shutdown to avoid panics
        self.drain_all_pending();

        // Disable output pins on close
        if let Err(e) = self.enable_pins(false) {
            log::warn!("Failed to disable pins on close: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// WASM-only methods (WebUSB device picker, async open, shutdown)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "wasm", not(feature = "is_sync")))]
impl Ch341a {
    /// Request a CH341A device via the WebUSB permission prompt
    ///
    /// This must be called from a user gesture (e.g., button click) in the browser.
    /// It shows the browser's device picker filtered to CH341A devices.
    #[cfg(target_arch = "wasm32")]
    pub async fn request_device() -> Result<nusb::DeviceInfo> {
        use wasm_bindgen::JsCast;
        use wasm_bindgen_futures::JsFuture;
        use web_sys::{UsbDevice, UsbDeviceFilter, UsbDeviceRequestOptions};

        let usb = web_sys::window()
            .ok_or(Ch341aError::DeviceNotFound)?
            .navigator()
            .usb();

        // Create filter for CH341A devices
        let filter = UsbDeviceFilter::new();
        filter.set_vendor_id(CH341A_USB_VENDOR);
        filter.set_product_id(CH341A_USB_PRODUCT);

        let filters = js_sys::Array::new();
        filters.push(&filter);

        let options = UsbDeviceRequestOptions::new(&filters);

        log::info!("Requesting CH341A device via WebUSB picker...");

        let device_promise = usb.request_device(&options);
        let device_js = JsFuture::from(device_promise)
            .await
            .map_err(|e| Ch341aError::OpenFailed(format!("WebUSB request failed: {:?}", e)))?;

        let device: UsbDevice = device_js
            .dyn_into()
            .map_err(|_| Ch341aError::OpenFailed("Failed to get USB device".to_string()))?;

        log::info!(
            "CH341A device selected: VID={:04X} PID={:04X}",
            device.vendor_id(),
            device.product_id()
        );

        let device_info = nusb::device_info_from_webusb(device)
            .await
            .map_err(|e| Ch341aError::OpenFailed(format!("Failed to get device info: {}", e)))?;

        Ok(device_info)
    }

    /// Open a CH341A device from a DeviceInfo
    pub async fn open(device_info: nusb::DeviceInfo) -> Result<Self> {
        log::info!(
            "Opening CH341A device VID={:04X} PID={:04X}",
            device_info.vendor_id(),
            device_info.product_id()
        );

        let device = device_info
            .open()
            .await
            .map_err(|e| Ch341aError::OpenFailed(e.to_string()))?;

        let interface = device
            .claim_interface(0)
            .await
            .map_err(|e| Ch341aError::ClaimFailed(e.to_string()))?;

        let out_ep = interface
            .endpoint::<Bulk, Out>(WRITE_EP)
            .map_err(|e| Ch341aError::ClaimFailed(e.to_string()))?;
        let in_ep = interface
            .endpoint::<Bulk, In>(READ_EP)
            .map_err(|e| Ch341aError::ClaimFailed(e.to_string()))?;

        let mut ch341a = Self {
            _interface: interface,
            out_ep,
            in_ep,
            stored_delay_us: 0,
        };

        ch341a.configure().await?;
        Ok(ch341a)
    }

    /// Shutdown: disable output pins (WASM equivalent of Drop)
    pub async fn shutdown(&mut self) {
        self.drain_all_pending().await;
        if let Err(e) = self.enable_pins(false).await {
            log::warn!("Failed to disable pins on shutdown: {}", e);
        }
    }
}

// ---------------------------------------------------------------------------
// Shared methods (sync or async via maybe_async)
// ---------------------------------------------------------------------------

impl Ch341a {
    /// Configure the CH341A for SPI mode
    #[maybe_async]
    async fn configure(&mut self) -> Result<()> {
        // Set I2C/SPI mode to 100kHz base (the actual SPI speed is ~2MHz)
        self.config_stream(CH341A_STM_I2C_100K).await?;

        // Enable output pins
        self.enable_pins(true).await?;

        log::info!("CH341A configured for SPI mode");
        Ok(())
    }

    /// Configure the stream interface speed
    #[maybe_async]
    async fn config_stream(&mut self, speed: u8) -> Result<()> {
        let buf = vec![
            CH341A_CMD_I2C_STREAM,
            CH341A_CMD_I2C_STM_SET | (speed & 0x7),
            CH341A_CMD_I2C_STM_END,
        ];

        self.usb_write(&buf).await?;
        Ok(())
    }

    /// Enable or disable output pins
    #[maybe_async]
    async fn enable_pins(&mut self, enable: bool) -> Result<()> {
        let dir = if enable {
            UIO_DIR_OUTPUT
        } else {
            UIO_DIR_INPUT
        };

        let buf = vec![
            CH341A_CMD_UIO_STREAM,
            CH341A_CMD_UIO_STM_OUT | UIO_CS_DEASSERT, // CS high, SCK=0, DOUT*=1
            CH341A_CMD_UIO_STM_DIR | dir,             // Output enable/disable
            CH341A_CMD_UIO_STM_END,
        ];

        self.usb_write(&buf).await?;
        log::debug!("Pins {}abled", if enable { "en" } else { "dis" });
        Ok(())
    }

    /// Write data to USB endpoint
    #[maybe_async]
    async fn usb_write(&mut self, data: &[u8]) -> Result<()> {
        let buf = Buffer::from(data.to_vec());
        self.out_ep.submit(buf);

        let completion = ep_wait!(self.out_ep, Duration::from_secs(5))
            .ok_or_else(|| Ch341aError::TransferFailed("USB write timed out".into()))?;

        completion
            .status
            .map_err(|e| Ch341aError::TransferFailed(e.to_string()))?;

        log::trace!("USB write {} bytes", data.len());
        Ok(())
    }

    /// Perform an SPI transfer using pipelined async USB transfers.
    ///
    /// This mirrors flashprog's `usb_transfer()` approach for maximum throughput:
    /// 1. Build all OUT data (CS packet + SPI_STREAM packets) into one contiguous buffer
    /// 2. Submit the entire OUT buffer as a single async bulk transfer
    /// 3. Simultaneously pre-submit up to `USB_IN_TRANSFERS` parallel IN transfers
    /// 4. As IN transfers complete, reap them and submit new ones until all data is read
    ///
    /// This pipelining is critical for USB 1.1 performance: the device produces
    /// IN responses as it processes each SPI_STREAM packet from the OUT data,
    /// and having multiple IN transfers pre-queued ensures we never miss data.
    #[maybe_async]
    async fn spi_transfer(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        let writecnt = write_data.len();
        let readcnt = read_len;
        let total_spi_bytes = writecnt + readcnt;
        let max_packet_size = self.in_ep.max_packet_size();

        let packets = (total_spi_bytes + CH341_PACKET_LENGTH - 2) / (CH341_PACKET_LENGTH - 1);

        // Build the entire OUT buffer: CS packet + all SPI_STREAM packets
        let out_total = CH341_PACKET_LENGTH + packets * CH341_PACKET_LENGTH;
        let mut wbuf = vec![0u8; out_total];

        // First 32-byte slot: CS assertion packet
        self.build_cs_packet(&mut wbuf[..CH341_PACKET_LENGTH]);

        // Following slots: SPI_STREAM packets
        let mut write_left = writecnt;
        let mut read_left = readcnt;
        let mut write_idx = 0;

        for p in 0..packets {
            let write_now = std::cmp::min(CH341_PACKET_LENGTH - 1, write_left);
            let read_now = std::cmp::min((CH341_PACKET_LENGTH - 1) - write_now, read_left);

            let offset = CH341_PACKET_LENGTH + p * CH341_PACKET_LENGTH;
            wbuf[offset] = CH341A_CMD_SPI_STREAM;
            for i in 0..write_now {
                wbuf[offset + 1 + i] = reverse_byte(write_data[write_idx + i]);
            }
            // Fill read portion with 0xFF (already 0x00 from vec init, set explicitly)
            for i in 0..read_now {
                wbuf[offset + 1 + write_now + i] = 0xFF;
            }

            write_idx += write_now;
            write_left -= write_now;
            read_left -= read_now;
        }

        // Actual OUT length: CS packet (32) + for each SPI packet: 1 cmd byte + payload bytes
        let out_len = CH341_PACKET_LENGTH + packets + total_spi_bytes;

        // Allocate read result buffer
        let mut rbuf = vec![0u8; total_spi_bytes];
        let mut in_done: usize = 0;
        let mut in_submitted: usize = 0;

        // Number of parallel IN transfers (matching flashprog's USB_IN_TRANSFERS=32)
        const USB_IN_TRANSFERS: usize = 32;
        // Track the expected payload size for each in-flight IN transfer
        let mut in_flight_sizes: [usize; USB_IN_TRANSFERS] = [0; USB_IN_TRANSFERS];
        let mut submit_idx: usize = 0;
        let mut complete_idx: usize = 0;
        let mut in_pending: usize = 0;

        // IN transfer request size must be a multiple of max_packet_size
        let in_request_len = max_packet_size;

        // Submit the OUT transfer (non-blocking)
        let out_buf = Buffer::from(wbuf[..out_len].to_vec());
        self.out_ep.submit(out_buf);
        let mut out_done = false;

        // Main loop: interleave IN submissions, IN completions, and OUT progress.
        // This mirrors flashprog's event loop where libusb_handle_events_timeout()
        // drives both OUT and IN transfers simultaneously. In nusb, waiting on
        // in_ep also allows the shared event handler to process OUT completions.
        //
        // We must keep IN transfers queued so the host controller can alternate
        // OUT and IN transactions in each USB frame. If we exhaust our IN queue
        // without resubmitting, the device has nowhere to send responses and
        // stalls, preventing it from accepting more OUT data -> deadlock.
        loop {
            // Schedule new IN reads as long as there are free slots and unscheduled bytes
            while in_pending < USB_IN_TRANSFERS && in_submitted < total_spi_bytes {
                let cur_todo =
                    std::cmp::min(CH341_PACKET_LENGTH - 1, total_spi_bytes - in_submitted);
                in_flight_sizes[submit_idx] = cur_todo;

                let buf = Buffer::new(in_request_len);
                self.in_ep.submit(buf);

                in_submitted += cur_todo;
                in_pending += 1;
                submit_idx = (submit_idx + 1) % USB_IN_TRANSFERS;
            }

            // Check if we're done
            if out_done && in_done >= total_spi_bytes {
                break;
            }

            // Wait for the next IN completion (this also drives OUT progress via
            // nusb's shared event loop on the usbfs fd)
            if self.in_ep.pending() > 0 {
                let completion = match ep_wait!(self.in_ep, Duration::from_secs(5)) {
                    Some(c) => c,
                    None => {
                        self.drain_all_pending().await;
                        return Err(Ch341aError::TransferFailed("IN transfer timed out".into()));
                    }
                };

                if let Err(e) = completion.status {
                    self.drain_all_pending().await;
                    return Err(Ch341aError::TransferFailed(format!(
                        "IN transfer failed: {e}"
                    )));
                }

                let expected = in_flight_sizes[complete_idx];
                let actual = std::cmp::min(completion.actual_len, expected);
                let dst_end = std::cmp::min(in_done + actual, total_spi_bytes);
                rbuf[in_done..dst_end].copy_from_slice(&completion.buffer[..dst_end - in_done]);
                in_done += actual;
                in_pending -= 1;
                complete_idx = (complete_idx + 1) % USB_IN_TRANSFERS;
            }

            // Check OUT completion (non-blocking: just check if it's done)
            if !out_done && self.out_ep.pending() > 0 {
                if let Some(c) = ep_try!(self.out_ep) {
                    if let Err(e) = c.status {
                        self.drain_all_pending().await;
                        return Err(Ch341aError::TransferFailed(format!(
                            "OUT transfer failed: {e}"
                        )));
                    }
                    out_done = true;
                }
            } else if !out_done {
                // OUT endpoint has 0 pending but we never saw completion - shouldn't happen
                out_done = true;
            }
        }

        // Drain any extra pending transfers
        self.drain_all_pending().await;

        // Extract and bit-reverse the read data
        let mut result = Vec::with_capacity(readcnt);
        for i in 0..readcnt {
            result.push(reverse_byte(rbuf[writecnt + i]));
        }

        Ok(result)
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

    /// Build the CS assertion packet with delay handling
    fn build_cs_packet(&mut self, packet: &mut [u8]) {
        // Calculate delay count (minimum 2 for ~2.25us deassertion)
        let delay_cnt = if self.stored_delay_us > 0 {
            (self.stored_delay_us * 4 / 3) as usize
        } else {
            2
        };
        self.stored_delay_us = 0;

        let mut idx = 0;
        packet[idx] = CH341A_CMD_UIO_STREAM;
        idx += 1;

        // Deassert CS
        packet[idx] = CH341A_CMD_UIO_STM_OUT | UIO_CS_DEASSERT;
        idx += 1;

        // Add delay cycles (limited by packet size)
        let max_delay = CH341_PACKET_LENGTH - 4; // Leave room for CS assert and end
        let actual_delay = std::cmp::min(delay_cnt, max_delay);
        for _ in 0..actual_delay {
            packet[idx] = CH341A_CMD_UIO_STM_OUT | UIO_CS_DEASSERT;
            idx += 1;
        }

        // Assert CS
        packet[idx] = CH341A_CMD_UIO_STM_OUT | UIO_CS_ASSERT;
        idx += 1;

        // End UIO stream
        packet[idx] = CH341A_CMD_UIO_STM_END;
    }
}

// ---------------------------------------------------------------------------
// SpiMaster trait implementation
// ---------------------------------------------------------------------------

#[maybe_async(AFIT)]
impl SpiMaster for Ch341a {
    fn features(&self) -> SpiFeatures {
        // CH341A supports 4-byte addressing (software handled)
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        // CH341A can handle large transfers, 4KB is a reasonable chunk size
        4 * 1024
    }

    fn max_write_len(&self) -> usize {
        // CH341A can handle large transfers, 4KB is a reasonable chunk size
        4 * 1024
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
        // Accumulate small delays into the CS packet (up to ~20us)
        if (us + self.stored_delay_us) > 20 {
            let inc = 20 - self.stored_delay_us;

            #[cfg(feature = "is_sync")]
            {
                std::thread::sleep(Duration::from_micros((us - inc) as u64));
            }

            #[cfg(all(feature = "wasm", not(feature = "is_sync")))]
            {
                let delay_ms = ((us - inc) as f64 / 1000.0).ceil() as i32;
                if delay_ms > 0 {
                    // Use setTimeout to delay in WASM
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

            self.stored_delay_us = inc;
        } else {
            self.stored_delay_us += us;
        }
    }
}

//! Async CH341A device implementation for WebUSB (wasm32)
//!
//! This module provides an async version of the CH341A that uses nusb's async
//! transfer API, suitable for WebUSB in the browser.

use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::{Endpoint, Interface};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};

use crate::error::{Ch341aError, Result};
use crate::protocol::*;

/// Async CH341A USB programmer for WebUSB
///
/// This is the async counterpart of `Ch341a`, designed for use in WASM
/// environments where blocking USB transfers are not available.
pub struct Ch341aAsync {
    /// USB interface (held to keep the device claim alive)
    _interface: Interface,
    /// Bulk OUT endpoint for writes
    out_ep: Endpoint<Bulk, Out>,
    /// Bulk IN endpoint for reads
    in_ep: Endpoint<Bulk, In>,
    /// Accumulated delay for CS handling
    stored_delay_us: u32,
}

impl Ch341aAsync {
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
        let device_js = JsFuture::from(device_promise).await.map_err(|e| {
            Ch341aError::OpenFailed(format!("WebUSB request failed: {:?}", e))
        })?;

        let device: UsbDevice = device_js
            .dyn_into()
            .map_err(|_| Ch341aError::OpenFailed("Failed to get USB device".to_string()))?;

        log::info!(
            "CH341A device selected: VID={:04X} PID={:04X}",
            device.vendor_id(),
            device.product_id()
        );

        let device_info = nusb::device_info_from_webusb(device).await.map_err(|e| {
            Ch341aError::OpenFailed(format!("Failed to get device info: {}", e))
        })?;

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

    /// Configure the CH341A for SPI mode
    async fn configure(&mut self) -> Result<()> {
        self.config_stream(CH341A_STM_I2C_100K).await?;
        self.enable_pins(true).await?;
        log::info!("CH341A configured for SPI mode");
        Ok(())
    }

    /// Configure the stream interface speed
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
    async fn enable_pins(&mut self, enable: bool) -> Result<()> {
        let dir = if enable {
            UIO_DIR_OUTPUT
        } else {
            UIO_DIR_INPUT
        };

        let buf = vec![
            CH341A_CMD_UIO_STREAM,
            CH341A_CMD_UIO_STM_OUT | UIO_CS_DEASSERT,
            CH341A_CMD_UIO_STM_DIR | dir,
            CH341A_CMD_UIO_STM_END,
        ];

        self.usb_write(&buf).await?;
        log::debug!("Pins {}abled", if enable { "en" } else { "dis" });
        Ok(())
    }

    /// Perform an SPI transfer (async)
    ///
    /// Unlike the native version which sends the entire buffer in one USB
    /// transfer, the WebUSB version sends each 32-byte packet individually
    /// and reads each 31-byte response right after. This avoids WebUSB
    /// issues with large bulk transfers on USB 1.1 full-speed devices.
    async fn spi_transfer(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        let writecnt = write_data.len();
        let readcnt = read_len;
        let max_packet_size = self.in_ep.max_packet_size();

        let packets = (writecnt + readcnt + CH341_PACKET_LENGTH - 2) / (CH341_PACKET_LENGTH - 1);

        // 1. Send the CS assertion packet
        let mut cs_packet = [0u8; CH341_PACKET_LENGTH];
        self.build_cs_packet(&mut cs_packet);
        self.usb_write(&cs_packet).await?;

        // 2. Send each SPI_STREAM packet and collect responses
        let mut rbuf = Vec::with_capacity(writecnt + readcnt);
        let mut write_left = writecnt;
        let mut read_left = readcnt;
        let mut write_idx = 0;

        for _p in 0..packets {
            let write_now = core::cmp::min(CH341_PACKET_LENGTH - 1, write_left);
            let read_now = core::cmp::min((CH341_PACKET_LENGTH - 1) - write_now, read_left);
            let payload_len = write_now + read_now;

            // Build one SPI_STREAM packet
            let mut packet = [0u8; CH341_PACKET_LENGTH];
            packet[0] = CH341A_CMD_SPI_STREAM;
            for i in 0..write_now {
                packet[1 + i] = reverse_byte(write_data[write_idx + i]);
            }
            for i in 0..read_now {
                packet[1 + write_now + i] = 0xFF;
            }
            write_idx += write_now;
            write_left -= write_now;
            read_left -= read_now;

            // Send the packet (only the meaningful bytes: 1 cmd + payload)
            let send_len = 1 + payload_len;
            let out_buf = Buffer::from(packet[..send_len].to_vec());
            self.out_ep.submit(out_buf);
            let wc = std::future::poll_fn(|cx| self.out_ep.poll_next_complete(cx)).await;
            wc.status
                .map_err(|e| Ch341aError::TransferFailed(e.to_string()))?;

            // Read the response (device returns payload_len bytes)
            let request_len = payload_len.div_ceil(max_packet_size) * max_packet_size;
            let request_len = core::cmp::max(request_len, max_packet_size); // at least one packet
            let mut in_buf = Buffer::new(request_len);
            in_buf.set_requested_len(request_len);
            self.in_ep.submit(in_buf);
            let rc = std::future::poll_fn(|cx| self.in_ep.poll_next_complete(cx)).await;
            rc.status
                .map_err(|e| Ch341aError::TransferFailed(e.to_string()))?;

            let actual = core::cmp::min(rc.actual_len, payload_len);
            rbuf.extend_from_slice(&rc.buffer[..actual]);
        }

        // 3. Extract and bit-reverse the read data
        let mut result = Vec::with_capacity(readcnt);
        for i in 0..readcnt {
            if writecnt + i < rbuf.len() {
                result.push(reverse_byte(rbuf[writecnt + i]));
            } else {
                result.push(0xFF); // pad if we got less data than expected
            }
        }

        Ok(result)
    }

    /// Build the CS assertion packet with delay handling
    fn build_cs_packet(&mut self, packet: &mut [u8]) {
        let delay_cnt = if self.stored_delay_us > 0 {
            (self.stored_delay_us * 4 / 3) as usize
        } else {
            2
        };
        self.stored_delay_us = 0;

        let mut idx = 0;
        packet[idx] = CH341A_CMD_UIO_STREAM;
        idx += 1;

        packet[idx] = CH341A_CMD_UIO_STM_OUT | UIO_CS_DEASSERT;
        idx += 1;

        let max_delay = CH341_PACKET_LENGTH - 4;
        let actual_delay = core::cmp::min(delay_cnt, max_delay);
        for _ in 0..actual_delay {
            packet[idx] = CH341A_CMD_UIO_STM_OUT | UIO_CS_DEASSERT;
            idx += 1;
        }

        packet[idx] = CH341A_CMD_UIO_STM_OUT | UIO_CS_ASSERT;
        idx += 1;

        packet[idx] = CH341A_CMD_UIO_STM_END;
    }

    /// Write data to USB endpoint (async)
    async fn usb_write(&mut self, data: &[u8]) -> Result<()> {
        let buf = Buffer::from(data.to_vec());
        self.out_ep.submit(buf);

        let completion =
            std::future::poll_fn(|cx| self.out_ep.poll_next_complete(cx)).await;
        completion
            .status
            .map_err(|e| Ch341aError::TransferFailed(e.to_string()))?;

        log::trace!("USB write {} bytes", data.len());
        Ok(())
    }



    /// Shutdown: disable output pins
    pub async fn shutdown(&mut self) {
        if let Err(e) = self.enable_pins(false).await {
            log::warn!("Failed to disable pins on shutdown: {}", e);
        }
    }
}

impl SpiMaster for Ch341aAsync {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        4 * 1024
    }

    fn max_write_len(&self) -> usize {
        4 * 1024
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        check_io_mode_supported(cmd.io_mode, self.features())?;

        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];

        cmd.encode_header(&mut write_data);
        write_data[header_len..].copy_from_slice(cmd.write_data);

        let read_len = cmd.read_buf.len();
        let result = self
            .spi_transfer(&write_data, read_len)
            .await
            .map_err(|_e| CoreError::ProgrammerError)?;

        cmd.read_buf.copy_from_slice(&result);

        Ok(())
    }

    async fn delay_us(&mut self, us: u32) {
        // Accumulate small delays into the CS packet (up to ~20us)
        // For longer delays, yield to browser with a timeout
        if (us + self.stored_delay_us) > 20 {
            let inc = 20 - self.stored_delay_us;
            let delay_ms = ((us - inc) as f64 / 1000.0).ceil() as i32;
            if delay_ms > 0 {
                // Use setTimeout to delay in WASM
                let promise = js_sys::Promise::new(&mut |resolve, _| {
                    let window = web_sys::window().unwrap();
                    window
                        .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, delay_ms)
                        .unwrap();
                });
                let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
            }
            self.stored_delay_us = inc;
        } else {
            self.stored_delay_us += us;
        }
    }
}

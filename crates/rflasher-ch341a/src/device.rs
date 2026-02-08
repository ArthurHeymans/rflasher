//! CH341A device implementation
//!
//! This module provides the main `Ch341a` struct that implements USB
//! communication with the CH341A programmer and the `SpiMaster` trait.

use std::time::Duration;

use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::{Endpoint, MaybeFuture};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{check_io_mode_supported, SpiCommand};

use crate::error::{Ch341aError, Result};
use crate::protocol::*;

/// CH341A USB programmer
///
/// This struct represents a connection to a CH341A USB device and implements
/// the `SpiMaster` trait for communicating with SPI flash chips.
pub struct Ch341a {
    /// Bulk OUT endpoint for writes
    out_ep: Endpoint<Bulk, Out>,
    /// Bulk IN endpoint for reads
    in_ep: Endpoint<Bulk, In>,
    /// Accumulated delay for CS handling
    stored_delay_us: u32,
}

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

    /// Configure the CH341A for SPI mode
    fn configure(&mut self) -> Result<()> {
        // Set I2C/SPI mode to 100kHz base (the actual SPI speed is ~2MHz)
        self.config_stream(CH341A_STM_I2C_100K)?;

        // Enable output pins
        self.enable_pins(true)?;

        log::info!("CH341A configured for SPI mode");
        Ok(())
    }

    /// Configure the stream interface speed
    fn config_stream(&mut self, speed: u8) -> Result<()> {
        let buf = vec![
            CH341A_CMD_I2C_STREAM,
            CH341A_CMD_I2C_STM_SET | (speed & 0x7),
            CH341A_CMD_I2C_STM_END,
        ];

        self.usb_write(&buf)?;
        Ok(())
    }

    /// Enable or disable output pins
    fn enable_pins(&mut self, enable: bool) -> Result<()> {
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

        self.usb_write(&buf)?;
        log::debug!("Pins {}abled", if enable { "en" } else { "dis" });
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
    fn spi_transfer(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
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
        // stalls, preventing it from accepting more OUT data → deadlock.
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
                let completion = match self.in_ep.wait_next_complete(Duration::from_secs(5)) {
                    Some(c) => c,
                    None => {
                        self.drain_all_pending();
                        return Err(Ch341aError::TransferFailed("IN transfer timed out".into()));
                    }
                };

                if let Err(e) = completion.status {
                    self.drain_all_pending();
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
                if let Some(c) = self.out_ep.wait_next_complete(Duration::ZERO) {
                    if let Err(e) = c.status {
                        self.drain_all_pending();
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
        self.drain_all_pending();

        // Extract and bit-reverse the read data
        let mut result = Vec::with_capacity(readcnt);
        for i in 0..readcnt {
            result.push(reverse_byte(rbuf[writecnt + i]));
        }

        Ok(result)
    }

    /// Cancel and drain all pending transfers on both endpoints.
    fn drain_all_pending(&mut self) {
        self.out_ep.cancel_all();
        while self.out_ep.pending() > 0 {
            let _ = self.out_ep.wait_next_complete(Duration::from_secs(1));
        }
        self.in_ep.cancel_all();
        while self.in_ep.pending() > 0 {
            let _ = self.in_ep.wait_next_complete(Duration::from_secs(1));
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

    /// Write data to USB endpoint (blocking)
    fn usb_write(&mut self, data: &[u8]) -> Result<()> {
        let mut buf = Buffer::new(data.len());
        buf.extend_from_slice(data);

        let completion = self.out_ep.transfer_blocking(buf, Duration::from_secs(5));

        completion
            .into_result()
            .map_err(|e| Ch341aError::TransferFailed(e.to_string()))?;

        log::trace!("USB write {} bytes", data.len());
        Ok(())
    }
}

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
        // Accumulate small delays into the CS packet (up to ~20us)
        // For longer delays, use actual sleep
        if (us + self.stored_delay_us) > 20 {
            let inc = 20 - self.stored_delay_us;
            std::thread::sleep(Duration::from_micros((us - inc) as u64));
            self.stored_delay_us = inc;
        } else {
            self.stored_delay_us += us;
        }
    }
}

/// Information about a connected CH341A device
#[derive(Debug, Clone)]
pub struct Ch341aDeviceInfo {
    /// USB bus number
    pub bus: u8,
    /// USB device address
    pub address: u8,
}

impl std::fmt::Display for Ch341aDeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CH341A at bus {} address {}", self.bus, self.address)
    }
}

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

    /// Perform an SPI transfer
    ///
    /// This is the core function that sends data to and receives data from
    /// the SPI flash chip.
    fn spi_transfer(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        let writecnt = write_data.len();
        let readcnt = read_len;

        // Calculate how many packets we need
        // Each packet can hold 31 bytes of SPI data (32 - 1 for command byte)
        let packets = (writecnt + readcnt + CH341_PACKET_LENGTH - 2) / (CH341_PACKET_LENGTH - 1);

        // Allocate write buffer: CS packet + data packets
        // CS packet is 32 bytes, each data packet is 32 bytes
        let total_write_len = CH341_PACKET_LENGTH + packets * CH341_PACKET_LENGTH;
        let mut wbuf = vec![0u8; total_write_len];

        // Build CS assertion packet with any accumulated delay
        self.build_cs_packet(&mut wbuf[0..CH341_PACKET_LENGTH]);

        // Build data packets
        let mut write_left = writecnt;
        let mut read_left = readcnt;
        let mut write_idx = 0;

        for p in 0..packets {
            let packet_start = CH341_PACKET_LENGTH + p * CH341_PACKET_LENGTH;
            let packet = &mut wbuf[packet_start..packet_start + CH341_PACKET_LENGTH];

            let write_now = std::cmp::min(CH341_PACKET_LENGTH - 1, write_left);
            let read_now = std::cmp::min((CH341_PACKET_LENGTH - 1) - write_now, read_left);

            packet[0] = CH341A_CMD_SPI_STREAM;

            // Copy write data with bit reversal
            for i in 0..write_now {
                packet[1 + i] = reverse_byte(write_data[write_idx + i]);
            }
            write_idx += write_now;
            write_left -= write_now;

            // Fill read portion with 0xFF
            for i in 0..read_now {
                packet[1 + write_now + i] = 0xFF;
            }
            read_left -= read_now;
        }

        // Calculate actual bytes to send and receive
        let actual_write_len = CH341_PACKET_LENGTH + packets + writecnt + readcnt;
        let actual_read_len = writecnt + readcnt;

        // Perform USB transfer
        let rbuf = self.usb_transfer(&wbuf[..actual_write_len], actual_read_len)?;

        // Extract and reverse read data
        let mut result = Vec::with_capacity(readcnt);
        for i in 0..readcnt {
            result.push(reverse_byte(rbuf[writecnt + i]));
        }

        Ok(result)
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

    /// Perform a USB transfer (write then read, blocking)
    fn usb_transfer(&mut self, write_data: &[u8], read_len: usize) -> Result<Vec<u8>> {
        // For the CH341A, we need to handle the quirky packet-based protocol
        // where each 32-byte write packet results in a 31-byte read response.

        // First, write the data
        let mut out_buf = Buffer::new(write_data.len());
        out_buf.extend_from_slice(write_data);
        let write_completion = self
            .out_ep
            .transfer_blocking(out_buf, Duration::from_secs(5));
        write_completion
            .into_result()
            .map_err(|e| Ch341aError::TransferFailed(e.to_string()))?;

        // Collect read responses (blocking)
        let mut result = Vec::with_capacity(read_len);
        let mut remaining = read_len;
        let max_packet_size = self.in_ep.max_packet_size();

        // We need to read enough data to cover all expected bytes
        // Each read can return up to max_packet_size bytes
        while remaining > 0 {
            // Request length must be multiple of max packet size
            let request_len = std::cmp::min(remaining, CH341_PACKET_LENGTH - 1)
                .div_ceil(max_packet_size)
                * max_packet_size;
            let mut in_buf = Buffer::new(request_len);
            in_buf.set_requested_len(request_len);

            let read_completion = self.in_ep.transfer_blocking(in_buf, Duration::from_secs(5));

            let data = read_completion
                .into_result()
                .map_err(|e| Ch341aError::TransferFailed(e.to_string()))?;

            let to_take = std::cmp::min(data.len(), remaining);
            result.extend_from_slice(&data[..to_take]);
            remaining -= to_take;

            // If we got less than expected, stop
            if data.len() < request_len {
                break;
            }
        }

        log::trace!(
            "USB transfer: wrote {} bytes, read {} bytes",
            write_data.len(),
            result.len()
        );

        Ok(result)
    }
}

impl Drop for Ch341a {
    fn drop(&mut self) {
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

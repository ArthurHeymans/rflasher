//! FEL USB protocol implementation
//!
//! The FEL protocol communicates with the Allwinner BROM over USB bulk
//! transfers. It supports three core operations:
//! - Read memory at an address
//! - Write memory at an address
//! - Execute code at an address
//!
//! All multi-byte values are little-endian.

use crate::error::{Error, Result};
use nusb::transfer::{Buffer, Bulk, In, Out};
use nusb::Endpoint;
use std::time::Duration;

/// Allwinner FEL USB Vendor ID
pub const FEL_VID: u16 = 0x1f3a;
/// Allwinner FEL USB Product ID
pub const FEL_PID: u16 = 0xefe8;

/// USB transfer timeout
const USB_TIMEOUT: Duration = Duration::from_secs(10);

// FEL request types
const FEL_VERSION: u32 = 0x001;
const FEL_WRITE: u32 = 0x101;
const FEL_EXEC: u32 = 0x102;
const FEL_READ: u32 = 0x103;

// USB request types
const USB_REQUEST_WRITE: u16 = 0x12;
const USB_REQUEST_READ: u16 = 0x11;

/// FEL version information returned by the BROM
#[derive(Debug, Clone)]
pub struct FelVersion {
    /// SoC ID
    pub id: u32,
    /// Firmware version
    pub firmware: u32,
    /// Protocol version
    pub protocol: u16,
    /// Data flag
    pub dflag: u8,
    /// Data length
    pub dlength: u8,
    /// Scratchpad address in SRAM
    pub scratchpad: u32,
}

/// Low-level FEL USB transport
pub struct FelTransport {
    out_ep: Endpoint<Bulk, Out>,
    in_ep: Endpoint<Bulk, In>,
    /// Max packet size for the IN endpoint
    max_packet_size: usize,
}

impl FelTransport {
    pub fn new(out_ep: Endpoint<Bulk, Out>, in_ep: Endpoint<Bulk, In>) -> Self {
        let max_packet_size = in_ep.max_packet_size();
        Self {
            out_ep,
            in_ep,
            max_packet_size,
        }
    }

    /// Round up to next multiple of max packet size (Linux usbfs requirement)
    fn round_up_to_mps(&self, len: usize) -> usize {
        let mps = self.max_packet_size;
        if mps == 0 {
            return len;
        }
        len.div_ceil(mps) * mps
    }

    fn usb_bulk_send(&mut self, data: &[u8]) -> Result<()> {
        let max_chunk = 128 * 1024;
        let mut offset = 0;
        while offset < data.len() {
            let chunk_size = (data.len() - offset).min(max_chunk);
            let chunk = &data[offset..offset + chunk_size];
            let buf = Buffer::from(chunk.to_vec());
            self.out_ep.submit(buf);
            let completion = self
                .out_ep
                .wait_next_complete(USB_TIMEOUT)
                .ok_or_else(|| Error::Usb("bulk send timeout".into()))?;
            completion
                .status
                .map_err(|e| Error::Usb(format!("bulk send: {}", e)))?;
            offset += chunk_size;
        }
        Ok(())
    }

    fn usb_bulk_recv(&mut self, len: usize) -> Result<Vec<u8>> {
        let mut result = Vec::with_capacity(len);
        let mut remaining = len;
        while remaining > 0 {
            let alloc_size = self.round_up_to_mps(remaining);
            let buf = Buffer::new(alloc_size);
            self.in_ep.submit(buf);
            let completion = self
                .in_ep
                .wait_next_complete(USB_TIMEOUT)
                .ok_or_else(|| Error::Usb("bulk recv timeout".into()))?;
            completion
                .status
                .map_err(|e| Error::Usb(format!("bulk recv: {}", e)))?;
            let actual = completion.actual_len.min(remaining);
            if actual == 0 {
                return Err(Error::Usb("bulk recv: zero-length transfer".into()));
            }
            result.extend_from_slice(&completion.buffer[..actual]);
            remaining -= actual;
        }
        Ok(result)
    }

    /// Send USB request header (32 bytes, packed)
    fn send_usb_request(&mut self, request_type: u16, length: u32) -> Result<()> {
        let mut req = [0u8; 32];
        req[0..4].copy_from_slice(b"AWUC");
        req[8..12].copy_from_slice(&length.to_le_bytes());
        req[12..16].copy_from_slice(&0x0c000000u32.to_le_bytes());
        req[16..18].copy_from_slice(&request_type.to_le_bytes());
        req[18..22].copy_from_slice(&length.to_le_bytes());
        self.usb_bulk_send(&req)
    }

    /// Read USB response (13 bytes, "AWUS" magic)
    fn read_usb_response(&mut self) -> Result<()> {
        let data = self.usb_bulk_recv(13)?;
        if data.len() < 4 || &data[0..4] != b"AWUS" {
            return Err(Error::Protocol("invalid USB response magic".into()));
        }
        Ok(())
    }

    fn usb_write(&mut self, data: &[u8]) -> Result<()> {
        self.send_usb_request(USB_REQUEST_WRITE, data.len() as u32)?;
        self.usb_bulk_send(data)?;
        self.read_usb_response()
    }

    fn usb_read(&mut self, len: usize) -> Result<Vec<u8>> {
        self.send_usb_request(USB_REQUEST_READ, len as u32)?;
        let data = self.usb_bulk_recv(len)?;
        self.read_usb_response()?;
        Ok(data)
    }

    fn send_fel_request(&mut self, request: u32, addr: u32, length: u32) -> Result<()> {
        let mut req = [0u8; 16];
        req[0..4].copy_from_slice(&request.to_le_bytes());
        req[4..8].copy_from_slice(&addr.to_le_bytes());
        req[8..12].copy_from_slice(&length.to_le_bytes());
        self.usb_write(&req)
    }

    fn read_fel_status(&mut self) -> Result<()> {
        self.usb_read(8)?;
        Ok(())
    }

    /// Query the FEL version (SoC identification)
    pub fn fel_version(&mut self) -> Result<FelVersion> {
        self.send_fel_request(FEL_VERSION, 0, 0)?;
        let data = self.usb_read(32)?;
        self.read_fel_status()?;
        if data.len() < 32 {
            return Err(Error::Protocol("version response too short".into()));
        }
        Ok(FelVersion {
            id: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            firmware: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
            protocol: u16::from_le_bytes([data[16], data[17]]),
            dflag: data[18],
            dlength: data[19],
            scratchpad: u32::from_le_bytes([data[20], data[21], data[22], data[23]]),
        })
    }

    /// Execute code at the given address
    pub fn fel_exec(&mut self, addr: u32) -> Result<()> {
        self.send_fel_request(FEL_EXEC, addr, 0)?;
        self.read_fel_status()
    }

    fn fel_read_raw(&mut self, addr: u32, len: usize) -> Result<Vec<u8>> {
        self.send_fel_request(FEL_READ, addr, len as u32)?;
        let data = self.usb_read(len)?;
        self.read_fel_status()?;
        Ok(data)
    }

    fn fel_write_raw(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        self.send_fel_request(FEL_WRITE, addr, data.len() as u32)?;
        self.usb_write(data)?;
        self.read_fel_status()
    }

    /// Read memory (with chunking for large transfers)
    pub fn fel_read(&mut self, addr: u32, len: usize) -> Result<Vec<u8>> {
        let mut result = Vec::with_capacity(len);
        let mut offset = 0u32;
        let mut remaining = len;
        while remaining > 0 {
            let n = remaining.min(65536);
            let chunk = self.fel_read_raw(addr + offset, n)?;
            result.extend_from_slice(&chunk);
            offset += n as u32;
            remaining -= n;
        }
        Ok(result)
    }

    /// Write memory (with chunking for large transfers)
    pub fn fel_write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        let mut offset = 0usize;
        while offset < data.len() {
            let n = (data.len() - offset).min(65536);
            self.fel_write_raw(addr + offset as u32, &data[offset..offset + n])?;
            offset += n;
        }
        Ok(())
    }
}

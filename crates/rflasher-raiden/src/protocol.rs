//! Raiden Debug SPI protocol constants and packet structures
//!
//! This module contains the USB protocol definitions for communicating with
//! ChromiumOS EC USB SPI bridges (Raiden Debug SPI).
//!
//! The protocol is defined in the ChromiumOS EC repository:
//! https://chromium.googlesource.com/chromiumos/platform/ec
//! Files: chip/stm32/usb_spi.h and chip/stm32/usb_spi.c

#![allow(dead_code)]

use zerocopy::byteorder::little_endian::U16 as U16Le;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

// ===========================================================================
// USB Device Identifiers
// ===========================================================================

/// Google USB Vendor ID
pub const GOOGLE_VID: u16 = 0x18D1;

/// USB subclass for Raiden SPI interface
pub const RAIDEN_SPI_SUBCLASS: u8 = 0x51;

/// Protocol version 1 (basic, 62-byte max payload)
pub const PROTOCOL_V1: u8 = 0x01;

/// Protocol version 2 (extended, supports larger transfers)
pub const PROTOCOL_V2: u8 = 0x02;

// ===========================================================================
// USB Transfer Parameters
// ===========================================================================

/// USB packet size (max for full-speed USB)
pub const USB_PACKET_SIZE: usize = 64;

/// Timeout for USB transfers in milliseconds
/// 200ms + 800ms SPI timeout
pub const USB_TIMEOUT_MS: u64 = 1000;

/// Number of write retries
pub const WRITE_RETRIES: u32 = 3;

/// Number of read retries
pub const READ_RETRIES: u32 = 3;

/// Delay between retries in milliseconds
pub const RETRY_DELAY_MS: u64 = 100;

/// Delay after enabling target for power/flash stabilization
pub const ENABLE_DELAY_MS: u64 = 50;

// ===========================================================================
// Protocol V1 Constants
// ===========================================================================

/// Maximum payload in a V1 command packet (64 - 2 = 62 bytes)
pub const V1_MAX_PAYLOAD: usize = 62;

/// Indicates full duplex mode in V1 read_count field
pub const V1_FULL_DUPLEX_MARKER: u8 = 0xFF;

// ===========================================================================
// Protocol V2 Constants
// ===========================================================================

/// Maximum payload in a V2 start packet (64 - 6 = 58 bytes)
pub const V2_START_PAYLOAD: usize = 58;

/// Maximum payload in a V2 continue packet (64 - 4 = 60 bytes)
pub const V2_CONTINUE_PAYLOAD: usize = 60;

/// Indicates full duplex mode in V2 read_count field
pub const V2_FULL_DUPLEX_MARKER: u16 = 0xFFFF;

// ===========================================================================
// Packet IDs (Protocol V2)
// ===========================================================================

/// Packet ID types for protocol V2
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum PacketId {
    /// Command: Request device configuration
    CmdGetUsbSpiConfig = 0,
    /// Response: Device configuration
    RspUsbSpiConfig = 1,
    /// Command: Start of SPI transfer
    CmdTransferStart = 2,
    /// Command: Continue SPI transfer (additional write data)
    CmdTransferContinue = 3,
    /// Command: Restart response (retry reading response)
    CmdRestartResponse = 4,
    /// Response: Start of SPI transfer response
    RspTransferStart = 5,
    /// Response: Continue transfer response (additional read data)
    RspTransferContinue = 6,
}

impl PacketId {
    /// Create a PacketId from a raw u16 value
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            0 => Some(PacketId::CmdGetUsbSpiConfig),
            1 => Some(PacketId::RspUsbSpiConfig),
            2 => Some(PacketId::CmdTransferStart),
            3 => Some(PacketId::CmdTransferContinue),
            4 => Some(PacketId::CmdRestartResponse),
            5 => Some(PacketId::RspTransferStart),
            6 => Some(PacketId::RspTransferContinue),
            _ => None,
        }
    }
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct PacketV2Header {
    packet_id: U16Le,
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct CommandV2StartHeader {
    packet_id: U16Le,
    write_count: U16Le,
    read_count: U16Le,
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct PacketV2ContinueHeader {
    packet_id: U16Le,
    data_index: U16Le,
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct ResponseV2StartHeader {
    packet_id: U16Le,
    status_code: U16Le,
}

#[repr(C)]
#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned)]
struct ResponseV2ConfigWire {
    packet_id: U16Le,
    max_write_count: U16Le,
    max_read_count: U16Le,
    feature_bitmap: U16Le,
}

// ===========================================================================
// USB Control Requests
// ===========================================================================

/// USB control request types for enabling/disabling SPI bridge
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ControlRequest {
    /// Enable SPI bridge (generic)
    Enable = 0x0000,
    /// Disable SPI bridge
    Disable = 0x0001,
    /// Enable SPI to Application Processor
    EnableAp = 0x0002,
    /// Enable SPI to Embedded Controller
    EnableEc = 0x0003,
    /// Enable SPI to H1 security chip
    EnableH1 = 0x0004,
    /// Reset target
    Reset = 0x0005,
    /// Boot configuration
    BootCfg = 0x0006,
    /// Socket
    Socket = 0x0007,
    /// Start signing
    SigningStart = 0x0008,
    /// Sign
    SigningSign = 0x0009,
    /// Enable AP with custom reset timing
    EnableApCustom = 0x000A,
}

// ===========================================================================
// USB SPI Status Codes
// ===========================================================================

/// Status codes returned by the USB SPI device
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum StatusCode {
    /// Operation successful
    Success = 0x0000,
    /// SPI timeout
    Timeout = 0x0001,
    /// SPI busy
    Busy = 0x0002,
    /// Invalid write count
    WriteCountInvalid = 0x0003,
    /// Invalid read count
    ReadCountInvalid = 0x0004,
    /// SPI disabled
    Disabled = 0x0005,
    /// Bad data index in response
    RxBadDataIndex = 0x0006,
    /// Data overflow
    RxDataOverflow = 0x0007,
    /// Unexpected packet
    RxUnexpectedPacket = 0x0008,
    /// Full duplex not supported
    UnsupportedFullDuplex = 0x0009,
    /// Unknown error
    UnknownError = 0x8000,
}

impl StatusCode {
    /// Create a StatusCode from a raw u16 value
    pub fn from_u16(value: u16) -> Self {
        match value {
            0x0000 => StatusCode::Success,
            0x0001 => StatusCode::Timeout,
            0x0002 => StatusCode::Busy,
            0x0003 => StatusCode::WriteCountInvalid,
            0x0004 => StatusCode::ReadCountInvalid,
            0x0005 => StatusCode::Disabled,
            0x0006 => StatusCode::RxBadDataIndex,
            0x0007 => StatusCode::RxDataOverflow,
            0x0008 => StatusCode::RxUnexpectedPacket,
            0x0009 => StatusCode::UnsupportedFullDuplex,
            _ => StatusCode::UnknownError,
        }
    }

    /// Check if this status indicates success
    pub fn is_success(&self) -> bool {
        *self == StatusCode::Success
    }
}

// ===========================================================================
// Configuration Features (Protocol V2)
// ===========================================================================

/// Feature bitmap for V2 configuration response
pub mod features {
    /// Full duplex SPI is supported
    pub const FULL_DUPLEX: u16 = 1 << 0;
}

// ===========================================================================
// SPI Target Types
// ===========================================================================

/// Target to enable for SPI communication
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    /// Application Processor flash
    #[default]
    Ap,
    /// Embedded Controller flash
    Ec,
    /// H1 security chip
    H1,
    /// AP with custom reset timing (10ms instead of 3ms)
    ApCustom,
}

impl Target {
    /// Get the USB control request for enabling this target
    pub fn enable_request(&self) -> ControlRequest {
        match self {
            Target::Ap => ControlRequest::EnableAp,
            Target::Ec => ControlRequest::EnableEc,
            Target::H1 => ControlRequest::EnableH1,
            Target::ApCustom => ControlRequest::EnableApCustom,
        }
    }
}

impl std::str::FromStr for Target {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "ap" => Ok(Target::Ap),
            "ec" => Ok(Target::Ec),
            "h1" => Ok(Target::H1),
            "ap_custom" | "ap-custom" => Ok(Target::ApCustom),
            _ => Err(format!("Unknown target: {}", s)),
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Ap => write!(f, "ap"),
            Target::Ec => write!(f, "ec"),
            Target::H1 => write!(f, "h1"),
            Target::ApCustom => write!(f, "ap_custom"),
        }
    }
}

// ===========================================================================
// Packet Structures
// ===========================================================================

/// Protocol V1 command packet
#[derive(Clone)]
pub struct CommandV1 {
    /// Number of bytes to write (actual count, not 0-based)
    pub write_count: u8,
    /// Number of bytes to read (0xFF = full duplex)
    pub read_count: u8,
    /// Write data (up to 62 bytes)
    pub data: [u8; V1_MAX_PAYLOAD],
}

impl Default for CommandV1 {
    fn default() -> Self {
        Self {
            write_count: 0,
            read_count: 0,
            data: [0; V1_MAX_PAYLOAD],
        }
    }
}

impl CommandV1 {
    /// Create a new V1 command
    pub fn new(write_data: &[u8], read_count: u8) -> Self {
        let mut cmd = Self {
            write_count: write_data.len() as u8,
            read_count,
            ..Default::default()
        };
        let len = std::cmp::min(write_data.len(), V1_MAX_PAYLOAD);
        cmd.data[..len].copy_from_slice(&write_data[..len]);
        cmd
    }

    /// Serialize to a variable-length packet
    ///
    /// Returns only the bytes needed: 2-byte header + actual write data
    pub fn to_bytes(&self) -> Vec<u8> {
        let len = 2 + self.write_count as usize;
        let mut buf = vec![0u8; len];
        buf[0] = self.write_count;
        buf[1] = self.read_count;
        if self.write_count > 0 {
            buf[2..len].copy_from_slice(&self.data[..self.write_count as usize]);
        }
        buf
    }
}

/// Protocol V1 response packet
#[derive(Clone)]
pub struct ResponseV1 {
    /// Status code
    pub status_code: u16,
    /// Read data (up to 62 bytes)
    pub data: [u8; V1_MAX_PAYLOAD],
}

impl Default for ResponseV1 {
    fn default() -> Self {
        Self {
            status_code: 0,
            data: [0; V1_MAX_PAYLOAD],
        }
    }
}

impl ResponseV1 {
    /// Parse from a 64-byte packet
    pub fn from_bytes(buf: &[u8]) -> Self {
        let mut rsp = Self::default();
        if buf.len() >= 2 {
            rsp.status_code = u16::from_le_bytes([buf[0], buf[1]]);
        }
        if buf.len() >= 64 {
            rsp.data.copy_from_slice(&buf[2..64]);
        } else if buf.len() > 2 {
            rsp.data[..buf.len() - 2].copy_from_slice(&buf[2..]);
        }
        rsp
    }

    /// Get the status as a StatusCode enum
    pub fn status(&self) -> StatusCode {
        StatusCode::from_u16(self.status_code)
    }
}

/// Protocol V2 transfer start command packet
#[derive(Clone)]
pub struct CommandV2Start {
    /// Packet ID (should be CmdTransferStart)
    pub packet_id: u16,
    /// Total number of bytes to write
    pub write_count: u16,
    /// Total number of bytes to read (0xFFFF = full duplex)
    pub read_count: u16,
    /// Number of valid bytes in `data`
    pub data_len: usize,
    /// First chunk of write data (up to 58 bytes)
    pub data: [u8; V2_START_PAYLOAD],
}

impl Default for CommandV2Start {
    fn default() -> Self {
        Self {
            packet_id: PacketId::CmdTransferStart as u16,
            write_count: 0,
            read_count: 0,
            data_len: 0,
            data: [0; V2_START_PAYLOAD],
        }
    }
}

impl CommandV2Start {
    /// Create a new V2 start command
    pub fn new(write_count: u16, read_count: u16, first_data: &[u8]) -> Self {
        let mut cmd = Self {
            write_count,
            read_count,
            ..Default::default()
        };
        let len = std::cmp::min(first_data.len(), V2_START_PAYLOAD);
        cmd.data_len = len;
        cmd.data[..len].copy_from_slice(&first_data[..len]);
        cmd
    }

    /// Serialize to a variable-length packet.
    ///
    /// The device uses the USB transfer length to determine how much write
    /// payload is present, so the final packet must not be padded to 64 bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let header = CommandV2StartHeader {
            packet_id: U16Le::new(self.packet_id),
            write_count: U16Le::new(self.write_count),
            read_count: U16Le::new(self.read_count),
        };
        let mut packet = Vec::with_capacity(size_of::<CommandV2StartHeader>() + self.data_len);
        packet.extend_from_slice(header.as_bytes());
        packet.extend_from_slice(&self.data[..self.data_len]);
        packet
    }
}

/// Protocol V2 transfer continue command packet
#[derive(Clone)]
pub struct CommandV2Continue {
    /// Packet ID (should be CmdTransferContinue)
    pub packet_id: u16,
    /// Byte offset for validation
    pub data_index: u16,
    /// Number of valid bytes in `data`
    pub data_len: usize,
    /// Additional write data (up to 60 bytes)
    pub data: [u8; V2_CONTINUE_PAYLOAD],
}

impl Default for CommandV2Continue {
    fn default() -> Self {
        Self {
            packet_id: PacketId::CmdTransferContinue as u16,
            data_index: 0,
            data_len: 0,
            data: [0; V2_CONTINUE_PAYLOAD],
        }
    }
}

impl CommandV2Continue {
    /// Create a new V2 continue command
    pub fn new(data_index: u16, data: &[u8]) -> Self {
        let mut cmd = Self {
            data_index,
            ..Default::default()
        };
        let len = std::cmp::min(data.len(), V2_CONTINUE_PAYLOAD);
        cmd.data_len = len;
        cmd.data[..len].copy_from_slice(&data[..len]);
        cmd
    }

    /// Serialize to a variable-length packet.
    pub fn to_bytes(&self) -> Vec<u8> {
        let header = PacketV2ContinueHeader {
            packet_id: U16Le::new(self.packet_id),
            data_index: U16Le::new(self.data_index),
        };
        let mut packet = Vec::with_capacity(size_of::<PacketV2ContinueHeader>() + self.data_len);
        packet.extend_from_slice(header.as_bytes());
        packet.extend_from_slice(&self.data[..self.data_len]);
        packet
    }
}

/// Protocol V2 restart response command
#[derive(Clone)]
pub struct CommandV2Restart {
    /// Packet ID (should be CmdRestartResponse)
    pub packet_id: u16,
}

impl Default for CommandV2Restart {
    fn default() -> Self {
        Self {
            packet_id: PacketId::CmdRestartResponse as u16,
        }
    }
}

impl CommandV2Restart {
    /// Serialize the two-byte command header.
    pub fn to_bytes(&self) -> Vec<u8> {
        PacketV2Header {
            packet_id: U16Le::new(self.packet_id),
        }
        .as_bytes()
        .to_vec()
    }
}

/// Protocol V2 get config command
#[derive(Clone)]
pub struct CommandV2GetConfig {
    /// Packet ID (should be CmdGetUsbSpiConfig)
    pub packet_id: u16,
}

impl Default for CommandV2GetConfig {
    fn default() -> Self {
        Self {
            packet_id: PacketId::CmdGetUsbSpiConfig as u16,
        }
    }
}

impl CommandV2GetConfig {
    /// Serialize the two-byte command header.
    pub fn to_bytes(&self) -> Vec<u8> {
        PacketV2Header {
            packet_id: U16Le::new(self.packet_id),
        }
        .as_bytes()
        .to_vec()
    }
}

/// Protocol V2 transfer start response packet
#[derive(Clone)]
pub struct ResponseV2Start {
    /// Packet ID (should be RspTransferStart)
    pub packet_id: u16,
    /// Status code
    pub status_code: u16,
    /// First chunk of read data (up to 60 bytes)
    pub data: [u8; V2_CONTINUE_PAYLOAD],
}

impl Default for ResponseV2Start {
    fn default() -> Self {
        Self {
            packet_id: 0,
            status_code: 0,
            data: [0; V2_CONTINUE_PAYLOAD],
        }
    }
}

impl ResponseV2Start {
    /// Parse from a 64-byte packet
    pub fn from_bytes(buf: &[u8]) -> Self {
        let Ok((header, payload)) = ResponseV2StartHeader::ref_from_prefix(buf) else {
            return Self::default();
        };
        let mut rsp = Self {
            packet_id: header.packet_id.get(),
            status_code: header.status_code.get(),
            ..Default::default()
        };
        let data_len = payload.len().min(V2_CONTINUE_PAYLOAD);
        rsp.data[..data_len].copy_from_slice(&payload[..data_len]);
        rsp
    }

    /// Get the status as a StatusCode enum
    pub fn status(&self) -> StatusCode {
        StatusCode::from_u16(self.status_code)
    }
}

/// Protocol V2 transfer continue response packet
#[derive(Clone)]
pub struct ResponseV2Continue {
    /// Packet ID (should be RspTransferContinue)
    pub packet_id: u16,
    /// Byte offset for validation
    pub data_index: u16,
    /// Additional read data (up to 60 bytes)
    pub data: [u8; V2_CONTINUE_PAYLOAD],
}

impl Default for ResponseV2Continue {
    fn default() -> Self {
        Self {
            packet_id: 0,
            data_index: 0,
            data: [0; V2_CONTINUE_PAYLOAD],
        }
    }
}

impl ResponseV2Continue {
    /// Parse from a 64-byte packet
    pub fn from_bytes(buf: &[u8]) -> Self {
        let Ok((header, payload)) = PacketV2ContinueHeader::ref_from_prefix(buf) else {
            return Self::default();
        };
        let mut rsp = Self {
            packet_id: header.packet_id.get(),
            data_index: header.data_index.get(),
            ..Default::default()
        };
        let data_len = payload.len().min(V2_CONTINUE_PAYLOAD);
        rsp.data[..data_len].copy_from_slice(&payload[..data_len]);
        rsp
    }
}

/// Protocol V2 configuration response packet
#[derive(Clone, Debug, Default)]
pub struct ResponseV2Config {
    /// Packet ID (should be RspUsbSpiConfig)
    pub packet_id: u16,
    /// Maximum write count supported
    pub max_write_count: u16,
    /// Maximum read count supported
    pub max_read_count: u16,
    /// Feature bitmap (see features module)
    pub feature_bitmap: u16,
}

impl ResponseV2Config {
    /// Parse from a 64-byte packet
    pub fn from_bytes(buf: &[u8]) -> Self {
        let Ok((wire, _)) = ResponseV2ConfigWire::ref_from_prefix(buf) else {
            return Self::default();
        };
        Self {
            packet_id: wire.packet_id.get(),
            max_write_count: wire.max_write_count.get(),
            max_read_count: wire.max_read_count.get(),
            feature_bitmap: wire.feature_bitmap.get(),
        }
    }

    /// Check if full duplex is supported
    pub fn supports_full_duplex(&self) -> bool {
        self.feature_bitmap & features::FULL_DUPLEX != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_header_only_commands_are_two_bytes() {
        assert_eq!(CommandV2GetConfig::default().to_bytes(), [0, 0]);
        assert_eq!(CommandV2Restart::default().to_bytes(), [4, 0]);
    }

    #[test]
    fn v2_start_packet_contains_only_valid_payload() {
        let cmd = CommandV2Start::new(3, 2, &[0x9f, 0xaa, 0x55]);
        assert_eq!(cmd.to_bytes(), vec![2, 0, 3, 0, 2, 0, 0x9f, 0xaa, 0x55]);
    }

    #[test]
    fn v2_continue_packet_contains_only_valid_payload() {
        let cmd = CommandV2Continue::new(58, &[0xde, 0xad]);
        assert_eq!(cmd.to_bytes(), vec![3, 0, 58, 0, 0xde, 0xad]);
    }
}

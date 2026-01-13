//! Interface Control Document for postcard-spi programmer protocol
//!
//! This crate defines the message types and RPC endpoints shared between
//! the host (PC) and firmware (Pico) for the postcard-spi programmer.
//!
//! The protocol supports multi-I/O SPI modes (1-1-1, 1-1-2, 1-2-2, 1-1-4, 1-4-4, 4-4-4)
//! and multiple chip select lines.

#![no_std]

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

/// Current protocol version
pub const PROTOCOL_VERSION: u16 = 1;

/// Default USB VID (shared testing VID from pid.codes)
pub const USB_VID: u16 = 0x16c0;

/// Default USB PID (shared testing PID)
pub const USB_PID: u16 = 0x27DD;

// ============================================================================
// ENUMS
// ============================================================================

/// SPI I/O mode for transactions
///
/// Specifies how many data lines are used for each phase of the transaction.
/// The notation X-Y-Z means:
/// - X lines for command phase
/// - Y lines for address phase  
/// - Z lines for data phase
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Schema)]
#[repr(u8)]
pub enum IoMode {
    /// Standard SPI: 1-1-1 (command, address, data all on single line)
    #[default]
    Single = 0,
    /// Dual Output: 1-1-2 (command and address single, data on 2 lines)
    DualOut = 1,
    /// Dual I/O: 1-2-2 (command single, address and data on 2 lines)
    DualIo = 2,
    /// Quad Output: 1-1-4 (command and address single, data on 4 lines)
    QuadOut = 3,
    /// Quad I/O: 1-4-4 (command single, address and data on 4 lines)
    QuadIo = 4,
    /// QPI mode: 4-4-4 (everything on 4 lines)
    Qpi = 5,
}

impl IoMode {
    /// Returns the number of data lines used for the command phase
    pub const fn cmd_lines(&self) -> u8 {
        match self {
            Self::Single | Self::DualOut | Self::DualIo | Self::QuadOut | Self::QuadIo => 1,
            Self::Qpi => 4,
        }
    }

    /// Returns the number of data lines used for the address phase
    pub const fn addr_lines(&self) -> u8 {
        match self {
            Self::Single | Self::DualOut | Self::QuadOut => 1,
            Self::DualIo => 2,
            Self::QuadIo | Self::Qpi => 4,
        }
    }

    /// Returns the number of data lines used for the data phase
    pub const fn data_lines(&self) -> u8 {
        match self {
            Self::Single => 1,
            Self::DualOut | Self::DualIo => 2,
            Self::QuadOut | Self::QuadIo | Self::Qpi => 4,
        }
    }
}

/// Address width for SPI commands
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Schema)]
#[repr(u8)]
pub enum AddressWidth {
    /// No address phase
    #[default]
    None = 0,
    /// 3-byte (24-bit) address
    ThreeByte = 3,
    /// 4-byte (32-bit) address
    FourByte = 4,
}

impl AddressWidth {
    /// Returns the number of address bytes
    pub const fn bytes(&self) -> u8 {
        match self {
            Self::None => 0,
            Self::ThreeByte => 3,
            Self::FourByte => 4,
        }
    }
}

/// I/O mode capability flags (bitmap for DeviceInfo)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Schema)]
pub struct IoModeFlags(pub u8);

impl IoModeFlags {
    pub const SINGLE: u8 = 0x01;
    pub const DUAL_OUT: u8 = 0x02;
    pub const DUAL_IO: u8 = 0x04;
    pub const QUAD_OUT: u8 = 0x08;
    pub const QUAD_IO: u8 = 0x10;
    pub const QPI: u8 = 0x20;

    /// All single-line and dual modes
    pub const DUAL: u8 = Self::SINGLE | Self::DUAL_OUT | Self::DUAL_IO;
    /// All modes including quad
    pub const QUAD: u8 = Self::DUAL | Self::QUAD_OUT | Self::QUAD_IO;
    /// All modes including QPI
    pub const ALL: u8 = Self::QUAD | Self::QPI;

    pub const fn new(flags: u8) -> Self {
        Self(flags)
    }

    pub const fn supports(&self, mode: IoMode) -> bool {
        let flag = match mode {
            IoMode::Single => Self::SINGLE,
            IoMode::DualOut => Self::DUAL_OUT,
            IoMode::DualIo => Self::DUAL_IO,
            IoMode::QuadOut => Self::QUAD_OUT,
            IoMode::QuadIo => Self::QUAD_IO,
            IoMode::Qpi => Self::QPI,
        };
        (self.0 & flag) != 0
    }

    pub const fn contains(&self, flag: u8) -> bool {
        (self.0 & flag) != 0
    }
}

// ============================================================================
// MESSAGE TYPES
// ============================================================================

/// Device information and capabilities (response to GetInfo)
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct DeviceInfo {
    /// Device name (null-terminated ASCII)
    pub name: [u8; 16],
    /// Protocol version
    pub version: u16,
    /// Maximum bytes per single transfer
    pub max_transfer_size: u32,
    /// Number of chip select lines available
    pub num_cs: u8,
    /// Currently selected chip select
    pub current_cs: u8,
    /// Supported I/O modes (bitmap)
    pub supported_modes: IoModeFlags,
    /// Current SPI clock frequency in Hz
    pub current_speed_hz: u32,
}

impl DeviceInfo {
    /// Get the device name as a string slice
    pub fn name_str(&self) -> &str {
        let len = self.name.iter().position(|&c| c == 0).unwrap_or(16);
        core::str::from_utf8(&self.name[..len]).unwrap_or("(invalid)")
    }
}

/// Request to set SPI clock speed
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct SetSpeedReq {
    /// Desired speed in Hz
    pub hz: u32,
}

/// Response to SetSpeed request
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct SetSpeedResp {
    /// Actual speed that was set (may differ from requested)
    pub actual_hz: u32,
}

/// Request to select a chip select line
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct SetCsReq {
    /// Chip select index (0-based)
    pub cs: u8,
}

/// Request to delay for a specified time
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct DelayReq {
    /// Delay in microseconds
    pub us: u32,
}

/// SPI transfer request
///
/// This defines a complete SPI transaction with optional address,
/// dummy cycles, write data, and read data.
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct SpiTransferReq {
    /// SPI opcode byte
    pub opcode: u8,
    /// Optional address (None if no address phase)
    pub address: Option<u32>,
    /// Address width
    pub address_width: AddressWidth,
    /// I/O mode for the transaction
    pub io_mode: IoMode,
    /// Number of dummy clock cycles after address
    pub dummy_cycles: u8,
    /// Number of bytes to write (data follows in WriteData topic)
    pub write_len: u16,
    /// Number of bytes to read (data returned in ReadData topic)
    pub read_len: u16,
}

/// SPI transfer response
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct SpiTransferResp {
    /// Whether the transfer completed successfully
    pub success: bool,
    /// Number of bytes actually read
    pub bytes_read: u16,
}

/// Wire error type for failed RPC calls
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub enum SpiWireError {
    /// Invalid chip select index
    InvalidCs,
    /// Requested I/O mode not supported
    UnsupportedIoMode,
    /// Transfer size exceeds maximum
    TransferTooLarge,
    /// Hardware error during transfer
    HardwareError,
    /// Device is busy
    Busy,
    /// Invalid request parameters
    InvalidRequest,
    /// Deserialization error
    DeserializeError,
}

// ============================================================================
// ENDPOINT DEFINITIONS
// ============================================================================

use postcard_rpc::endpoints;

endpoints! {
    list = ENDPOINT_LIST;
    | EndpointTy              | RequestTy               | ResponseTy              | Path                        |
    | ----------              | ---------               | ----------              | ----                        |
    | GetInfoEndpoint         | ()                      | DeviceInfo              | "postcard_spi/info"         |
    | SetSpeedEndpoint        | SetSpeedReq             | SetSpeedResp            | "postcard_spi/set_speed"    |
    | SetCsEndpoint           | SetCsReq                | ()                      | "postcard_spi/set_cs"       |
    | DelayEndpoint           | DelayReq                | ()                      | "postcard_spi/delay"        |
    | SpiTransferEndpoint     | SpiTransferReq          | SpiTransferResp         | "postcard_spi/transfer"     |
    | SpiTransferDataEndpoint | SpiTransferReqWithData  | SpiTransferRespWithData | "postcard_spi/transfer_data"|
}

/// Maximum data size for inline transfers (1KB)
pub const MAX_INLINE_DATA: usize = 1024;

/// SPI transfer request with inline write data
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct SpiTransferReqWithData {
    /// The transfer parameters
    pub req: SpiTransferReq,
    /// Write data (can be empty, max MAX_INLINE_DATA bytes)
    pub write_data: heapless::Vec<u8, MAX_INLINE_DATA>,
}

/// SPI transfer response with inline read data
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct SpiTransferRespWithData {
    /// The transfer result
    pub resp: SpiTransferResp,
    /// Read data (can be empty, max MAX_INLINE_DATA bytes)
    pub read_data: heapless::Vec<u8, MAX_INLINE_DATA>,
}

// ============================================================================
// TOPIC LISTS (for define_dispatch! macro)
// ============================================================================

use postcard_rpc::{topics, TopicDirection};

topics! {
    list = TOPICS_IN_LIST;
    direction = TopicDirection::ToServer;
    | TopicTy   | MessageTy | Path |
    | -------   | --------- | ---- |
}

topics! {
    list = TOPICS_OUT_LIST;
    direction = TopicDirection::ToClient;
    | TopicTy   | MessageTy | Path |
    | -------   | --------- | ---- |
}

// ============================================================================
// CONVERSION HELPERS
// ============================================================================

/// Convert from rflasher-core IoMode to our IoMode
/// (This is a helper for the host crate)
#[cfg(feature = "std")]
impl From<u8> for IoMode {
    fn from(v: u8) -> Self {
        match v {
            0 => IoMode::Single,
            1 => IoMode::DualOut,
            2 => IoMode::DualIo,
            3 => IoMode::QuadOut,
            4 => IoMode::QuadIo,
            5 => IoMode::Qpi,
            _ => IoMode::Single,
        }
    }
}

#[cfg(feature = "std")]
impl From<u8> for AddressWidth {
    fn from(v: u8) -> Self {
        match v {
            0 => AddressWidth::None,
            3 => AddressWidth::ThreeByte,
            4 => AddressWidth::FourByte,
            _ => AddressWidth::None,
        }
    }
}

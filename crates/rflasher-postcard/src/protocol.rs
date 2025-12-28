//! Postcard-RPC protocol definitions for flash programming
//!
//! This module defines the RPC endpoints and message types for communication
//! between the host and a microcontroller-based flash programmer.
//!
//! The protocol is designed to be:
//! - `no_std` compatible (uses `heapless` for buffers)
//! - Flexible for multi-I/O modes (Single, Dual, Quad, QPI)
//! - USB transport focused (via postcard-rpc)

use heapless::{String, Vec};
use postcard_rpc::{endpoints, topics, TopicDirection};
use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

// ============================================================================
// Constants
// ============================================================================

/// Maximum SPI data payload size (1KB)
pub const MAX_SPI_DATA: usize = 1024;

/// Maximum programmer name length
pub const MAX_NAME_LEN: usize = 32;

/// Maximum version string length
pub const MAX_VERSION_LEN: usize = 16;

/// Maximum error message length
pub const MAX_ERROR_MSG_LEN: usize = 64;

// ============================================================================
// I/O Mode
// ============================================================================

/// SPI I/O mode for transactions
///
/// These modes specify how data is transferred on the SPI bus, following
/// the standard notation (cmd-addr-data lines):
/// - Single (1-1-1): Standard SPI
/// - DualOut (1-1-2): Data phase on 2 lines (read only)
/// - DualIo (1-2-2): Address and data on 2 lines
/// - QuadOut (1-1-4): Data phase on 4 lines (read only)
/// - QuadIo (1-4-4): Address and data on 4 lines
/// - Qpi (4-4-4): Everything on 4 lines
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Schema, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum IoMode {
    /// Standard SPI: 1-1-1 (cmd, addr, data all on single line)
    #[default]
    Single = 0,
    /// Dual Output: 1-1-2 (data phase on 2 lines)
    DualOut = 1,
    /// Dual I/O: 1-2-2 (addr and data on 2 lines)
    DualIo = 2,
    /// Quad Output: 1-1-4 (data phase on 4 lines)
    QuadOut = 3,
    /// Quad I/O: 1-4-4 (addr and data on 4 lines)
    QuadIo = 4,
    /// QPI mode: 4-4-4 (everything on 4 lines)
    Qpi = 5,
}

impl IoMode {
    /// Convert from rflasher-core IoMode
    #[cfg(feature = "rflasher-core")]
    pub fn from_core(mode: rflasher_core::spi::IoMode) -> Self {
        match mode {
            rflasher_core::spi::IoMode::Single => Self::Single,
            rflasher_core::spi::IoMode::DualOut => Self::DualOut,
            rflasher_core::spi::IoMode::DualIo => Self::DualIo,
            rflasher_core::spi::IoMode::QuadOut => Self::QuadOut,
            rflasher_core::spi::IoMode::QuadIo => Self::QuadIo,
            rflasher_core::spi::IoMode::Qpi => Self::Qpi,
        }
    }

    /// Convert to rflasher-core IoMode
    #[cfg(feature = "rflasher-core")]
    pub fn to_core(self) -> rflasher_core::spi::IoMode {
        match self {
            Self::Single => rflasher_core::spi::IoMode::Single,
            Self::DualOut => rflasher_core::spi::IoMode::DualOut,
            Self::DualIo => rflasher_core::spi::IoMode::DualIo,
            Self::QuadOut => rflasher_core::spi::IoMode::QuadOut,
            Self::QuadIo => rflasher_core::spi::IoMode::QuadIo,
            Self::Qpi => rflasher_core::spi::IoMode::Qpi,
        }
    }
}

// ============================================================================
// Address Width
// ============================================================================

/// Address width for SPI commands
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Schema, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum AddressWidth {
    /// No address phase
    #[default]
    None = 0,
    /// 3-byte address (24 bits) - supports up to 16 MiB
    ThreeByte = 3,
    /// 4-byte address (32 bits) - supports up to 4 GiB
    FourByte = 4,
}

impl AddressWidth {
    /// Get the number of bytes for this address width
    pub const fn bytes(&self) -> u8 {
        match self {
            Self::None => 0,
            Self::ThreeByte => 3,
            Self::FourByte => 4,
        }
    }

    /// Convert from rflasher-core AddressWidth
    #[cfg(feature = "rflasher-core")]
    pub fn from_core(width: rflasher_core::spi::AddressWidth) -> Self {
        match width {
            rflasher_core::spi::AddressWidth::None => Self::None,
            rflasher_core::spi::AddressWidth::ThreeByte => Self::ThreeByte,
            rflasher_core::spi::AddressWidth::FourByte => Self::FourByte,
        }
    }
}

// ============================================================================
// Feature Flags
// ============================================================================

/// Programmer feature flags for multi-I/O modes
///
/// 4-byte addressing is always supported and not a feature flag.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Schema, PartialEq, Eq, Default)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SpiFeatures {
    /// Raw feature bits
    pub bits: u32,
}

impl SpiFeatures {
    /// Can read two bits at once (1-1-2 mode)
    pub const DUAL_IN: u32 = 1 << 0;
    /// Can transfer two bits at once (1-2-2 mode)
    pub const DUAL_IO: u32 = 1 << 1;
    /// Can read four bits at once (1-1-4 mode)
    pub const QUAD_IN: u32 = 1 << 2;
    /// Can transfer four bits at once (1-4-4 mode)
    pub const QUAD_IO: u32 = 1 << 3;
    /// Can send commands with quad I/O (4-4-4 mode)
    pub const QPI: u32 = 1 << 4;

    /// Create empty features (standard SPI only)
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Create with raw bits
    pub const fn from_bits(bits: u32) -> Self {
        Self { bits }
    }

    /// Check if a feature is set
    pub const fn contains(&self, flag: u32) -> bool {
        (self.bits & flag) != 0
    }
}

// ============================================================================
// Message Types
// ============================================================================

/// Device information response
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct DeviceInfo {
    /// Programmer name
    pub name: String<MAX_NAME_LEN>,
    /// Firmware version
    pub version: String<MAX_VERSION_LEN>,
    /// Maximum SPI frequency in Hz
    pub max_spi_freq: u32,
    /// Programmer feature flags (multi-I/O modes)
    pub features: SpiFeatures,
    /// Maximum read length per transaction
    pub max_read_len: u32,
    /// Maximum write length per transaction
    pub max_write_len: u32,
}

/// Set SPI frequency request
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SetSpiFreq {
    /// Requested frequency in Hz
    pub freq_hz: u32,
}

/// Set SPI frequency response
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SetSpiFreqResp {
    /// Actual frequency set by the programmer (may differ from requested)
    pub actual_freq: u32,
}

/// Set chip select request
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SetChipSelect {
    /// Chip select number (0-255)
    pub cs: u8,
}

/// Set pin state (enable/disable output drivers)
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SetPinState {
    /// true = enabled, false = disabled
    pub enabled: bool,
}

/// SPI operation request
///
/// This is the main operation for SPI flash programming. It represents
/// a complete SPI transaction with support for multi-I/O modes.
///
/// The transaction structure is:
/// 1. Assert CS
/// 2. Send opcode (1 byte, using io_mode.cmd_lines)
/// 3. Send address (if present, using io_mode.addr_lines)
/// 4. Wait dummy_cycles (if any)
/// 5. Write write_data and/or read read_count bytes (using io_mode.data_lines)
/// 6. Deassert CS
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SpiOp {
    /// Command opcode
    pub opcode: u8,
    /// Address (if any) - interpretation depends on address_width
    pub address: u32,
    /// Address width
    pub address_width: AddressWidth,
    /// I/O mode for this transaction
    pub io_mode: IoMode,
    /// Number of dummy cycles after address (typically 0-8)
    pub dummy_cycles: u8,
    /// Data to write after the header
    pub write_data: Vec<u8, MAX_SPI_DATA>,
    /// Number of bytes to read back
    pub read_count: u16,
}

/// SPI operation response
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct SpiOpResp {
    /// Data read from the SPI bus
    pub read_data: Vec<u8, MAX_SPI_DATA>,
}

/// Delay request (microseconds)
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct DelayUs {
    /// Microseconds to delay
    pub us: u32,
}

/// Generic acknowledgment (for commands with no response data)
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Ack;

// ============================================================================
// Error Types
// ============================================================================

/// Error codes returned by the programmer
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Schema, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub enum ErrorCode {
    /// Unknown/unspecified error
    Unknown = 0,
    /// Invalid parameter
    InvalidParam = 1,
    /// Operation not supported
    NotSupported = 2,
    /// SPI bus error
    SpiBusError = 3,
    /// Timeout
    Timeout = 4,
    /// Buffer overflow
    BufferOverflow = 5,
    /// I/O mode not supported
    IoModeNotSupported = 6,
}

/// Error response from the programmer
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ErrorResp {
    /// Error code
    pub code: ErrorCode,
    /// Optional error message
    pub message: Option<String<MAX_ERROR_MSG_LEN>>,
}

// ============================================================================
// Result Types for Endpoints
// ============================================================================

/// Result type for operations that can fail
pub type SpiResult<T> = Result<T, ErrorResp>;

/// Result type for SetSpiFreq endpoint
pub type SetSpiFreqResult = Result<SetSpiFreqResp, ErrorResp>;

/// Result type for Ack-only endpoints
pub type AckResult = Result<Ack, ErrorResp>;

/// Result type for SpiOp endpoint
pub type SpiOpResult = Result<SpiOpResp, ErrorResp>;

/// Log message type
pub type LogMessage = String<128>;

// ============================================================================
// Endpoint Definitions
// ============================================================================

endpoints! {
    list = ENDPOINT_LIST;

    | EndpointTy        | RequestTy      | ResponseTy        | Path              |
    | ----------        | ---------      | ----------        | ----              |
    | GetDeviceInfo     | ()             | DeviceInfo        | "info/device"     |
    | SetSpiFreqEp      | SetSpiFreq     | SetSpiFreqResult  | "spi/set_freq"    |
    | SetChipSelectEp   | SetChipSelect  | AckResult         | "spi/set_cs"      |
    | SetPinStateEp     | SetPinState    | AckResult         | "spi/set_pin"     |
    | SpiOpEp           | SpiOp          | SpiOpResult       | "spi/op"          |
    | DelayUsEp         | DelayUs        | Ack               | "delay_us"        |
    | SpiResetEp        | ()             | AckResult         | "spi/reset"       |
}

// ============================================================================
// Topic Definitions (for future use - logging, events, etc.)
// ============================================================================

topics! {
    list = TOPICS_IN_LIST;
    direction = TopicDirection::ToServer;

    | TopicTy           | MessageTy      | Path                    |
    | -------           | ---------      | ----                    |
}

topics! {
    list = TOPICS_OUT_LIST;
    direction = TopicDirection::ToClient;

    | TopicTy           | MessageTy      | Path                    |
    | -------           | ---------      | ----                    |
    | LogTopic          | LogMessage     | "log"                   |
}

// ============================================================================
// USB Configuration
// ============================================================================

/// USB Vendor ID (using test VID - replace with your own for production)
pub const USB_VID: u16 = 0x16c0;

/// USB Product ID (using test PID - replace with your own for production)
pub const USB_PID: u16 = 0x27dd;

/// USB manufacturer string
pub const USB_MANUFACTURER: &str = "rflasher";

/// USB product string
pub const USB_PRODUCT: &str = "rflasher-postcard";

/// USB interface class (vendor-specific)
pub const USB_CLASS: u8 = 0xFF;

/// USB interface subclass
pub const USB_SUBCLASS: u8 = 0x00;

/// USB interface protocol
pub const USB_PROTOCOL: u8 = 0x00;

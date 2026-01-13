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
    | BatchEndpoint           | BatchRequest            | BatchResponse           | "postcard_spi/batch"        |
}

// ============================================================================
// BATCH OPERATIONS
// ============================================================================

/// Maximum number of operations in a batch
pub const MAX_BATCH_OPS: usize = 32;

/// Maximum read results we can return in a batch
pub const MAX_BATCH_READS: usize = 16;

/// Maximum bytes per read result in a batch
pub const MAX_BATCH_READ_SIZE: usize = 64;

/// Maximum write data per transaction in a batch (256 = typical flash page size)
pub const MAX_BATCH_TX_DATA: usize = 256;

/// A complete SPI transaction for use in batches
///
/// Each transaction is a complete command with automatic CS handling:
/// 1. Assert CS
/// 2. Send opcode (on cmd lines per io_mode)
/// 3. Send address if present (on addr lines per io_mode)
/// 4. Send dummy cycles
/// 5. Write data OR read data (on data lines per io_mode)
/// 6. Deassert CS
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct SpiTransaction {
    /// SPI opcode byte
    pub opcode: u8,
    /// Optional address (None = no address phase)
    pub address: Option<u32>,
    /// Address width (ignored if address is None)
    pub address_width: AddressWidth,
    /// I/O mode for the transaction (determines lines used for each phase)
    pub io_mode: IoMode,
    /// Number of dummy clock cycles after address (0 = none)
    pub dummy_cycles: u8,
    /// Data to write after opcode/address/dummy (empty for read-only commands)
    pub write_data: heapless::Vec<u8, MAX_BATCH_TX_DATA>,
    /// Number of bytes to read (0 for write-only commands)
    /// Note: Most commands are either write OR read, not both
    pub read_len: u8,
}

impl SpiTransaction {
    /// Create a simple command with no address or data (e.g., WREN, WRDI)
    pub fn cmd(opcode: u8) -> Self {
        Self {
            opcode,
            address: None,
            address_width: AddressWidth::None,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: heapless::Vec::new(),
            read_len: 0,
        }
    }

    /// Create a read command (e.g., RDSR, RDID)
    pub fn read(opcode: u8, len: u8) -> Self {
        Self {
            opcode,
            address: None,
            address_width: AddressWidth::None,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: heapless::Vec::new(),
            read_len: len,
        }
    }

    /// Create a write command with data (e.g., WRSR)
    pub fn write(opcode: u8, data: &[u8]) -> Self {
        let mut write_data = heapless::Vec::new();
        let _ = write_data.extend_from_slice(data);
        Self {
            opcode,
            address: None,
            address_width: AddressWidth::None,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data,
            read_len: 0,
        }
    }

    /// Set the I/O mode
    pub fn with_mode(mut self, mode: IoMode) -> Self {
        self.io_mode = mode;
        self
    }

    /// Set address with specified width
    pub fn with_addr(mut self, addr: u32, width: AddressWidth) -> Self {
        self.address = Some(addr);
        self.address_width = width;
        self
    }

    /// Set dummy cycles
    pub fn with_dummy(mut self, cycles: u8) -> Self {
        self.dummy_cycles = cycles;
        self
    }
}

/// A single operation in a batch
///
/// Operations are executed sequentially. Each `Transact` operation
/// handles CS automatically (assert before, deassert after).
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub enum BatchOp {
    /// Execute a complete SPI transaction (CS is handled automatically)
    Transact(SpiTransaction),

    /// Delay for specified microseconds between transactions
    DelayUs(u32),

    /// Poll status register until condition met or timeout
    ///
    /// Executes: CS assert, write cmd, read 1 byte, CS deassert
    /// Repeats until (read_value & mask) == expected, or timeout
    Poll {
        /// Command byte to send (usually 0x05 for RDSR)
        cmd: u8,
        /// Mask to apply to read value
        mask: u8,
        /// Expected value after masking (usually 0 to wait for WIP=0)
        expected: u8,
        /// Timeout in milliseconds
        timeout_ms: u16,
    },

    /// Switch to a different chip select for subsequent operations
    SetCs(u8),
}

/// Result of a single batch operation
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub enum BatchOpResult {
    /// Operation completed successfully (no data returned)
    Ok,
    /// Transaction completed, read data returned
    Data(heapless::Vec<u8, MAX_BATCH_READ_SIZE>),
    /// Poll completed successfully, returns final status byte
    PollOk(u8),
    /// Poll timed out, returns last status byte read
    PollTimeout(u8),
    /// Error occurred during operation
    Error(BatchError),
}

/// Errors that can occur during batch operations
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Schema)]
pub enum BatchError {
    /// Invalid operation parameters
    InvalidParams,
    /// Buffer overflow (too much data to return)
    BufferOverflow,
    /// Hardware error during transaction
    HardwareError,
    /// Invalid chip select index
    InvalidCs,
}

/// Request for batch operations
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct BatchRequest {
    /// List of operations to execute sequentially
    pub ops: heapless::Vec<BatchOp, MAX_BATCH_OPS>,
}

/// Response from batch operations
#[derive(Debug, Clone, Serialize, Deserialize, Schema)]
pub struct BatchResponse {
    /// Results for each operation (same order as request)
    /// Note: Only operations that return data will have entries here
    pub results: heapless::Vec<BatchOpResult, MAX_BATCH_READS>,
    /// Number of operations successfully completed
    pub ops_completed: u8,
    /// Whether all operations completed successfully
    pub success: bool,
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

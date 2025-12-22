//! Error types for rflasher-core
//!
//! This module provides a no_std compatible error type that can be used
//! throughout the crate.

use core::fmt;

/// Core error type - no_std compatible, Copy for efficiency
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    // SPI errors
    /// SPI transfer failed
    SpiTransferFailed,
    /// SPI operation timed out
    SpiTimeout,
    /// Opcode is not supported by the programmer
    OpcodeNotSupported,

    // Chip errors
    /// Flash chip not found (JEDEC ID read failed or unknown)
    ChipNotFound,
    /// Flash chip detected but not supported
    ChipNotSupported,
    /// JEDEC ID does not match expected value
    JedecIdMismatch,

    // Operation errors
    /// Erase operation failed
    EraseError,
    /// Write/program operation failed
    WriteError,
    /// Verify operation failed (data mismatch)
    VerifyError,
    /// Operation timed out
    Timeout,

    // Address/size errors
    /// Address is beyond flash chip size
    AddressOutOfBounds,
    /// Operation requires aligned address or size
    InvalidAlignment,
    /// Provided buffer is too small for the operation
    BufferTooSmall,

    // Protection errors
    /// Flash chip is write protected
    WriteProtected,
    /// Specific region is protected
    RegionProtected,

    // Programmer errors
    /// Programmer is not ready (not initialized or busy)
    ProgrammerNotReady,
    /// General programmer error
    ProgrammerError,

    // I/O errors
    /// Read operation failed
    ReadError,
    /// I/O error occurred
    IoError,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SpiTransferFailed => write!(f, "SPI transfer failed"),
            Self::SpiTimeout => write!(f, "SPI operation timed out"),
            Self::OpcodeNotSupported => write!(f, "SPI opcode not supported by programmer"),
            Self::ChipNotFound => write!(f, "flash chip not found"),
            Self::ChipNotSupported => write!(f, "flash chip not supported"),
            Self::JedecIdMismatch => write!(f, "JEDEC ID mismatch"),
            Self::EraseError => write!(f, "erase operation failed"),
            Self::WriteError => write!(f, "write operation failed"),
            Self::VerifyError => write!(f, "verify failed: data mismatch"),
            Self::Timeout => write!(f, "operation timed out"),
            Self::AddressOutOfBounds => write!(f, "address out of bounds"),
            Self::InvalidAlignment => write!(f, "invalid alignment"),
            Self::BufferTooSmall => write!(f, "buffer too small"),
            Self::WriteProtected => write!(f, "flash chip is write protected"),
            Self::RegionProtected => write!(f, "region is protected"),
            Self::ProgrammerNotReady => write!(f, "programmer not ready"),
            Self::ProgrammerError => write!(f, "programmer error"),
            Self::ReadError => write!(f, "read operation failed"),
            Self::IoError => write!(f, "I/O error"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

/// Result type alias using the core Error type
pub type Result<T> = core::result::Result<T, Error>;

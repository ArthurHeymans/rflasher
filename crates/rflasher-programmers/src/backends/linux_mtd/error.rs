//! Error types for Linux MTD operations

use std::io;
use thiserror::Error;

/// Linux MTD-specific errors
#[derive(Debug, Error)]
pub enum LinuxMtdError {
    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// MTD device not found
    #[error("MTD device not found: {0}")]
    DeviceNotFound(String),

    /// MTD device type is not NOR flash
    #[error("MTD device type is not 'nor': {0}")]
    NotNorFlash(String),

    /// Failed to read sysfs attribute
    #[error("Failed to read sysfs attribute '{path}': {source}")]
    SysfsRead {
        path: String,
        #[source]
        source: io::Error,
    },

    /// Failed to parse sysfs attribute
    #[error("Failed to parse sysfs attribute '{path}': {value}")]
    SysfsParse { path: String, value: String },

    /// MTD size is not a power of 2
    #[error("MTD size is not a power of 2: {0}")]
    InvalidSize(u64),

    /// MTD erase size is not a power of 2
    #[error("MTD erase size is not a power of 2: {0}")]
    InvalidEraseSize(u64),

    /// Non-uniform erase regions are not supported
    #[error("MTD device has non-uniform erase regions (count: {0}), which is not supported")]
    NonUniformEraseRegions(u64),

    /// Device is not writable
    #[error("MTD device is not writable")]
    NotWritable,

    /// Device does not support erase
    #[error("MTD device does not support erase operations")]
    NoEraseSupport,

    /// Erase operation failed
    #[error("Erase operation failed at offset {offset:#x}: {source}")]
    EraseFailed {
        offset: u32,
        #[source]
        source: nix::errno::Errno,
    },

    /// Seek error
    #[error("Seek to offset {offset:#x} failed: {source}")]
    SeekFailed {
        offset: u32,
        #[source]
        source: io::Error,
    },

    /// Read error
    #[error("Read of {len} bytes at offset {offset:#x} failed: {source}")]
    ReadFailed {
        offset: u32,
        len: usize,
        #[source]
        source: io::Error,
    },

    /// Write error
    #[error("Write of {len} bytes at offset {offset:#x} failed: {source}")]
    WriteFailed {
        offset: u32,
        len: usize,
        #[source]
        source: io::Error,
    },

    /// Missing required parameter
    #[error("Missing required parameter: {0}")]
    MissingParameter(&'static str),

    /// Invalid parameter value
    #[error("Invalid parameter '{name}': {message}")]
    InvalidParameter { name: &'static str, message: String },
}

/// Result type for Linux MTD operations
pub type Result<T> = std::result::Result<T, LinuxMtdError>;

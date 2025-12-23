//! Error types for serprog operations

#[cfg(not(feature = "std"))]
use alloc::string::String;

#[cfg(feature = "std")]
use thiserror::Error;

/// Serprog-specific errors
#[derive(Debug)]
#[cfg_attr(feature = "std", derive(Error))]
pub enum SerprogError {
    /// Failed to connect to device
    #[cfg_attr(feature = "std", error("Connection failed: {0}"))]
    ConnectionFailed(String),

    /// Failed to synchronize protocol
    #[cfg_attr(feature = "std", error("Protocol synchronization failed"))]
    SyncFailed,

    /// Unsupported protocol version
    #[cfg_attr(feature = "std", error("Unsupported protocol version: {0}"))]
    UnsupportedVersion(u16),

    /// Command not supported by programmer
    #[cfg_attr(feature = "std", error("Command 0x{0:02X} not supported"))]
    CommandNotSupported(u8),

    /// SPI bus not supported by programmer
    #[cfg_attr(feature = "std", error("SPI bus not supported by programmer"))]
    SpiNotSupported,

    /// NAK response received
    #[cfg_attr(feature = "std", error("NAK received for command 0x{0:02X}"))]
    Nak(u8),

    /// Invalid response received
    #[cfg_attr(
        feature = "std",
        error("Invalid response 0x{response:02X} for command 0x{command:02X}")
    )]
    InvalidResponse { command: u8, response: u8 },

    /// I/O error during communication
    #[cfg_attr(feature = "std", error("I/O error: {0}"))]
    IoError(String),

    /// Timeout during communication
    #[cfg_attr(feature = "std", error("Communication timeout"))]
    Timeout,

    /// Invalid parameter
    #[cfg_attr(feature = "std", error("Invalid parameter: {0}"))]
    InvalidParameter(String),

    /// Serial port error
    #[cfg(feature = "std")]
    #[cfg_attr(feature = "std", error("Serial port error: {0}"))]
    SerialError(#[from] serialport::Error),
}

/// Result type for serprog operations
pub type Result<T> = core::result::Result<T, SerprogError>;

#[cfg(feature = "std")]
impl From<std::io::Error> for SerprogError {
    fn from(e: std::io::Error) -> Self {
        SerprogError::IoError(e.to_string())
    }
}

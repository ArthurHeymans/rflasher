//! Error types for Raiden Debug SPI programmer

use std::fmt;

/// Result type for Raiden operations
pub type Result<T> = std::result::Result<T, RaidenError>;

/// Errors that can occur when using the Raiden Debug SPI programmer
#[derive(Debug)]
pub enum RaidenError {
    /// Device not found
    DeviceNotFound,
    /// Multiple devices found, serial number required
    MultipleDevicesFound(usize),
    /// Failed to open device
    OpenFailed(String),
    /// Failed to claim interface
    ClaimFailed(String),
    /// USB transfer failed
    TransferFailed(String),
    /// Invalid response from device
    InvalidResponse(String),
    /// Timeout during operation
    Timeout,
    /// Configuration error
    ConfigError(String),
    /// Protocol error (USB SPI status code)
    ProtocolError(u16),
    /// Failed to enable SPI bridge
    EnableFailed(String),
    /// Invalid parameter
    InvalidParameter(String),
    /// Unsupported protocol version
    UnsupportedProtocol(u8),
    /// Core library error
    Core(rflasher_core::error::Error),
}

impl RaidenError {
    /// Create a protocol error with a human-readable description
    pub fn from_status_code(code: u16) -> Self {
        RaidenError::ProtocolError(code)
    }
}

impl fmt::Display for RaidenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RaidenError::DeviceNotFound => {
                write!(
                    f,
                    "Raiden Debug SPI device not found (VID:18D1, subclass:51)"
                )
            }
            RaidenError::MultipleDevicesFound(count) => {
                write!(
                    f,
                    "Multiple Raiden devices found ({}), specify serial number",
                    count
                )
            }
            RaidenError::OpenFailed(msg) => write!(f, "Failed to open Raiden device: {}", msg),
            RaidenError::ClaimFailed(msg) => write!(f, "Failed to claim interface: {}", msg),
            RaidenError::TransferFailed(msg) => write!(f, "USB transfer failed: {}", msg),
            RaidenError::InvalidResponse(msg) => {
                write!(f, "Invalid response from Raiden device: {}", msg)
            }
            RaidenError::Timeout => write!(f, "Timeout during USB transfer"),
            RaidenError::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
            RaidenError::ProtocolError(code) => {
                let desc = match code {
                    0x0001 => "SPI timeout",
                    0x0002 => "SPI busy",
                    0x0003 => "Invalid write count",
                    0x0004 => "Invalid read count",
                    0x0005 => "SPI disabled",
                    0x0006 => "Bad data index in response",
                    0x0007 => "Data overflow",
                    0x0008 => "Unexpected packet",
                    0x0009 => "Full duplex not supported",
                    0x8000 => "Unknown error",
                    _ => "Unknown status code",
                };
                write!(f, "Protocol error 0x{:04X}: {}", code, desc)
            }
            RaidenError::EnableFailed(msg) => write!(f, "Failed to enable SPI bridge: {}", msg),
            RaidenError::InvalidParameter(msg) => write!(f, "Invalid parameter: {}", msg),
            RaidenError::UnsupportedProtocol(ver) => {
                write!(f, "Unsupported protocol version: {}", ver)
            }
            RaidenError::Core(e) => write!(f, "Core error: {}", e),
        }
    }
}

impl std::error::Error for RaidenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RaidenError::Core(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rflasher_core::error::Error> for RaidenError {
    fn from(e: rflasher_core::error::Error) -> Self {
        RaidenError::Core(e)
    }
}

impl From<nusb::Error> for RaidenError {
    fn from(e: nusb::Error) -> Self {
        RaidenError::TransferFailed(e.to_string())
    }
}

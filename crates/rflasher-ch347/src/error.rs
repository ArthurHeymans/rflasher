//! Error types for CH347 programmer

use std::fmt;

/// Result type for CH347 operations
pub type Result<T> = std::result::Result<T, Ch347Error>;

/// Errors that can occur when using the CH347 programmer
#[derive(Debug)]
pub enum Ch347Error {
    /// Device not found
    DeviceNotFound,
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
    /// Core library error
    Core(rflasher_core::error::Error),
}

impl fmt::Display for Ch347Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ch347Error::DeviceNotFound => {
                write!(f, "CH347 device not found (VID:1a86 PID:55db or 55de)")
            }
            Ch347Error::OpenFailed(msg) => write!(f, "Failed to open CH347: {}", msg),
            Ch347Error::ClaimFailed(msg) => write!(f, "Failed to claim interface: {}", msg),
            Ch347Error::TransferFailed(msg) => write!(f, "USB transfer failed: {}", msg),
            Ch347Error::InvalidResponse(msg) => {
                write!(f, "Invalid response from CH347: {}", msg)
            }
            Ch347Error::Timeout => write!(f, "Timeout during USB transfer"),
            Ch347Error::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
            Ch347Error::Core(e) => write!(f, "Core error: {}", e),
        }
    }
}

impl std::error::Error for Ch347Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Ch347Error::Core(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rflasher_core::error::Error> for Ch347Error {
    fn from(e: rflasher_core::error::Error) -> Self {
        Ch347Error::Core(e)
    }
}

impl From<nusb::Error> for Ch347Error {
    fn from(e: nusb::Error) -> Self {
        Ch347Error::TransferFailed(e.to_string())
    }
}

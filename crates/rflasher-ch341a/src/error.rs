//! Error types for CH341A programmer

use std::fmt;

/// Result type for CH341A operations
pub type Result<T> = std::result::Result<T, Ch341aError>;

/// Errors that can occur when using the CH341A programmer
#[derive(Debug)]
pub enum Ch341aError {
    /// Device not found
    DeviceNotFound,
    /// Failed to open device
    OpenFailed(String),
    /// Failed to claim interface
    ClaimFailed(String),
    /// USB transfer failed
    TransferFailed(String),
    /// Invalid response from device
    InvalidResponse,
    /// Timeout during operation
    Timeout,
    /// Configuration error
    ConfigError(String),
    /// Core library error
    Core(rflasher_core::error::Error),
}

impl fmt::Display for Ch341aError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ch341aError::DeviceNotFound => {
                write!(f, "CH341A device not found (VID:1a86 PID:5512)")
            }
            Ch341aError::OpenFailed(msg) => write!(f, "Failed to open CH341A: {}", msg),
            Ch341aError::ClaimFailed(msg) => write!(f, "Failed to claim interface: {}", msg),
            Ch341aError::TransferFailed(msg) => write!(f, "USB transfer failed: {}", msg),
            Ch341aError::InvalidResponse => write!(f, "Invalid response from CH341A"),
            Ch341aError::Timeout => write!(f, "Timeout during USB transfer"),
            Ch341aError::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
            Ch341aError::Core(e) => write!(f, "Core error: {}", e),
        }
    }
}

impl std::error::Error for Ch341aError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Ch341aError::Core(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rflasher_core::error::Error> for Ch341aError {
    fn from(e: rflasher_core::error::Error) -> Self {
        Ch341aError::Core(e)
    }
}

impl From<nusb::Error> for Ch341aError {
    fn from(e: nusb::Error) -> Self {
        Ch341aError::TransferFailed(e.to_string())
    }
}

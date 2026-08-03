//! Error types for FT4222 programmer

use std::fmt;

/// Result type for FT4222 operations
pub type Result<T> = std::result::Result<T, Ft4222Error>;

/// Errors that can occur when using the FT4222 programmer
#[derive(Debug)]
pub enum Ft4222Error {
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
    /// Invalid parameter
    InvalidParameter(String),
    /// Core library error
    Core(rflasher_core::error::Error),
}

impl fmt::Display for Ft4222Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ft4222Error::DeviceNotFound => {
                write!(f, "FT4222H device not found (VID:0403 PID:601c)")
            }
            Ft4222Error::OpenFailed(msg) => write!(f, "Failed to open FT4222H: {}", msg),
            Ft4222Error::ClaimFailed(msg) => write!(f, "Failed to claim interface: {}", msg),
            Ft4222Error::TransferFailed(msg) => write!(f, "USB transfer failed: {}", msg),
            Ft4222Error::InvalidResponse(msg) => {
                write!(f, "Invalid response from FT4222H: {}", msg)
            }
            Ft4222Error::Timeout => write!(f, "Timeout during USB transfer"),
            Ft4222Error::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
            Ft4222Error::InvalidParameter(msg) => write!(f, "Invalid parameter: {}", msg),
            Ft4222Error::Core(e) => write!(f, "Core error: {}", e),
        }
    }
}

impl std::error::Error for Ft4222Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Ft4222Error::Core(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rflasher_core::error::Error> for Ft4222Error {
    fn from(e: rflasher_core::error::Error) -> Self {
        Ft4222Error::Core(e)
    }
}

impl From<nusb::Error> for Ft4222Error {
    fn from(e: nusb::Error) -> Self {
        Ft4222Error::TransferFailed(e.to_string())
    }
}

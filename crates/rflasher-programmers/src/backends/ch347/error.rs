//! Error types for CH347 programmer

use thiserror::Error;

/// Result type for CH347 operations
pub type Result<T> = std::result::Result<T, Ch347Error>;

/// Errors that can occur when using the CH347 programmer
#[derive(Debug, Error)]
pub enum Ch347Error {
    /// Device not found
    #[error("CH347 device not found (VID:1a86 PID:55db or 55de)")]
    DeviceNotFound,
    /// Failed to open device
    #[error("Failed to open CH347: {0}")]
    OpenFailed(String),
    /// Failed to claim interface
    #[error("Failed to claim interface: {0}")]
    ClaimFailed(String),
    /// USB transfer failed
    #[error("USB transfer failed: {0}")]
    TransferFailed(String),
    /// Invalid response from device
    #[error("Invalid response from CH347: {0}")]
    InvalidResponse(String),
    /// Configuration error
    #[error("Configuration error: {0}")]
    ConfigError(String),
}

impl From<nusb::Error> for Ch347Error {
    fn from(e: nusb::Error) -> Self {
        Ch347Error::TransferFailed(e.to_string())
    }
}

//! Error types for CH341A programmer

use thiserror::Error;

/// Result type for CH341A operations
pub type Result<T> = std::result::Result<T, Ch341aError>;

/// Errors that can occur when using the CH341A programmer
#[derive(Debug, Error)]
pub enum Ch341aError {
    /// Device not found
    #[error("CH341A device not found (VID:1a86 PID:5512)")]
    DeviceNotFound,
    /// Failed to open device
    #[error("Failed to open CH341A: {0}")]
    OpenFailed(String),
    /// Failed to claim interface
    #[error("Failed to claim interface: {0}")]
    ClaimFailed(String),
    /// USB transfer failed
    #[error("USB transfer failed: {0}")]
    TransferFailed(String),
}

impl From<nusb::Error> for Ch341aError {
    fn from(e: nusb::Error) -> Self {
        Ch341aError::TransferFailed(e.to_string())
    }
}

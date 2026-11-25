//! Error types for FT4222 programmer

use thiserror::Error;

/// Result type for FT4222 operations
pub type Result<T> = std::result::Result<T, Ft4222Error>;

/// Errors that can occur when using the FT4222 programmer
#[derive(Debug, Error)]
pub enum Ft4222Error {
    /// Device not found
    #[error("FT4222H device not found (VID:0403 PID:601c)")]
    DeviceNotFound,
    /// Failed to open device
    #[error("Failed to open FT4222H: {0}")]
    OpenFailed(String),
    /// Failed to claim interface
    #[error("Failed to claim interface: {0}")]
    ClaimFailed(String),
    /// USB transfer failed
    #[error("USB transfer failed: {0}")]
    TransferFailed(String),
    /// Invalid response from device
    #[error("Invalid response from FT4222H: {0}")]
    InvalidResponse(String),
    /// Timeout during operation
    #[error("Timeout during USB transfer")]
    Timeout,
    /// Invalid parameter
    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),
}

impl From<nusb::Error> for Ft4222Error {
    fn from(e: nusb::Error) -> Self {
        Ft4222Error::TransferFailed(e.to_string())
    }
}

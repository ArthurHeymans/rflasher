//! Error types for FTDI programmer (ftdi-nusb backend, shared by native and wasm)

use thiserror::Error;

/// Result type for FTDI operations
pub type Result<T> = std::result::Result<T, FtdiError>;

/// Errors that can occur during FTDI operations
#[derive(Debug, Error)]
pub enum FtdiError {
    /// Failed to open device
    #[error("Failed to open device: {0}")]
    OpenFailed(String),

    /// USB transfer failed
    #[error("USB transfer failed: {0}")]
    TransferFailed(String),

    /// Failed to configure device
    #[error("Failed to configure device: {0}")]
    ConfigFailed(String),

    /// Invalid device type
    #[error("Invalid device type: {0}")]
    InvalidDeviceType(String),

    /// Invalid channel/port specification
    #[error("Invalid channel: {0}")]
    InvalidChannel(String),

    /// Invalid parameter
    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    /// ftdi-nusb error
    #[error("ftdi-nusb error: {0}")]
    FtdiNusb(String),

    /// USB enumeration error
    #[error("USB error: {0}")]
    UsbError(String),
}

impl From<ftdi_nusb::Error> for FtdiError {
    fn from(e: ftdi_nusb::Error) -> Self {
        FtdiError::FtdiNusb(e.to_string())
    }
}

impl From<String> for FtdiError {
    fn from(s: String) -> Self {
        FtdiError::InvalidParameter(s)
    }
}

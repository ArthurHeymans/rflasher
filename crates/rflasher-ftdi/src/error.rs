//! Error types for FTDI programmer

use std::fmt;

/// Result type for FTDI operations
pub type Result<T> = std::result::Result<T, FtdiError>;

/// Errors that can occur during FTDI operations
#[derive(Debug)]
pub enum FtdiError {
    /// No FTDI device found
    DeviceNotFound,

    /// Failed to open device
    OpenFailed(String),

    /// Failed to claim USB interface
    ClaimFailed(String),

    /// USB transfer failed
    TransferFailed(String),

    /// Failed to configure device
    ConfigFailed(String),

    /// Invalid device type
    InvalidDeviceType(String),

    /// Invalid channel/port specification
    InvalidChannel(String),

    /// Invalid parameter
    InvalidParameter(String),

    /// libftdi error
    LibFtdi(String),

    /// USB enumeration error
    UsbError(String),
}

impl fmt::Display for FtdiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FtdiError::DeviceNotFound => write!(f, "No FTDI device found"),
            FtdiError::OpenFailed(s) => write!(f, "Failed to open device: {}", s),
            FtdiError::ClaimFailed(s) => write!(f, "Failed to claim interface: {}", s),
            FtdiError::TransferFailed(s) => write!(f, "USB transfer failed: {}", s),
            FtdiError::ConfigFailed(s) => write!(f, "Failed to configure device: {}", s),
            FtdiError::InvalidDeviceType(s) => write!(f, "Invalid device type: {}", s),
            FtdiError::InvalidChannel(s) => write!(f, "Invalid channel: {}", s),
            FtdiError::InvalidParameter(s) => write!(f, "Invalid parameter: {}", s),
            FtdiError::LibFtdi(s) => write!(f, "libftdi error: {}", s),
            FtdiError::UsbError(s) => write!(f, "USB error: {}", s),
        }
    }
}

impl std::error::Error for FtdiError {}

impl From<nusb::Error> for FtdiError {
    fn from(e: nusb::Error) -> Self {
        FtdiError::UsbError(e.to_string())
    }
}

impl From<ftdi::Error> for FtdiError {
    fn from(e: ftdi::Error) -> Self {
        FtdiError::LibFtdi(e.to_string())
    }
}

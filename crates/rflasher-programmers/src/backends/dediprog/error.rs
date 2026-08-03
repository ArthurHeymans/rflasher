//! Error types for Dediprog programmer

use std::fmt;

/// Result type for Dediprog operations
pub type Result<T> = std::result::Result<T, DediprogError>;

/// Errors that can occur when using the Dediprog programmer
#[derive(Debug)]
pub enum DediprogError {
    /// Device not found
    DeviceNotFound,
    /// Unknown or unsupported device type
    UnknownDevice(String),
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
    /// Firmware version error
    FirmwareError(String),
    /// Unsupported operation for this device/firmware
    Unsupported(String),
    /// Parameter parsing error
    InvalidParameter(String),
    /// Core library error
    Core(rflasher_core::error::Error),
}

impl fmt::Display for DediprogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DediprogError::DeviceNotFound => {
                write!(f, "Dediprog device not found (VID:0483 PID:DADA)")
            }
            DediprogError::UnknownDevice(msg) => write!(f, "Unknown Dediprog device: {}", msg),
            DediprogError::OpenFailed(msg) => write!(f, "Failed to open Dediprog: {}", msg),
            DediprogError::ClaimFailed(msg) => write!(f, "Failed to claim interface: {}", msg),
            DediprogError::TransferFailed(msg) => write!(f, "USB transfer failed: {}", msg),
            DediprogError::InvalidResponse(msg) => {
                write!(f, "Invalid response from Dediprog: {}", msg)
            }
            DediprogError::Timeout => write!(f, "Timeout during USB transfer"),
            DediprogError::ConfigError(msg) => write!(f, "Configuration error: {}", msg),
            DediprogError::FirmwareError(msg) => write!(f, "Firmware error: {}", msg),
            DediprogError::Unsupported(msg) => write!(f, "Unsupported operation: {}", msg),
            DediprogError::InvalidParameter(msg) => write!(f, "Invalid parameter: {}", msg),
            DediprogError::Core(e) => write!(f, "Core error: {}", e),
        }
    }
}

impl std::error::Error for DediprogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DediprogError::Core(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rflasher_core::error::Error> for DediprogError {
    fn from(e: rflasher_core::error::Error) -> Self {
        DediprogError::Core(e)
    }
}

impl From<nusb::Error> for DediprogError {
    fn from(e: nusb::Error) -> Self {
        DediprogError::TransferFailed(e.to_string())
    }
}

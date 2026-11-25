//! Error types for Dediprog programmer

use thiserror::Error;

/// Result type for Dediprog operations
pub type Result<T> = std::result::Result<T, DediprogError>;

/// Errors that can occur when using the Dediprog programmer
#[derive(Debug, Error)]
pub enum DediprogError {
    /// Device not found
    #[error("Dediprog device not found (VID:0483 PID:DADA)")]
    DeviceNotFound,
    /// Unknown or unsupported device type
    #[error("Unknown Dediprog device: {0}")]
    UnknownDevice(String),
    /// Failed to open device
    #[error("Failed to open Dediprog: {0}")]
    OpenFailed(String),
    /// Failed to claim interface
    #[error("Failed to claim interface: {0}")]
    ClaimFailed(String),
    /// USB transfer failed
    #[error("USB transfer failed: {0}")]
    TransferFailed(String),
    /// Typed USB transfer failure, preserving STALL for safe status-read retries.
    #[error("USB transfer failed: {0}")]
    UsbTransfer(#[source] nusb::transfer::TransferError),
    /// Invalid response from device
    #[error("Invalid response from Dediprog: {0}")]
    InvalidResponse(String),
    /// Timeout during operation
    #[error("Timeout during USB transfer")]
    Timeout,
    /// Firmware version error
    #[error("Firmware error: {0}")]
    FirmwareError(String),
    /// Unsupported operation for this device/firmware
    #[error("Unsupported operation: {0}")]
    Unsupported(String),
    /// Parameter parsing error
    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),
}

impl From<nusb::Error> for DediprogError {
    fn from(e: nusb::Error) -> Self {
        DediprogError::TransferFailed(e.to_string())
    }
}

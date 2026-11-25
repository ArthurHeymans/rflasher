//! Error types for Raiden Debug SPI programmer

use thiserror::Error;

/// Result type for Raiden operations
pub type Result<T> = std::result::Result<T, RaidenError>;

/// Errors that can occur when using the Raiden Debug SPI programmer
#[derive(Debug, Error)]
pub enum RaidenError {
    /// Device not found
    #[error("Raiden Debug SPI device not found (VID:18D1, subclass:51)")]
    DeviceNotFound,
    /// Failed to open device
    #[error("Failed to open Raiden device: {0}")]
    OpenFailed(String),
    /// Failed to claim interface
    #[error("Failed to claim interface: {0}")]
    ClaimFailed(String),
    /// USB transfer failed
    #[error("USB transfer failed: {0}")]
    TransferFailed(String),
    /// Invalid response from device
    #[error("Invalid response from Raiden device: {0}")]
    InvalidResponse(String),
    /// Timeout during operation
    #[error("Timeout during USB transfer")]
    Timeout,
    /// Protocol error (USB SPI status code)
    #[error("Protocol error 0x{0:04X}: {desc}", desc = status_code_description(*.0))]
    ProtocolError(u16),
    /// Failed to enable SPI bridge
    #[error("Failed to enable SPI bridge: {0}")]
    EnableFailed(String),
    /// Invalid parameter
    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),
}

fn status_code_description(code: u16) -> &'static str {
    match code {
        0x0001 => "SPI timeout",
        0x0002 => "SPI busy",
        0x0003 => "Invalid write count",
        0x0004 => "Invalid read count",
        0x0005 => "SPI disabled",
        0x0006 => "Bad data index in response",
        0x0007 => "Data overflow",
        0x0008 => "Unexpected packet",
        0x0009 => "Full duplex not supported",
        0x8000 => "Unknown error",
        _ => "Unknown status code",
    }
}

impl From<nusb::Error> for RaidenError {
    fn from(e: nusb::Error) -> Self {
        RaidenError::TransferFailed(e.to_string())
    }
}

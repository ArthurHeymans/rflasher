//! Error types for Linux SPI operations

use thiserror::Error;

/// Linux SPI specific errors
#[derive(Debug, Error)]
pub enum LinuxSpiError {
    /// Failed to open device
    #[error("Failed to open {path}: {source}")]
    OpenFailed {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Failed to set SPI mode
    #[error("Failed to set SPI mode to {mode}: {source}")]
    SetModeFailed {
        mode: u8,
        #[source]
        source: std::io::Error,
    },

    /// Failed to set bits per word
    #[error("Failed to set bits per word to {bits}: {source}")]
    SetBitsPerWordFailed {
        bits: u8,
        #[source]
        source: std::io::Error,
    },

    /// Failed to set clock speed
    #[error("Failed to set clock speed to {speed} Hz: {source}")]
    SetSpeedFailed {
        speed: u32,
        #[source]
        source: std::io::Error,
    },

    /// SPI transfer failed
    #[error("SPI transfer failed: {0}")]
    TransferFailed(#[source] std::io::Error),

    /// Invalid parameter
    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    /// Device not specified
    #[error("No device specified. Use dev=/dev/spidevX.Y")]
    NoDevice,

    /// Failed to read kernel buffer size
    #[error("Failed to read kernel buffer size: {0}")]
    BufferSizeReadFailed(String),
}

/// Result type for Linux SPI operations
pub type Result<T> = std::result::Result<T, LinuxSpiError>;

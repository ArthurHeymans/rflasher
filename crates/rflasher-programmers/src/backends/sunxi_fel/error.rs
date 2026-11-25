//! Error types for the sunxi FEL programmer

use thiserror::Error;

/// Errors that can occur during FEL operations
#[derive(Debug, Error)]
pub enum Error {
    /// USB communication error
    #[error("USB error: {0}")]
    Usb(String),
    /// FEL protocol error
    #[error("FEL protocol error: {0}")]
    Protocol(String),
    /// Unsupported SoC
    #[error("Unsupported SoC ID: 0x{0:08x}")]
    UnsupportedSoc(u32),
    /// SPI transfer failed
    #[error("SPI transfer failed")]
    SpiTransferFailed,
    /// No FEL device found
    #[error("No Allwinner FEL device found")]
    DeviceNotFound,
}

/// Result type for FEL operations
pub type Result<T> = std::result::Result<T, Error>;

//! Error types for the sunxi FEL programmer

use std::fmt;

/// Errors that can occur during FEL operations
#[derive(Debug)]
pub enum Error {
    /// USB communication error
    Usb(String),
    /// FEL protocol error
    Protocol(String),
    /// Unsupported SoC
    UnsupportedSoc(u32),
    /// SPI initialization failed
    SpiInitFailed,
    /// SPI transfer failed
    SpiTransferFailed,
    /// No FEL device found
    DeviceNotFound,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Usb(msg) => write!(f, "USB error: {}", msg),
            Error::Protocol(msg) => write!(f, "FEL protocol error: {}", msg),
            Error::UnsupportedSoc(id) => {
                write!(f, "Unsupported SoC ID: 0x{:08x}", id)
            }
            Error::SpiInitFailed => write!(f, "SPI initialization failed"),
            Error::SpiTransferFailed => write!(f, "SPI transfer failed"),
            Error::DeviceNotFound => write!(f, "No Allwinner FEL device found"),
        }
    }
}

impl std::error::Error for Error {}

/// Result type for FEL operations
pub type Result<T> = std::result::Result<T, Error>;

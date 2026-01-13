//! Error types for postcard-spi programmer

use postcard_spi_icd::SpiWireError;
use thiserror::Error;

/// Errors that can occur when using the postcard-spi programmer
#[derive(Error, Debug)]
pub enum Error {
    /// Failed to find USB device
    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    /// Failed to connect to device
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    /// USB communication error
    #[error("USB error: {0}")]
    UsbError(String),

    /// RPC communication error
    #[error("RPC error: {0}")]
    RpcError(String),

    /// Error returned by the device
    #[error("Device error: {0:?}")]
    WireError(SpiWireError),

    /// Serialization/deserialization error
    #[error("Serialization error: {0}")]
    SerdeError(String),

    /// Transfer too large for device
    #[error("Transfer too large: requested {requested} bytes, max is {max}")]
    TransferTooLarge { requested: usize, max: usize },

    /// Invalid chip select
    #[error("Invalid chip select {cs}, device has {num_cs} CS lines")]
    InvalidCs { cs: u8, num_cs: u8 },

    /// I/O mode not supported
    #[error("I/O mode not supported by device")]
    IoModeNotSupported,

    /// Timeout waiting for response
    #[error("Timeout waiting for device response")]
    Timeout,
}

/// Result type for postcard-spi operations
pub type Result<T> = std::result::Result<T, Error>;

impl From<SpiWireError> for Error {
    fn from(e: SpiWireError) -> Self {
        Error::WireError(e)
    }
}

//! Error types for Linux GPIO SPI operations

use thiserror::Error;

/// Linux GPIO SPI specific errors
#[derive(Debug, Error)]
pub enum LinuxGpioError {
    /// Failed to open GPIO chip
    #[error("Failed to open GPIO chip '{path}': {source}")]
    ChipOpenFailed {
        path: String,
        #[source]
        source: gpiocdev::Error,
    },

    /// Failed to request GPIO lines
    #[error("Failed to request GPIO lines: {0}")]
    LineRequestFailed(#[source] gpiocdev::Error),

    /// Failed to set GPIO line value
    #[error("Failed to set GPIO line value: {0}")]
    SetValueFailed(#[source] gpiocdev::Error),

    /// Failed to get GPIO line value
    #[error("Failed to get GPIO line value: {0}")]
    GetValueFailed(#[source] gpiocdev::Error),

    /// Failed to reconfigure GPIO lines
    #[error("Failed to reconfigure GPIO lines: {0}")]
    ReconfigureFailed(#[source] gpiocdev::Error),

    /// Invalid parameter
    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    /// Missing required parameter
    #[error("Missing required parameter: {0}")]
    MissingParameter(&'static str),

    /// GPIO chip or device not specified
    #[error("No GPIO chip specified. Use dev=/dev/gpiochipN or gpiochip=N")]
    NoDevice,

    /// Invalid GPIO line number
    #[error("Invalid GPIO line number for {name}: {value}")]
    InvalidLineNumber { name: &'static str, value: String },

    /// io2 and io3 must be specified together for quad I/O
    #[error("Both io2 and io3 must be specified for quad I/O mode")]
    IncompleteQuadIo,
}

/// Result type for Linux GPIO SPI operations
pub type Result<T> = std::result::Result<T, LinuxGpioError>;

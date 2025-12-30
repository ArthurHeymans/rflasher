//! Error types for the REPL

use thiserror::Error;

/// Errors that can occur in the REPL
#[derive(Error, Debug)]
pub enum ReplError {
    /// I/O error (reading/writing stdin/stdout)
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    /// Steel evaluation error
    #[error("Scheme error: {0}")]
    SteelError(String),

    /// SPI operation error
    #[error("SPI error: {0}")]
    SpiError(String),

    /// Invalid argument
    #[error("Invalid argument: {0}")]
    InvalidArgument(String),
}

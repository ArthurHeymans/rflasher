//! Typed CLI error for `rflasher`.

use thiserror::Error;

/// Result alias for CLI commands and helpers.
pub type Result<T> = std::result::Result<T, CliError>;

/// Top-level error type for the `rflasher` CLI.
#[derive(Debug, Error)]
pub enum CliError {
    /// Programmer discovery, opening, or handle error.
    #[error("{0}")]
    Programmer(#[from] rflasher_programmers::Error),

    /// Core flash operation error.
    #[error("{0}")]
    Core(#[from] rflasher_core::Error),

    /// Layout parsing or validation error.
    #[error("{0}")]
    Layout(#[from] rflasher_core::layout::LayoutError),

    /// Write-protection error.
    #[error("{0}")]
    Wp(#[from] rflasher_core::wp::WpError),

    /// Chip database load error.
    #[error("Failed to load chip database: {0}")]
    ChipDb(#[from] rflasher_chips::ChipDbError),

    /// REPL error.
    #[cfg(feature = "repl")]
    #[error("{0}")]
    Repl(#[from] rflasher_repl::ReplError),

    /// Host filesystem or terminal I/O error.
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// Progress-bar template error.
    #[error("{0}")]
    Template(#[from] indicatif::style::TemplateError),

    /// User-facing CLI error message.
    #[error("{0}")]
    Message(String),
}

impl From<String> for CliError {
    fn from(msg: String) -> Self {
        Self::Message(msg)
    }
}

impl From<&str> for CliError {
    fn from(msg: &str) -> Self {
        Self::Message(msg.to_owned())
    }
}

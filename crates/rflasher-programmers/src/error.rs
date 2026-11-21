//! Typed error for programmer catalog, device selection, registry, and `FlashHandle`.

use std::fmt;

/// Result alias using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Boxed `Send + Sync` error source preserved across backend boundaries.
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Error returned by programmer discovery, opening, and handle operations.
#[derive(Debug)]
pub enum Error {
    /// Core flash protocol, probing, or device error.
    Core(rflasher_core::Error),
    /// Layout parsing or validation error.
    Layout(rflasher_core::layout::LayoutError),
    /// Requested programmer name is unknown or not compiled in.
    UnknownProgrammer(String),
    /// Invalid programmer parameter syntax or option value.
    InvalidOptions(String),
    /// Backend open, transport, or configuration error, optionally preserving
    /// the underlying backend error in [`std::error::Error::source`].
    Backend {
        message: String,
        source: Option<BoxError>,
    },
    /// Requested operation is unsupported by the selected programmer.
    Unsupported(String),
}

impl Error {
    /// Construct a backend error with no underlying source.
    pub fn backend(message: impl Into<String>) -> Self {
        Self::Backend {
            message: message.into(),
            source: None,
        }
    }

    /// Construct a backend error preserving `source` in [`std::error::Error::source`].
    pub fn backend_source<E>(source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Backend {
            message: source.to_string(),
            source: Some(Box::new(source)),
        }
    }

    /// Construct a backend error with additional context while preserving `source`.
    pub fn backend_with_source<E>(message: impl Into<String>, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Backend {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(e) => write!(f, "{e}"),
            Self::Layout(e) => write!(f, "{e}"),
            Self::UnknownProgrammer(name) => write!(f, "Unknown programmer: {name}"),
            Self::InvalidOptions(msg)
            | Self::Backend { message: msg, .. }
            | Self::Unsupported(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Core(e) => Some(e),
            Self::Layout(e) => Some(e),
            Self::Backend {
                source: Some(source),
                ..
            } => Some(source.as_ref()),
            Self::UnknownProgrammer(_)
            | Self::InvalidOptions(_)
            | Self::Backend { source: None, .. }
            | Self::Unsupported(_) => None,
        }
    }
}

impl From<rflasher_core::Error> for Error {
    fn from(e: rflasher_core::Error) -> Self {
        Self::Core(e)
    }
}

impl From<rflasher_core::layout::LayoutError> for Error {
    fn from(e: rflasher_core::layout::LayoutError) -> Self {
        Self::Layout(e)
    }
}

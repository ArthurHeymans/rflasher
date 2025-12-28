//! Error types for rflasher-postcard

use crate::protocol::ErrorResp;

/// Result type for postcard programmer operations
pub type Result<T> = core::result::Result<T, Error>;

/// Error type for postcard programmer operations
#[derive(Debug)]
pub enum Error {
    /// USB device not found
    DeviceNotFound,

    /// USB communication error
    Usb(String),

    /// Protocol error (serialization/deserialization)
    Protocol(String),

    /// Timeout waiting for response
    Timeout,

    /// Device returned an error
    Device(ErrorResp),

    /// Invalid response from device
    InvalidResponse(String),

    /// I/O mode not supported
    IoModeNotSupported,

    /// Buffer too small
    BufferTooSmall,

    /// Not connected
    NotConnected,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DeviceNotFound => write!(f, "USB device not found"),
            Self::Usb(msg) => write!(f, "USB error: {}", msg),
            Self::Protocol(msg) => write!(f, "Protocol error: {}", msg),
            Self::Timeout => write!(f, "Timeout waiting for response"),
            Self::Device(resp) => {
                write!(f, "Device error: {:?}", resp.code)?;
                if let Some(msg) = &resp.message {
                    write!(f, " - {}", msg)?;
                }
                Ok(())
            }
            Self::InvalidResponse(msg) => write!(f, "Invalid response: {}", msg),
            Self::IoModeNotSupported => write!(f, "I/O mode not supported"),
            Self::BufferTooSmall => write!(f, "Buffer too small"),
            Self::NotConnected => write!(f, "Not connected to device"),
        }
    }
}

impl std::error::Error for Error {}

impl From<nusb::Error> for Error {
    fn from(e: nusb::Error) -> Self {
        Self::Usb(e.to_string())
    }
}

impl From<nusb::transfer::TransferError> for Error {
    fn from(e: nusb::transfer::TransferError) -> Self {
        Self::Usb(e.to_string())
    }
}

impl From<postcard::Error> for Error {
    fn from(e: postcard::Error) -> Self {
        Self::Protocol(format!("{:?}", e))
    }
}

// Provide a String type for std builds
type String = std::string::String;

//! Error types for the EZP2013 (`ezp_spi`) programmer

use thiserror::Error;

/// Result type for EZP2013 operations
pub type Result<T> = std::result::Result<T, EzpSpiError>;

/// Errors that can occur when using the EZP2013 USB programmer
#[derive(Debug, Error)]
pub enum EzpSpiError {
    /// Device not found
    #[error("EZP2013 device not found (VID:0314 PID:1011)")]
    DeviceNotFound,
    /// Failed to open device
    #[error("Failed to open EZP2013: {0}")]
    OpenFailed(String),
    /// Failed to claim interface or endpoints
    #[error("Failed to claim EZP2013 interface: {0}")]
    ClaimFailed(String),
    /// USB control transfer stalled cleanly on the wire
    #[error("USB control stall: {0}")]
    Stall(String),
    /// USB transfer failed
    #[error("USB transfer failed: {0}")]
    TransferFailed(String),
    /// USB or programmer operation timed out
    #[error("EZP2013 operation timed out: {0}")]
    Timeout(String),
    /// Session was invalidated by a previous failed, timed-out, or cancelled transfer
    #[error(
        "EZP2013 USB session is out of sync after an interrupted or failed transfer;          reopen or replug the programmer"
    )]
    SessionPoisoned,
    /// Programmer returned an unexpected response
    #[error("Invalid response from EZP2013: {0}")]
    InvalidResponse(String),
    /// Device is running its USB bootloader instead of application firmware
    #[error(
        "EZP2013 is in bootloader mode (\"{0}\"); replug the programmer to return to normal mode"
    )]
    BootloaderMode(String),
    /// Firmware version is outside the supported 1.2.0.x series
    #[error(
        "Unsupported EZP2013 firmware version \"{version}\" on \"{device_type}\" (only 1.2.0.x is supported)"
    )]
    UnsupportedFirmware {
        /// Device type string reported by request `0x54`
        device_type: String,
        /// Firmware version string reported by request `0x56`
        version: String,
    },
    /// Invalid length for a bulk write command
    #[error("Invalid EZP2013 write length: {0}")]
    InvalidLength(usize),
}

impl From<nusb::Error> for EzpSpiError {
    fn from(e: nusb::Error) -> Self {
        EzpSpiError::TransferFailed(e.to_string())
    }
}

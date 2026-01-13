//! PostcardSpi programmer for rflasher
//!
//! This crate provides a SPI flash programmer that communicates with
//! a microcontroller (e.g., Raspberry Pi Pico) running the postcard-spi
//! firmware over USB using the postcard-rpc protocol.
//!
//! ## Features
//!
//! - Multi-I/O SPI modes: 1-1-1, 1-1-2, 1-2-2, 1-1-4, 1-4-4, 4-4-4 (QPI)
//! - Multiple chip select lines
//! - Configurable SPI clock speed
//! - USB bulk transfer for high throughput
//!
//! ## Example
//!
//! ```ignore
//! use rflasher_postcard_spi::PostcardSpi;
//!
//! // Open by serial number
//! let mut programmer = PostcardSpi::open_by_serial("12345678")?;
//!
//! // Or open the first available device
//! let mut programmer = PostcardSpi::open()?;
//!
//! // Configure
//! programmer.set_speed(10_000_000)?; // 10 MHz
//! programmer.set_cs(0)?;             // Use CS0
//!
//! // Use with rflasher-core SpiMaster trait
//! use rflasher_core::programmer::SpiMaster;
//! let features = programmer.features();
//! ```

mod device;
mod error;

pub use device::{open_with_options, parse_options, PostcardSpi, PostcardSpiOptions};
pub use error::{Error, Result};

// Re-export ICD types that users might need
pub use postcard_spi_icd::{
    AddressWidth, DeviceInfo, IoMode, IoModeFlags, SpiWireError, PROTOCOL_VERSION, USB_PID, USB_VID,
};

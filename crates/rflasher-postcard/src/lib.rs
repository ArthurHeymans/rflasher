//! Postcard-RPC based flash programmer for rflasher
//!
//! This crate provides a postcard-RPC based communication protocol for flash
//! programming between a host PC and a microcontroller. It is similar to serprog
//! but uses postcard-RPC for structured, type-safe communication.
//!
//! # Architecture
//!
//! The crate is split into several modules:
//!
//! - [`protocol`]: Shared protocol definitions (endpoints, message types)
//! - [`error`]: Error types
//! - [`device`]: Host-side USB transport and SpiMaster implementation (std only)
//!
//! # Usage
//!
//! ## Host Side (std)
//!
//! ```ignore
//! use rflasher_postcard::device::PostcardProgrammer;
//! use rflasher_core::programmer::SpiMaster;
//!
//! // Open USB connection to the programmer
//! let mut programmer = PostcardProgrammer::open_usb(0x16c0, 0x27dd)?;
//!
//! // Query device info
//! let info = programmer.device_info()?;
//! println!("Connected to: {}", info.name);
//!
//! // Set SPI frequency
//! let actual_freq = programmer.set_spi_freq(10_000_000)?;
//!
//! // Execute SPI commands via SpiMaster trait
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(0x9F, &mut id);
//! programmer.execute(&mut cmd)?;
//! ```
//!
//! ## Firmware Side (no_std with Embassy)
//!
//! See the `examples/rp2040-firmware` directory for a complete example.
//!
//! # Features
//!
//! - `std` (default): Enables host-side USB support via nusb
//! - `defmt`: Enables defmt formatting for embedded debugging
//! - `rflasher-core`: Enable integration with rflasher-core types

#![cfg_attr(not(feature = "std"), no_std)]

pub mod protocol;

#[cfg(feature = "std")]
pub mod device;

#[cfg(feature = "std")]
pub mod error;

// Re-export commonly used items
pub use protocol::*;

#[cfg(feature = "std")]
pub use device::PostcardProgrammer;

#[cfg(feature = "std")]
pub use error::{Error, Result};

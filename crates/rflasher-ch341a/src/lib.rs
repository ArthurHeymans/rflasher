//! rflasher-ch341a - CH341A USB programmer support
//!
//! This crate provides support for the CH341A USB-to-SPI programmer.
//! The CH341A is a cheap and widely available USB programmer commonly used
//! for programming SPI flash chips.
//!
//! # Protocol Overview
//!
//! The CH341A communicates via USB bulk transfers. SPI data is sent using
//! the `SPI_STREAM` command, and chip select is controlled via `UIO_STREAM`
//! commands. Data bytes must be bit-reversed due to the CH341A's bit ordering.
//!
//! # Example
//!
//! ```no_run
//! use rflasher_ch341a::Ch341a;
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! let mut ch341a = Ch341a::open()?;
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id); // JEDEC ID
//! ch341a.execute(&mut cmd)?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![cfg_attr(not(any(feature = "std", feature = "wasm")), no_std)]

#[cfg(feature = "std")]
mod device;
#[cfg(any(feature = "std", feature = "wasm"))]
mod error;
#[cfg(any(feature = "std", feature = "wasm"))]
mod protocol;
#[cfg(feature = "wasm")]
mod web_device;

#[cfg(feature = "std")]
pub use device::Ch341a;
#[cfg(any(feature = "std", feature = "wasm"))]
pub use error::{Ch341aError, Result};
#[cfg(feature = "wasm")]
pub use web_device::Ch341aAsync;

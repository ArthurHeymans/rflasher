//! rflasher-core - Core library for flash chip programming
//!
//! This crate provides the core functionality for reading, writing, and
//! erasing SPI flash chips. It is designed to be `no_std` compatible for
//! use in embedded environments.
//!
//! # Features
//!
//! - `std` - Enable standard library support (includes `alloc`)
//! - `alloc` - Enable heap allocation for features like chip name search
//!
//! # Example
//!
//! ```ignore
//! use rflasher_core::{flash, programmer::SpiMaster};
//!
//! fn probe_chip<M: SpiMaster>(master: &mut M) {
//!     match flash::probe(master) {
//!         Ok(ctx) => {
//!             println!("Found: {} {}", ctx.chip.vendor, ctx.chip.name);
//!             println!("Size: {} bytes", ctx.chip.total_size);
//!         }
//!         Err(e) => println!("Probe failed: {}", e),
//!     }
//! }
//! ```

#![no_std]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

pub mod chip;
pub mod error;
pub mod flash;
pub mod programmer;
pub mod protocol;
pub mod spi;
pub mod wp;

pub use error::{Error, Result};

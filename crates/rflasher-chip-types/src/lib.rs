//! Shared SPI NOR flash chip data model.
//!
//! This crate contains chip descriptors and lookup traits shared by database
//! providers and flash-programming implementations.

#![no_std]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

#[cfg(feature = "alloc")]
extern crate alloc;

mod features;
mod provider;
mod types;

pub use features::{Features, QeMethod};
pub use provider::ChipProvider;
pub use types::*;

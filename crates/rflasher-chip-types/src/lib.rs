//! Shared SPI NOR flash chip data model.
//!
//! This crate contains chip descriptors and lookup traits shared by database
//! providers and flash-programming implementations.

#![no_std]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

#[cfg(feature = "alloc")]
extern crate alloc;

mod catalog;
mod features;
mod name;
mod provider;
mod regions;
mod storage;
mod types;
mod unlock;

pub use catalog::{ChipCatalog, ChipIdentity, ResolveError};
pub use features::Features;
pub use name::name_matches;
pub use provider::ChipProvider;
pub use regions::EraseRegions;
pub use storage::{ChipName, ChipSlice};
pub use types::*;
pub use unlock::Unlock;

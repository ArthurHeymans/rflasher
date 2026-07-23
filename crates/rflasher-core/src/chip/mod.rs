//! Flash chip types and database
//!
//! This module provides types for describing flash chips and their
//! capabilities, as well as a database of known chips.

mod features;
mod provider;
mod types;

#[cfg(feature = "std")]
mod database;

pub use features::Features;
pub use provider::ChipProvider;
pub use types::*;

#[cfg(feature = "std")]
pub use database::*;

//! Programmer traits and abstractions
//!
//! This module defines the core traits that all programmers must implement
//! to interact with flash chips.

pub mod bitbang;
mod traits;

pub use bitbang::{BitbangDualIo, BitbangQuadIo, BitbangSpiMaster};
pub use traits::*;

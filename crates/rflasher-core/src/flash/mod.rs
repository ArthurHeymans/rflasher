//! High-level flash operations
//!
//! This module provides high-level operations for reading, writing,
//! and erasing flash chips.

mod context;
mod operations;

pub use context::FlashContext;
pub use operations::*;

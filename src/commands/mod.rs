//! CLI command implementations
//!
//! This module provides command implementations that work with the unified
//! `FlashDevice` abstraction.
//!
//! ## Unified commands (FlashDevice-based)
//!
//! The `unified` module contains command implementations that work with any
//! `FlashDevice` implementation - both SPI-based (CH341A, FTDI, etc.) and
//! opaque programmers (Intel internal).
//!
//! ## Probe commands
//!
//! Probe commands are special because they need to identify the chip type:
//! - SPI: Uses JEDEC ID probing
//! - Opaque: Uses Intel Flash Descriptor

pub mod layout;
mod list;
pub mod unified;
pub mod wp;

pub use list::{list_chips, list_programmers};

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

#[cfg(feature = "repl")]
pub mod repl;

pub use list::{list_chips, list_programmers};

/// Format a byte size as a human-readable string (e.g., "256 KiB", "4 MiB")
pub fn format_size(bytes: u32) -> String {
    if bytes >= 1024 * 1024 && bytes % (1024 * 1024) == 0 {
        format!("{} MiB", bytes / (1024 * 1024))
    } else if bytes >= 1024 && bytes % 1024 == 0 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{} bytes", bytes)
    }
}

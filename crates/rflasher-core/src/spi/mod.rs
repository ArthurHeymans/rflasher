//! SPI types and command structures
//!
//! This module provides types for representing SPI transactions,
//! I/O modes, and standard JEDEC opcodes.

mod address;
mod command;
mod io_mode;
pub mod opcodes;

pub use address::AddressWidth;
pub use command::SpiCommand;
pub use io_mode::{check_io_mode_supported, IoMode};
pub use opcodes::*;

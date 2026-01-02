//! High-level flash operations
//!
//! This module provides high-level operations for reading, writing,
//! and erasing flash chips.
//!
//! # Architecture
//!
//! The flash module provides a unified abstraction over different types
//! of flash programmers:
//!
//! - **SPI-based programmers** (CH341A, FTDI, serprog, etc.) provide raw SPI
//!   command access. Flash operations are implemented by sending SPI commands
//!   directly, using chip metadata from JEDEC probing.
//!
//! - **Opaque programmers** (Intel internal) don't expose raw SPI access.
//!   Instead, they provide address-based read/write/erase operations where
//!   the hardware handles the protocol internally.
//!
//! The [`FlashDevice`] trait abstracts over both types, allowing high-level
//! operations (smart write, layout-based operations, verification) to work
//! with any programmer type.
//!
//! # Example
//!
//! ```ignore
//! use rflasher_core::flash::{FlashDevice, SpiFlashDevice, OpaqueFlashDevice};
//! use rflasher_core::flash::unified;
//!
//! // Using SPI programmer
//! let ctx = flash::probe(master, &db)?;
//! let mut device = SpiFlashDevice::new(master, ctx);
//! unified::smart_write(&mut device, &data, &mut progress)?;
//!
//! // Using opaque programmer  
//! let mut device = OpaqueFlashDevice::new(master);
//! unified::smart_write(&mut device, &data, &mut progress)?;
//! ```

mod context;
mod device;
mod opaque_device;
mod operations;
mod spi_device;
#[cfg(feature = "alloc")]
pub mod unified;

pub use context::FlashContext;
pub use device::FlashDevice;
#[cfg(feature = "alloc")]
pub use device::FlashDeviceExt;
pub use opaque_device::OpaqueFlashDevice;
pub use spi_device::SpiFlashDevice;

// Re-export low-level SPI operations (work with SpiMaster directly)
// For high-level operations that work with any FlashDevice, use the `unified` module
pub use operations::{read, write};

// Re-export detailed probe result
#[cfg(feature = "std")]
pub use operations::{probe_detailed, ProbeResult};

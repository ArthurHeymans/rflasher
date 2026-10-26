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
//! - **SPI-based programmers** (CH341A, FTDI, serprog, Dediprog, etc.)
//!   provide raw SPI command access. Flash operations are implemented by
//!   sending SPI commands directly, using chip metadata from JEDEC probing.
//!   Programmers with faster hardware paths override
//!   [`SpiMaster::read_flash`](crate::programmer::SpiMaster::read_flash) and
//!   [`SpiMaster::program_flash`](crate::programmer::SpiMaster::program_flash).
//!
//! - **Opaque programmers** (Intel hardware sequencing, Linux MTD) don't
//!   expose raw SPI access. The hardware handles the protocol, so they
//!   implement [`FlashDevice`] directly, including their own geometry.
//!
//! The [`FlashDevice`] trait abstracts over both types, allowing high-level
//! operations (smart write, layout-based operations, verification) to work
//! with any programmer type.
//!
//! # Example
//!
//! ```ignore
//! use rflasher_core::flash::{FlashDevice, MutationPolicy, SpiFlashDevice};
//! use rflasher_core::flash::unified;
//!
//! let mut policy = MutationPolicy::default();
//!
//! // Using SPI programmer
//! let ctx = flash::probe(master, &db)?;
//! let mut device = SpiFlashDevice::new(master, ctx);
//! unified::smart_write_with_policy(&mut device, &data, &mut progress, &mut policy)?;
//!
//! // Using an opaque programmer, which is a FlashDevice itself
//! unified::smart_write_with_policy(&mut mtd, &data, &mut progress, &mut policy)?;
//! ```

mod context;
mod device;
mod operations;
#[cfg(feature = "alloc")]
mod policy;
mod protection;
mod spi_device;
#[cfg(feature = "alloc")]
pub mod unified;
#[cfg(feature = "alloc")]
pub use policy::{MutationPolicy, RecoveryBackup};

pub use context::{AddressMode, FlashContext};
pub use device::FlashDevice;
#[cfg(feature = "alloc")]
pub use device::FlashDeviceExt;
pub use spi_device::SpiFlashDevice;

// Re-export low-level SPI operations (work with SpiMaster directly)
// For high-level operations that work with any FlashDevice, use the `unified` module
pub use operations::{OptimalEraseOp, read, select_erase_block, spi_program, spi_read, write};

// Re-export detailed probe result
#[cfg(feature = "std")]
pub use operations::{ProbeOptions, ProbeResult, probe_detailed, probe_with_options};

//! High-level flash programming abstraction
//!
//! This crate provides a unified abstraction for flash programming that hides
//! the differences between SPI-based and opaque programmers. The CLI should
//! only interact with types from this crate, never directly with `SpiMaster`
//! or `OpaqueMaster`.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                        CLI (bin/rflasher)                    │
//! │  - Only imports rflasher-flash and rflasher-core (chip db)  │
//! │  - Never sees SpiMaster or OpaqueMaster                      │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!                              ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     rflasher-flash (this crate)              │
//! │  - FlashHandle: Unified abstraction over Flash + Programmer │
//! │  - ProgrammerRegistry: Opens programmers by name             │
//! │  - Hides SpiMaster/OpaqueMaster from users                   │
//! └─────────────────────────────────────────────────────────────┘
//!                              │
//!              ┌───────────────┴───────────────┐
//!              ▼                               ▼
//! ┌──────────────────────────┐   ┌──────────────────────────┐
//! │    rflasher-core         │   │  Programmer crates       │
//! │  - FlashDevice trait     │   │  - ch341a, ftdi, etc.    │
//! │  - SpiFlashDevice        │   │  - Implement SpiMaster   │
//! │  - OpaqueFlashDevice     │   │    or OpaqueMaster       │
//! │  - Chip database         │   │                          │
//! └──────────────────────────┘   └──────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```ignore
//! use rflasher_flash::{FlashHandle, open_flash};
//! use rflasher_core::chip::ChipDatabase;
//!
//! let db = ChipDatabase::new();
//! // ... load chip database
//!
//! // Open any programmer type with a simple string
//! let handle = open_flash("ch341a", &db)?;
//!
//! // Use the handle - same interface for all programmer types
//! let mut buffer = vec![0u8; handle.size() as usize];
//! handle.read(0, &mut buffer)?;
//! ```

mod handle;
mod registry;

pub use handle::{ChipInfo, FlashHandle};
pub use registry::{
    available_programmers, open_flash, open_spi_programmer, parse_programmer_params,
    programmer_names_short, BoxedSpiMaster, ProgrammerInfo, ProgrammerParams,
};

// Re-export core types that CLI needs
pub use rflasher_core::flash::FlashDevice;
pub use rflasher_core::layout::Layout;

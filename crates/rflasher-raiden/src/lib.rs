//! rflasher-raiden - Raiden Debug SPI (Chrome OS EC USB) programmer support
//!
//! This crate provides support for Raiden Debug SPI, which is a USB SPI bridge
//! protocol used by Chrome OS debug hardware. This allows programming SPI flash
//! chips connected to Chrome OS devices through debug interfaces.
//!
//! # Supported Hardware
//!
//! - SuzyQable (USB-C debug cable)
//! - Servo V4
//! - C2D2
//! - uServo
//! - Servo Micro
//!
//! # Protocol Overview
//!
//! The Raiden Debug SPI protocol communicates via USB bulk transfers. It supports
//! two protocol versions:
//!
//! - **V1**: Simple protocol with 62-byte max payload per transaction
//! - **V2**: Extended protocol supporting larger transfers and device capability querying
//!
//! The protocol version is determined by the USB interface protocol field.
//!
//! # Targets
//!
//! The SPI bridge can be configured to access different targets:
//!
//! - **AP (Application Processor)**: Main SoC flash
//! - **EC (Embedded Controller)**: EC firmware flash
//! - **H1**: Security chip flash
//!
//! # Example
//!
//! ```no_run
//! use rflasher_raiden::{RaidenDebugSpi, RaidenConfig, Target};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! // Open with default settings (first device, AP target)
//! let mut raiden = RaidenDebugSpi::open()?;
//!
//! // Or with specific configuration
//! let config = RaidenConfig {
//!     serial: Some("SERIALNUM".to_string()),
//!     target: Target::Ec,
//! };
//! let mut raiden = RaidenDebugSpi::open_with_config(&config)?;
//!
//! // Read JEDEC ID
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! raiden.execute(&mut cmd)?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
mod device;
#[cfg(feature = "std")]
mod error;
#[cfg(feature = "std")]
mod protocol;

#[cfg(feature = "std")]
pub use device::{parse_options, RaidenConfig, RaidenDebugSpi, RaidenDeviceInfo};
#[cfg(feature = "std")]
pub use error::{RaidenError, Result};
#[cfg(feature = "std")]
pub use protocol::Target;

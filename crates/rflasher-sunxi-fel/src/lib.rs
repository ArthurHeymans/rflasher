//! rflasher-sunxi-fel - Allwinner sunxi FEL SPI NOR programmer support
//!
//! This crate provides support for programming SPI NOR flash through the
//! Allwinner FEL (USB boot) protocol. When an Allwinner SoC boots in FEL
//! mode, its BROM exposes a simple USB protocol for reading/writing memory
//! and executing code. This crate leverages that to drive the SoC's SPI
//! controller and program attached SPI NOR flash chips.
//!
//! # How it works
//!
//! 1. Connect to the device in FEL mode (USB VID:1F3A PID:EFE8)
//! 2. Query the SoC version to identify the chip family
//! 3. Upload a small SPI driver payload to the SoC's SRAM
//! 4. Drive the SPI bus by writing commands to a shared buffer and
//!    executing the payload
//!
//! The SPI driver payload interprets a simple bytecode protocol that
//! handles chip select, data transfer, and SPINOR busy-wait operations.
//!
//! # Supported SoCs
//!
//! - Allwinner H2+/H3 (sun8iw7)
//! - Allwinner H5 (sun50iw2)
//! - Allwinner H6 (sun50iw6)
//! - Allwinner H616/H618 (sun50iw9)
//! - Allwinner A64 (sun50iw1)
//! - Allwinner R328 (sun8iw18)
//! - Allwinner D1/F133 (sun20iw1)
//! - Allwinner V3s/S3 (sun8iw12)
//! - Allwinner F1C100s/F1C200s (suniv)
//! - And more...
//!
//! # Example
//!
//! ```no_run
//! use rflasher_sunxi_fel::SunxiFel;
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! let mut fel = SunxiFel::open()?;
//! println!("Connected to: {}", fel.soc_name());
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! fel.execute(&mut cmd)?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
mod chips;
#[cfg(feature = "std")]
mod device;
#[cfg(feature = "std")]
mod error;
#[cfg(feature = "std")]
mod protocol;

#[cfg(feature = "std")]
pub use device::SunxiFel;
#[cfg(feature = "std")]
pub use error::{Error, Result};

//! rflasher-dediprog - Dediprog SF100/SF200/SF600/SF700 USB programmer support
//!
//! This crate provides support for Dediprog SF-series USB SPI flash programmers.
//! Supported devices:
//! - SF100: Original programmer, single-I/O only
//! - SF200: Similar to SF100 with different form factor
//! - SF600: Faster programmer with dual/quad I/O support
//! - SF600PG2: Second generation SF600
//! - SF700: Latest generation with fastest speeds
//!
//! # Protocol Overview
//!
//! The Dediprog programmers use USB control transfers for commands and bulk
//! transfers for data. The protocol has evolved through multiple versions:
//! - V1: Original protocol (SF100/SF200 < 5.5, SF600 < 6.9)
//! - V2: Extended protocol (SF100/SF200 >= 5.5, SF600 6.9-7.2.21)
//! - V3: Latest protocol (SF600 >= 7.2.22, SF600PG2, SF700)
//!
//! # Example
//!
//! ```no_run
//! use rflasher_dediprog::Dediprog;
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! let mut dediprog = Dediprog::open()?;
//! println!("Device: {}", dediprog.device_string());
//!
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! dediprog.execute(&mut cmd)?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Configuration Options
//!
//! When opening a device, you can specify various options:
//!
//! - `device=N` or `index=N`: Select the Nth device (0-indexed)
//! - `id=SFXXXXXX`: Select device by serial number
//! - `target=1|2`: Select target flash for dual-chip programmers
//! - `spispeed=24M|12M|8M|3M|2.18M|1.5M|750k|375k`: SPI clock speed
//! - `voltage=0|1.8|2.5|3.5` or `1800mV`: Target voltage
//! - `iomode=single|dual|quad`: Maximum I/O mode (SF600+ only)
//!
//! # Example with options
//!
//! ```no_run
//! use rflasher_dediprog::{Dediprog, parse_options};
//!
//! let options = [
//!     ("spispeed", "24M"),
//!     ("voltage", "3.5"),
//!     ("iomode", "dual"),
//! ];
//! let config = parse_options(&options)?;
//! let dediprog = Dediprog::open_with_config(config)?;
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
pub use device::{parse_options, Dediprog, DediprogConfig, DediprogDeviceInfo};
#[cfg(feature = "std")]
pub use error::{DediprogError, Result};
#[cfg(feature = "std")]
pub use protocol::{DeviceType, Protocol};

//! rflasher-ch347 - CH347 USB programmer support
//!
//! This crate provides support for the CH347 USB-to-SPI programmer.
//! The CH347 is a high-speed USB 2.0 (480 Mbps) device that supports
//! SPI, I2C, UART, and JTAG interfaces.
//!
//! # Protocol Overview
//!
//! The CH347 communicates via USB bulk transfers using a dedicated command
//! protocol (different from CH341A). Key features:
//!
//! - Command codes 0xC0-0xCA for SPI operations
//! - Max 510 bytes per USB packet (507 bytes data)
//! - No bit reversal required (unlike CH341A)
//! - Two chip select lines (CS0, CS1)
//! - Configurable SPI speeds from 468.75 kHz to 60 MHz
//!
//! # Device Variants
//!
//! - **CH347T** (PID: 0x55DB): USB to UART+SPI+I2C
//! - **CH347F** (PID: 0x55DE): USB to UART+SPI+I2C+JTAG
//!
//! # Example
//!
//! ```no_run
//! use rflasher_ch347::{Ch347, SpiConfig, SpiSpeed, ChipSelect};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! // Open with default settings (7.5 MHz, mode 0, CS0)
//! let mut ch347 = Ch347::open()?;
//!
//! // Or with custom configuration
//! let config = SpiConfig::new()
//!     .with_speed(SpiSpeed::Speed30M)
//!     .with_cs(ChipSelect::CS1);
//! let mut ch347 = Ch347::open_with_config(config)?;
//!
//! // Read JEDEC ID
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! ch347.execute(&mut cmd)?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Dual/Quad I/O Support (Future)
//!
//! The CH347 hardware supports dual and quad I/O modes, but these are not
//! yet implemented in this driver. The hardware has the following capabilities:
//!
//! - **Dual I/O**: Uses MOSI and MISO for bidirectional 2-bit data transfer
//! - **Quad I/O**: Uses 4 data lines (IO0-IO3) for 4-bit data transfer
//!
//! These modes require additional protocol commands and state management
//! that are planned for future implementation.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
mod device;
#[cfg(feature = "std")]
mod error;
#[cfg(feature = "std")]
mod protocol;

#[cfg(feature = "std")]
pub use device::{parse_options, Ch347, Ch347DeviceInfo};
#[cfg(feature = "std")]
pub use error::{Ch347Error, Result};
#[cfg(feature = "std")]
pub use protocol::{Ch347Variant, ChipSelect, SpiConfig, SpiMode, SpiSpeed};

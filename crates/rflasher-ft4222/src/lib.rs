//! rflasher-ft4222 - FT4222H USB SPI programmer support
//!
//! This crate provides support for the FTDI FT4222H USB to SPI bridge.
//! Unlike FTDI's MPSSE-based chips (FT2232H, FT4232H, FT232H), the FT4222H
//! is a dedicated SPI/I2C/GPIO bridge with a different USB protocol.
//!
//! # Key Differences from FTDI MPSSE
//!
//! - **Dedicated SPI hardware**: The FT4222H has a hardware SPI master,
//!   not a programmable MPSSE engine
//! - **Different USB protocol**: Uses vendor-specific control and bulk transfers,
//!   not MPSSE commands
//! - **Multi-I/O support**: Native support for dual and quad SPI modes
//! - **Higher integration**: Single chip solution without external level shifters
//!
//! # Supported Features
//!
//! - SPI Master mode with clock speeds from ~47 kHz to 40 MHz
//! - Single I/O (1-1-1) mode - standard SPI
//! - Up to 4 chip select outputs (depending on device mode)
//! - 4-byte addressing for >16MB flash chips
//! - Pure USB implementation (no LibFT4222 required)
//!
//! # Limitations
//!
//! - Dual/Quad I/O modes are supported by hardware but not yet implemented
//! - Only SPI mode 0 (CPOL=0, CPHA=0) is currently supported
//!
//! # Example
//!
//! ```no_run
//! use rflasher_ft4222::{Ft4222, SpiConfig};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! // Open with default settings (10 MHz, CS0)
//! let mut ft4222 = Ft4222::open()?;
//!
//! // Or with custom configuration
//! let config = SpiConfig::new()
//!     .with_speed_khz(20_000)  // 20 MHz
//!     .with_cs(1);             // Use CS1
//! let mut ft4222 = Ft4222::open_with_config(config)?;
//!
//! // Read JEDEC ID
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! ft4222.execute(&mut cmd)?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Programmer Options
//!
//! When using the CLI, the following options are available:
//!
//! - `spispeed=<kHz>` - Target SPI clock speed in kHz (default: 10000)
//! - `cs=<0-3>` - Chip select line to use (default: 0)
//! - `iomode=<single|dual|quad>` - I/O mode (default: single)
//!
//! # SPI Clock Speed
//!
//! The FT4222H derives its SPI clock from one of four system clocks
//! (60/24/48/80 MHz) divided by powers of 2 (2 to 512):
//!
//! ```text
//! SPI_clock = system_clock / divisor
//! ```
//!
//! The driver automatically selects the best system clock and divisor
//! combination to achieve a speed at or below the requested value.
//!
//! Common achievable speeds:
//! - 40 MHz (80 / 2)
//! - 30 MHz (60 / 2)
//! - 20 MHz (80 / 4 or 40 / 2)
//! - 15 MHz (60 / 4)
//! - 10 MHz (80 / 8 or 40 / 4)
//! - 7.5 MHz (60 / 8)
//!
//! # Hardware Setup
//!
//! The FT4222H has multiple operating modes set by GPIO pins at power-up.
//! For SPI master operation, ensure:
//!
//! - Mode 0 (default): All 4 GPIOs available as CS outputs
//! - Mode 1: 3 GPIOs as CS, 1 as GPIO
//! - Mode 2: 2 GPIOs as CS, 2 as GPIO
//! - Mode 3: 1 GPIO as CS, 3 as GPIO
//!
//! Connect MOSI, MISO, SCK, and CS to your target SPI flash chip.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
mod device;
#[cfg(feature = "std")]
mod error;
#[cfg(feature = "std")]
mod protocol;

#[cfg(feature = "std")]
pub use device::{parse_options, Ft4222, Ft4222DeviceInfo};
#[cfg(feature = "std")]
pub use error::{Ft4222Error, Result};
#[cfg(feature = "std")]
pub use protocol::{ClockConfig, ClockDivisor, IoMode, SpiConfig, SystemClock};

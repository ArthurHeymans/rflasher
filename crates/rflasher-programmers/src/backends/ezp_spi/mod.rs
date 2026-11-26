//! `rflasher_programmers::ezp_spi` - EZP2013 / CORIGHT EP01 USB programmer support
//!
//! This module supports the EZP2013 USB SPI flash programmer (CORIGHT EP01,
//! also sold as JHW2013 and CH2013B; VID `0x0314`, PID `0x1011`) running
//! firmware 1.2.0.x.
//!
//! Note that this is the Silicon Labs C8051F340-based EZP2013 hardware, not
//! the unrelated WCH CH552/CH554-based EZP2019/EZP2020/EZP2023 programmers.
//!
//! # Protocol Overview
//!
//! The EZP2013 firmware does not expose raw SPI pass-through. Instead, it
//! executes complete flash operations (`READ`, `WRITE`, `ERASE`, `RDID`) using
//! a 16-byte SPI opcode table embedded in every 64-byte bulk command packet.
//! [`EzpSpi`] maps [`rflasher_core::programmer::SpiMaster`] commands onto these
//! firmware operations and uses bulk transfers for `read_flash` and
//! `program_flash` (both page programming and SST25 AAI word programming).
//!
//! Only the first 16 MiB (24-bit addressing) is supported.
//!
//! Async on every target: native drives the async API with a `block_on`
//! boundary in the application, WASM uses WebUSB.
//!
//! # Example
//!
//! ```no_run
//! use rflasher_programmers::UsbProgrammer;
//! use rflasher_programmers::ezp_spi::EzpSpi;
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! # futures_lite::future::block_on(async {
//! let mut ezp = EzpSpi::open_matching((), None).await?;
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! ezp.execute(&mut cmd).await?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # }).unwrap();
//! ```

#[cfg(any(feature = "std", feature = "wasm"))]
mod device;
#[cfg(any(feature = "std", feature = "wasm"))]
mod error;
#[cfg(any(feature = "std", feature = "wasm"))]
mod protocol;

#[cfg(any(feature = "std", feature = "wasm"))]
pub use device::EzpSpi;
#[cfg(any(feature = "std", feature = "wasm"))]
pub use error::{EzpSpiError, Result};

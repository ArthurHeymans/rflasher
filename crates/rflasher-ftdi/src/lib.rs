//! rflasher-ftdi - FTDI MPSSE programmer support
//!
//! This crate provides support for FTDI-based SPI programmers using
//! the MPSSE engine (FT2232H, FT4232H, FT232H, etc.).
//!
//! # Backends
//!
//! Three backends are available:
//!
//! - **`std`** (default): Uses libftdi1 C bindings (`ftdi` crate)
//! - **`native`**: Pure-Rust `rs-ftdi` backend (backed by `nusb`)
//! - **`wasm`**: WebUSB backend using `nusb` + `maybe_async` for browser use
//!
//! Uses `maybe_async` to support both sync and async modes (native + wasm):
//! - With `is_sync` feature (native CLI): blocking/synchronous
//! - Without `is_sync` (WASM): async with WebUSB
//!
//! # Supported Devices
//!
//! - FTDI FT2232H (dual channel, 60 MHz)
//! - FTDI FT4232H (quad channel, 60 MHz)
//! - FTDI FT232H (single channel, 60 MHz)
//! - FTDI FT4233H (quad channel, 60 MHz)
//! - TIAO TUMPA / TUMPA Lite
//! - Amontec JTAGkey
//! - GOEPEL PicoTAP
//! - Olimex ARM-USB-OCD(-H) / ARM-USB-TINY(-H)
//! - Google Servo / Servo V2
//! - Bus Blaster
//! - Flyswatter
//!
//! # Example
//!
//! ```no_run
//! use rflasher_ftdi::{Ftdi, FtdiConfig, FtdiDeviceType};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! // Open with default settings (FT4232H channel A)
//! let mut ftdi = Ftdi::open_first()?;
//!
//! // Or open a specific device type
//! let mut ftdi = Ftdi::open_device(FtdiDeviceType::Ft2232H)?;
//!
//! // Or with full configuration
//! let config = FtdiConfig::for_device(FtdiDeviceType::Ft2232H)
//!     .interface(rflasher_ftdi::FtdiInterface::B)?
//!     .divisor(4)?;
//! let mut ftdi = Ftdi::open(&config)?;
//!
//! // Read JEDEC ID
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! ftdi.execute(&mut cmd)?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Programmer Options
//!
//! When using the CLI, the following options are available:
//!
//! - `type=<device>` - Device type (2232h, 4232h, 232h, jtagkey, tumpa, etc.)
//! - `port=<A|B|C|D>` - Channel to use (default: A)
//! - `divisor=<N>` - Clock divisor (2-65536, even; default: 2)
//! - `serial=<string>` - USB serial number filter
//! - `description=<string>` - USB description filter
//! - `gpiol0=<H|L|C>` - GPIOL0 mode (H=high, L=low, C=CS)
//! - `gpiol1=<H|L|C>` - GPIOL1 mode
//! - `gpiol2=<H|L|C>` - GPIOL2 mode
//! - `gpiol3=<H|L|C>` - GPIOL3 mode
//!
//! # SPI Clock Speed
//!
//! The SPI clock is derived from a 60 MHz base clock (for 'H' devices):
//!
//! ```text
//! SPI_clock = 60 MHz / divisor
//! ```
//!
//! | Divisor | SPI Clock |
//! |---------|-----------|
//! | 2       | 30 MHz    |
//! | 4       | 15 MHz    |
//! | 6       | 10 MHz    |
//! | 10      | 6 MHz     |
//! | 20      | 3 MHz     |
//! | 60      | 1 MHz     |

#![cfg_attr(not(any(feature = "std", feature = "native", feature = "wasm")), no_std)]

// libftdi1 C backend (default `std` feature)
#[cfg(all(feature = "std", not(feature = "native"), not(feature = "wasm")))]
mod device;
#[cfg(all(feature = "std", not(feature = "native"), not(feature = "wasm")))]
mod error;

// Pure-Rust rs-ftdi backend (`native` feature)
#[cfg(all(feature = "native", not(feature = "wasm")))]
mod native_device;
#[cfg(all(feature = "native", not(feature = "wasm")))]
mod native_error;

// WASM/WebUSB backend (`wasm` feature) - uses nusb directly with maybe_async
#[cfg(feature = "wasm")]
mod wasm_device;
#[cfg(feature = "wasm")]
mod wasm_error;

// Protocol constants are shared by all backends
#[cfg(any(feature = "std", feature = "native", feature = "wasm"))]
mod protocol;

// Re-exports: same public API regardless of backend
#[cfg(all(feature = "std", not(feature = "native"), not(feature = "wasm")))]
pub use device::{parse_options, Ftdi, FtdiDeviceInfo};
#[cfg(all(feature = "std", not(feature = "native"), not(feature = "wasm")))]
pub use error::{FtdiError, Result};

#[cfg(all(feature = "native", not(feature = "wasm")))]
pub use native_device::{parse_options, Ftdi, FtdiDeviceInfo};
#[cfg(all(feature = "native", not(feature = "wasm")))]
pub use native_error::{FtdiError, Result};

#[cfg(feature = "wasm")]
pub use wasm_device::Ftdi;
#[cfg(feature = "wasm")]
pub use wasm_error::{FtdiError, Result};

// parse_options is only available in native/std mode (not wasm)
// In WASM, the UI provides configuration directly

// FtdiConfig, FtdiDeviceType, FtdiInterface, and SUPPORTED_DEVICES are
// shared across all backends and live in the protocol module.
#[cfg(any(feature = "std", feature = "native", feature = "wasm"))]
pub use protocol::{FtdiConfig, FtdiDeviceType, FtdiInterface, SUPPORTED_DEVICES};

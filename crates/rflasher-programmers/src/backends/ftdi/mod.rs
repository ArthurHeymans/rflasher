//! `rflasher_programmers::ftdi` - FTDI MPSSE programmer support
//!
//! This module provides support for FTDI-based SPI programmers using
//! the MPSSE engine (FT2232H, FT4232H, FT232H, etc.).
//!
//! # Backends
//!
//! The backend is the pure-Rust `ftdi-nusb` crate on every target:
//!
//! - **`ftdi`**: native `nusb` USB transport
//! - **`ftdi-wasm`**: WebUSB transport
//!
//! Async on every target: native drives the async API with a `block_on`
//! boundary in the application, WASM uses WebUSB.
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
//! use rflasher_programmers::ftdi::{Ftdi, FtdiConfig, FtdiDeviceType};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! # futures_lite::future::block_on(async {
//! // Open with default settings (FT4232H channel A)
//! let mut ftdi = Ftdi::open_first().await?;
//!
//! // Or open a specific device type
//! let mut ftdi = Ftdi::open_device(FtdiDeviceType::Ft2232H).await?;
//!
//! // Or with full configuration
//! let config = FtdiConfig::for_device(FtdiDeviceType::Ft2232H)
//!     .interface(rflasher_programmers::ftdi::FtdiInterface::B)?
//!     .divisor(4)?;
//! let mut ftdi = Ftdi::open(&config).await?;
//!
//! // Read JEDEC ID
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! ftdi.execute(&mut cmd).await?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # }).unwrap();
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

// Pure-Rust ftdi-nusb backend, shared by native and wasm. Async on every
// target.
mod device;
mod error;

// Adapter configuration and device tables
mod protocol;

pub use device::Ftdi;
// parse_options and device enumeration are native-only; in WASM the UI
// provides configuration directly.
#[cfg(all(feature = "ftdi", not(target_arch = "wasm32")))]
pub use device::{FtdiDeviceInfo, parse_options};
pub use error::{FtdiError, Result};

pub use protocol::{FtdiConfig, FtdiDeviceType, FtdiInterface, SUPPORTED_DEVICES};

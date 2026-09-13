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
// Device enumeration is native-only; parse_options is pure and shared with the
// WASM frontend so both surfaces validate the same way.
#[cfg(all(feature = "ftdi", not(target_arch = "wasm32")))]
pub use device::FtdiDeviceInfo;
#[cfg(any(feature = "ftdi", feature = "ftdi-wasm"))]
pub use device::parse_options;
pub use error::{FtdiError, Result};

pub use protocol::{FtdiConfig, FtdiDeviceType, FtdiInterface, SUPPORTED_DEVICES};

// ---------------------------------------------------------------------------
// Option schema shared by the CLI and the web frontend.
// The table sits next to the parser it describes; `crate::catalog` only
// aggregates these modules for listing and validation.
// ---------------------------------------------------------------------------

pub mod schema {
    #[allow(unused_imports)]
    use crate::catalog::{Choice, OptionKind, OptionSpec, Scope, choice};

    const FTDI_TYPES: &[Choice] = &[
        choice("2232h", "FT2232H"),
        choice("4232h", "FT4232H"),
        choice("232h", "FT232H"),
        choice("4233h", "FT4233H"),
        choice("tumpa", "TUMPA"),
        choice("tumpalite", "TUMPA Lite"),
        choice("kt-link", "KT-LINK"),
        choice("jtagkey", "JTAGkey"),
        choice("picotap", "PicoTAP"),
        choice("openmoko", "OpenMoko Debug Board"),
        choice("arm-usb-ocd", "ARM-USB-OCD"),
        choice("arm-usb-tiny", "ARM-USB-TINY"),
        choice("arm-usb-ocd-h", "ARM-USB-OCD-H"),
        choice("arm-usb-tiny-h", "ARM-USB-TINY-H"),
        choice("google-servo", "Google Servo"),
        choice("google-servo-v2", "Google Servo V2"),
        choice("google-servo-v2-legacy", "Google Servo V2 Legacy"),
        choice("busblaster", "Bus Blaster"),
        choice("flyswatter", "Flyswatter"),
    ];

    const FTDI_CHANNELS: &[Choice] = &[
        choice("A", "Channel A"),
        choice("B", "Channel B"),
        choice("C", "Channel C"),
        choice("D", "Channel D"),
    ];

    const FTDI_GPIO_MODES: &[Choice] = &[
        choice("H", "High"),
        choice("L", "Low"),
        choice("C", "Chip select"),
        choice("I", "Input"),
    ];

    pub const OPTIONS: &[OptionSpec] = &[
        OptionSpec {
            key: "type",
            label: "Device type",
            help: "FTDI-based device type",
            kind: OptionKind::Choice(FTDI_TYPES),
            default: Some("4232h"),
            scope: Scope::Any,
        },
        OptionSpec {
            key: "port",
            label: "Channel",
            help: "MPSSE channel to use (also accepted as `channel`)",
            kind: OptionKind::Choice(FTDI_CHANNELS),
            default: None,
            scope: Scope::Any,
        },
        OptionSpec {
            key: "divisor",
            label: "Clock divisor",
            help: "MPSSE clock divisor (2-65534, even); clock = 60 MHz / divisor",
            kind: OptionKind::Int { min: 2, max: 65534 },
            default: None,
            scope: Scope::Any,
        },
        OptionSpec {
            key: "serial",
            label: "Serial number",
            help: "Select a device by USB serial number",
            kind: OptionKind::Text,
            default: None,
            scope: Scope::NativeOnly,
        },
        OptionSpec {
            key: "description",
            label: "Description",
            help: "Select a device by USB description",
            kind: OptionKind::Text,
            default: None,
            scope: Scope::NativeOnly,
        },
        OptionSpec {
            key: "gpiol0",
            label: "GPIOL0",
            help: "GPIOL0 mode",
            kind: OptionKind::Choice(FTDI_GPIO_MODES),
            default: None,
            scope: Scope::Any,
        },
        OptionSpec {
            key: "gpiol1",
            label: "GPIOL1",
            help: "GPIOL1 mode",
            kind: OptionKind::Choice(FTDI_GPIO_MODES),
            default: None,
            scope: Scope::Any,
        },
        OptionSpec {
            key: "gpiol2",
            label: "GPIOL2",
            help: "GPIOL2 mode",
            kind: OptionKind::Choice(FTDI_GPIO_MODES),
            default: None,
            scope: Scope::Any,
        },
        OptionSpec {
            key: "gpiol3",
            label: "GPIOL3",
            help: "GPIOL3 mode",
            kind: OptionKind::Choice(FTDI_GPIO_MODES),
            default: None,
            scope: Scope::Any,
        },
    ];

    pub fn validate_options(options: &[(&str, &str)]) -> std::result::Result<(), String> {
        crate::ftdi::parse_options(options)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

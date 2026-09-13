//! `rflasher_programmers::dediprog` - Dediprog SF100/SF200/SF600/SF700 USB programmer support
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
//! Async on every target: native drives the async API with a `block_on`
//! boundary in the application, WASM uses WebUSB.
//!
//! # Example
//!
//! ```no_run
//! use rflasher_programmers::dediprog::Dediprog;
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! # futures_lite::future::block_on(async {
//! let mut dediprog = Dediprog::open().await?;
//! println!("Device: {}", dediprog.device_string());
//!
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! dediprog.execute(&mut cmd).await?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # }).unwrap();
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
//! use rflasher_programmers::dediprog::{Dediprog, parse_options};
//!
//! let options = [
//!     ("spispeed", "24M"),
//!     ("voltage", "3.5"),
//!     ("iomode", "dual"),
//! ];
//! # futures_lite::future::block_on(async {
//! let config = parse_options(&options)?;
//! let dediprog = Dediprog::open_with_config(config).await?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # }).unwrap();
//! ```

#[cfg(any(feature = "std", feature = "wasm"))]
mod device;
#[cfg(any(feature = "std", feature = "wasm"))]
mod error;
#[cfg(any(feature = "std", feature = "wasm"))]
mod protocol;

#[cfg(all(feature = "std", not(feature = "wasm")))]
pub use device::DediprogDeviceInfo;
#[cfg(any(feature = "std", feature = "wasm"))]
pub use device::{Dediprog, DediprogConfig, parse_options};
#[cfg(any(feature = "std", feature = "wasm"))]
pub use error::{DediprogError, Result};
#[cfg(any(feature = "std", feature = "wasm"))]
pub use protocol::{DeviceType, Protocol};

// ---------------------------------------------------------------------------
// Option schema shared by the CLI and the web frontend.
// The table sits next to the parser it describes; `crate::catalog` only
// aggregates these modules for listing and validation.
// ---------------------------------------------------------------------------

pub mod schema {
    #[allow(unused_imports)]
    use crate::catalog::{Choice, OptionKind, OptionSpec, Scope, choice};

    const DEDIPROG_SPEEDS: &[Choice] = &[
        choice("24M", "24 MHz"),
        choice("12M", "12 MHz"),
        choice("8M", "8 MHz"),
        choice("3M", "3 MHz"),
        choice("2.18M", "2.18 MHz"),
        choice("1.5M", "1.5 MHz"),
        choice("750k", "750 kHz"),
        choice("375k", "375 kHz"),
    ];

    const DEDIPROG_VOLTAGES: &[Choice] = &[
        choice("0", "Off (no target voltage)"),
        choice("1.8", "1.8 V"),
        choice("2.5", "2.5 V"),
        choice("3.5", "3.5 V"),
    ];

    const IO_MODES: &[Choice] = &[
        choice("single", "Single"),
        choice("dual", "Dual"),
        choice("quad", "Quad"),
    ];

    pub const OPTIONS: &[OptionSpec] = &[
        OptionSpec {
            key: "device",
            label: "Device index",
            help: "Select the Nth connected device (0-indexed)",
            kind: OptionKind::Int { min: 0, max: 255 },
            default: None,
            scope: Scope::NativeOnly,
        },
        OptionSpec {
            key: "id",
            label: "Device ID",
            help: "Select a device by serial number (e.g. SF123456)",
            kind: OptionKind::Text,
            default: None,
            scope: Scope::NativeOnly,
        },
        OptionSpec {
            key: "target",
            label: "Target",
            help: "Flash target on dual-chip programmers",
            kind: OptionKind::Choice(&[choice("1", "Flash 1"), choice("2", "Flash 2")]),
            default: Some("1"),
            scope: Scope::Any,
        },
        OptionSpec {
            key: "spispeed",
            label: "SPI speed",
            help: "SPI clock preset",
            kind: OptionKind::Choice(DEDIPROG_SPEEDS),
            default: Some("12M"),
            scope: Scope::Any,
        },
        OptionSpec {
            key: "voltage",
            label: "Target voltage",
            help: "Target voltage (0 = off; the programmer's own default is 3.5 V)",
            kind: OptionKind::Choice(DEDIPROG_VOLTAGES),
            default: Some("3.5"),
            scope: Scope::Any,
        },
        OptionSpec {
            key: "iomode",
            label: "I/O mode",
            help: "Maximum I/O mode (SF600 and newer)",
            kind: OptionKind::Choice(IO_MODES),
            default: None,
            scope: Scope::Any,
        },
    ];

    pub fn validate_options(options: &[(&str, &str)]) -> std::result::Result<(), String> {
        crate::dediprog::parse_options(options)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

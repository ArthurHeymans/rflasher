//! `rflasher_programmers::ch347` - CH347 USB programmer support
//!
//! This crate provides support for the CH347 USB-to-SPI programmer.
//! The CH347 is a high-speed USB 2.0 (480 Mbps) device that supports
//! SPI, I2C, UART, and JTAG interfaces.
//!
//! Async on every target: native drives the async API with a `block_on`
//! boundary in the application, WASM uses WebUSB.
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
//! use rflasher_programmers::ch347::{Ch347, SpiConfig, SpiSpeed, ChipSelect};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! # futures_lite::future::block_on(async {
//! // Open with default settings (7.5 MHz, mode 0, CS0)
//! let mut ch347 = Ch347::open().await?;
//!
//! // Or with custom configuration
//! let config = SpiConfig::new()
//!     .with_speed(SpiSpeed::Speed30M)
//!     .with_cs(ChipSelect::CS1);
//! let mut ch347 = Ch347::open_with_config(config).await?;
//!
//! // Read JEDEC ID
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! ch347.execute(&mut cmd).await?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # }).unwrap();
//! ```
//!
//! # Limitations
//!
//! This driver currently only implements standard single-bit SPI mode.
//! The CH347 hardware may support dual and quad I/O modes, but:
//!
//! - **Dual I/O (2-bit)**: Not supported - requires additional hardware investigation
//! - **Quad I/O (4-bit)**: Not supported - requires additional hardware investigation
//!
//! The USB protocol commands for these modes are not documented, and it's unclear
//! if the CH347 hardware actually supports them. Standard SPI mode covers the vast
//! majority of flash chip programming use cases.

#[cfg(any(feature = "std", feature = "wasm"))]
mod device;
#[cfg(any(feature = "std", feature = "wasm"))]
mod error;
#[cfg(any(feature = "std", feature = "wasm"))]
mod protocol;

#[cfg(any(feature = "std", feature = "wasm"))]
pub use device::Ch347;
#[cfg(all(feature = "std", not(feature = "wasm")))]
pub use device::Ch347DeviceInfo;
#[cfg(feature = "std")]
pub use device::parse_options;
#[cfg(any(feature = "std", feature = "wasm"))]
pub use error::{Ch347Error, Result};
#[cfg(any(feature = "std", feature = "wasm"))]
pub use protocol::{Ch347Variant, ChipSelect, SpiConfig, SpiMode, SpiSpeed};

// ---------------------------------------------------------------------------
// Option schema shared by the CLI and the web frontend.
// The table sits next to the parser it describes; `crate::catalog` only
// aggregates these modules for listing and validation.
// ---------------------------------------------------------------------------

pub mod schema {
    #[allow(unused_imports)]
    use crate::catalog::{Choice, OptionKind, OptionSpec, Scope, choice};

    const CH347_SPEEDS: &[u32] = &[60000, 30000, 15000, 7500, 3750, 1875, 937, 468];

    const ON_OFF_CS: &[Choice] = &[choice("0", "CS0"), choice("1", "CS1")];

    const SPI_MODES: &[Choice] = &[
        choice("0", "Mode 0"),
        choice("1", "Mode 1"),
        choice("2", "Mode 2"),
        choice("3", "Mode 3"),
    ];

    pub const OPTIONS: &[OptionSpec] = &[
        OptionSpec {
            key: "spispeed",
            label: "SPI speed",
            help: "SPI clock in kHz (selects a supported speed without exceeding it when possible)",
            kind: OptionKind::SpeedKhz {
                presets: CH347_SPEEDS,
            },
            default: Some("7500"),
            scope: Scope::Any,
        },
        OptionSpec {
            key: "spimode",
            label: "SPI mode",
            help: "SPI mode (clock polarity and phase)",
            kind: OptionKind::Choice(SPI_MODES),
            default: Some("0"),
            scope: Scope::Any,
        },
        OptionSpec {
            key: "cs",
            label: "Chip select",
            help: "Chip select line",
            kind: OptionKind::Choice(ON_OFF_CS),
            default: Some("0"),
            scope: Scope::Any,
        },
    ];

    pub fn validate_options(options: &[(&str, &str)]) -> std::result::Result<(), String> {
        crate::ch347::parse_options(options)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

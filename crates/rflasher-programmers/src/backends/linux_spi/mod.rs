//! `rflasher_programmers::linux_spi` - Linux spidev support
//!
//! This crate provides support for Linux spidev-based SPI flash access
//! via the `/dev/spidevX.Y` device interface.
//!
//! # Overview
//!
//! The Linux SPI driver exposes SPI controllers through character devices
//! at `/dev/spidevX.Y` where X is the bus number and Y is the chip select.
//!
//! # Example
//!
//! ```no_run
//! use rflasher_programmers::linux_spi::{LinuxSpi, LinuxSpiConfig};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! # futures_lite::future::block_on(async {
//! // Open with default settings (2 MHz, mode 0)
//! let mut spi = LinuxSpi::open_device("/dev/spidev0.0")?;
//!
//! // Or with custom settings
//! let config = LinuxSpiConfig::new("/dev/spidev0.0")
//!     .with_speed(4_000_000)  // 4 MHz
//!     .with_mode(0);
//! let mut spi = LinuxSpi::open(&config)?;
//!
//! // Read JEDEC ID
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! spi.execute(&mut cmd).await?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # }).unwrap();
//! ```
//!
//! # Usage with rflasher CLI
//!
//! ```bash
//! # Probe chip using default settings
//! rflasher probe -p linux_spi:dev=/dev/spidev0.0
//!
//! # Specify SPI speed in kHz
//! rflasher probe -p linux_spi:dev=/dev/spidev0.0,spispeed=4000
//!
//! # Specify SPI mode
//! rflasher read -p linux_spi:dev=/dev/spidev0.0,mode=3 -o flash.bin
//! ```
//!
//! # System Requirements
//!
//! - Linux kernel with spidev support enabled (`CONFIG_SPI_SPIDEV`)
//! - Read/write access to `/dev/spidevX.Y` device
//! - May require adding user to `spi` group or using udev rules
//!
//! # Known Working Devices
//!
//! - Raspberry Pi (all models)
//! - BeagleBone Black
//! - HummingBoard
//! - Any board with spidev-enabled SPI controller

pub mod device;
pub mod error;

// Re-exports
pub use device::{LinuxSpi, LinuxSpiConfig, mode, parse_options};
pub use error::{LinuxSpiError, Result};

// ---------------------------------------------------------------------------
// Option schema shared by the CLI and the web frontend.
// The table sits next to the parser it describes; `crate::catalog` only
// aggregates these modules for listing and validation.
// ---------------------------------------------------------------------------

pub mod schema {
    #[allow(unused_imports)]
    use crate::catalog::{Choice, OptionKind, OptionSpec, Scope, choice};

    const SPI_MODES: &[Choice] = &[
        choice("0", "Mode 0"),
        choice("1", "Mode 1"),
        choice("2", "Mode 2"),
        choice("3", "Mode 3"),
    ];

    pub const OPTIONS: &[OptionSpec] = &[
        OptionSpec {
            key: "dev",
            label: "Device",
            help: "spidev device node (e.g. /dev/spidev0.0)",
            kind: OptionKind::Path,
            default: None,
            scope: Scope::NativeOnly,
        },
        OptionSpec {
            key: "spispeed",
            label: "SPI speed",
            help: "SPI clock in kHz",
            kind: OptionKind::SpeedKhz { presets: &[] },
            default: None,
            scope: Scope::NativeOnly,
        },
        OptionSpec {
            key: "mode",
            label: "SPI mode",
            help: "SPI mode (0-3)",
            kind: OptionKind::Choice(SPI_MODES),
            default: None,
            scope: Scope::NativeOnly,
        },
    ];

    pub fn validate_options(options: &[(&str, &str)]) -> std::result::Result<(), String> {
        crate::linux_spi::parse_options(options).map(|_| ())
    }
}

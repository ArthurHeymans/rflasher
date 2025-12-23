//! rflasher-linux-spi - Linux spidev support
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
//! use rflasher_linux_spi::{LinuxSpi, LinuxSpiConfig};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
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
//! spi.execute(&mut cmd)?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
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
pub use device::{mode, parse_options, LinuxSpi, LinuxSpiConfig};
pub use error::{LinuxSpiError, Result};

/// Open a Linux SPI device and return a boxed SpiMaster
///
/// This is a convenience function for use in the CLI programmer dispatch.
///
/// # Arguments
///
/// * `options` - Slice of (key, value) pairs from programmer string parsing
///
/// # Example Options
///
/// - `dev=/dev/spidev0.0` - Required: device path
/// - `spispeed=4000` - Optional: speed in kHz (default: 2000)
/// - `mode=0` - Optional: SPI mode 0-3 (default: 0)
pub fn open_linux_spi(
    options: &[(&str, &str)],
) -> std::result::Result<Box<dyn rflasher_core::programmer::SpiMaster>, Box<dyn std::error::Error>>
{
    let config = parse_options(options)?;
    let spi = LinuxSpi::open(&config)?;
    Ok(Box::new(spi))
}

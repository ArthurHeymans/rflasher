//! rflasher-linux-gpio - Linux GPIO bitbang SPI support
//!
//! This crate provides support for SPI flash access via GPIO bitbanging
//! using the Linux character device GPIO interface (gpiocdev).
//!
//! # Overview
//!
//! GPIO bitbanging allows SPI communication using regular GPIO pins, without
//! requiring a dedicated SPI controller. This is useful on platforms like
//! Raspberry Pi where GPIO pins are easily accessible.
//!
//! The implementation uses the gpiocdev crate which provides a pure Rust
//! implementation of the GPIO character device interface, which is the modern
//! way to access GPIO on Linux, replacing the deprecated sysfs interface.
//!
//! # Example
//!
//! ```no_run
//! use rflasher_linux_gpio::{LinuxGpioSpi, LinuxGpioSpiConfig};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! // Configure GPIO pins for SPI
//! let config = LinuxGpioSpiConfig::new("/dev/gpiochip0", 25, 11, 10, 9);
//! //                                    device          CS  SCK MOSI MISO
//!
//! let mut spi = LinuxGpioSpi::open(&config)?;
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
//! # Basic usage with GPIO chip and pin numbers
//! rflasher probe -p linux_gpio_spi:dev=/dev/gpiochip0,cs=25,sck=11,mosi=10,miso=9
//!
//! # Using gpiochip number instead of device path
//! rflasher probe -p linux_gpio_spi:gpiochip=0,cs=25,sck=11,mosi=10,miso=9
//!
//! # With custom SPI speed (in kHz, approximate)
//! rflasher read -p linux_gpio_spi:dev=/dev/gpiochip0,cs=25,sck=11,mosi=10,miso=9,spispeed=500 -o flash.bin
//! ```
//!
//! # GPIO Pin Wiring
//!
//! Connect the SPI flash chip to GPIO pins as follows:
//!
//! | Flash Pin | GPIO Function | Description |
//! |-----------|---------------|-------------|
//! | CS#       | CS (output)   | Chip Select (directly, no pull-up needed) |
//! | CLK       | SCK (output)  | Serial Clock |
//! | DI/MOSI   | MOSI (output) | Master Out Slave In |
//! | DO/MISO   | MISO (input)  | Master In Slave Out |
//! | VCC       | 3.3V          | Power supply |
//! | GND       | GND           | Ground |
//! | WP#       | 3.3V          | Write Protect (tie high to disable) |
//! | HOLD#     | 3.3V          | Hold (tie high to disable) |
//!
//! # System Requirements
//!
//! - Linux kernel 4.8+ with GPIO character device support (kernel 5.5+ for v2 API)
//! - Access to `/dev/gpiochipN` devices (may require root or udev rules)
//!
//! # Known Working Platforms
//!
//! - Raspberry Pi (all models)
//! - BeagleBone
//! - Any platform with GPIO accessible via /dev/gpiochip interface

pub mod device;
pub mod error;

// Re-exports
pub use device::{parse_options, LinuxGpioSpi, LinuxGpioSpiConfig};
pub use error::{LinuxGpioError, Result};

/// Open a Linux GPIO SPI device and return a boxed SpiMaster
///
/// This is a convenience function for use in the CLI programmer dispatch.
///
/// # Arguments
///
/// * `options` - Slice of (key, value) pairs from programmer string parsing
///
/// # Example Options
///
/// - `dev=/dev/gpiochip0` - GPIO chip device path (or use gpiochip=N)
/// - `gpiochip=0` - GPIO chip number (alternative to dev)
/// - `cs=25` - CS pin GPIO offset (required)
/// - `sck=11` - SCK pin GPIO offset (required)
/// - `mosi=10` or `io0=10` - MOSI pin GPIO offset (required)
/// - `miso=9` or `io1=9` - MISO pin GPIO offset (required)
/// - `io2=N` - IO2 pin for quad mode (optional)
/// - `io3=N` - IO3 pin for quad mode (optional)
/// - `spispeed=100` - SPI speed in kHz (optional, default ~100 kHz)
pub fn open_linux_gpio_spi(
    options: &[(&str, &str)],
) -> std::result::Result<Box<dyn rflasher_core::programmer::SpiMaster>, Box<dyn std::error::Error>>
{
    let config = parse_options(options)?;
    let spi = LinuxGpioSpi::open(&config)?;
    Ok(Box::new(spi))
}

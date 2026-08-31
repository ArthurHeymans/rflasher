//! `rflasher_programmers::linux_gpio` - Linux GPIO bitbang SPI support
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
//! use rflasher_programmers::linux_gpio::{LinuxGpioSpi, LinuxGpioSpiConfig};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! # futures_lite::future::block_on(async {
//! // Configure GPIO pins for SPI
//! let config = LinuxGpioSpiConfig::new("/dev/gpiochip0", 25, 11, 10, 9);
//! //                                    device          CS  SCK MOSI MISO
//!
//! let mut spi = LinuxGpioSpi::open(&config)?;
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
//! - Linux kernel 4.8+ with GPIO character device support
//! - Access to `/dev/gpiochipN` devices (may require root or udev rules)
//!
//! # GPIO Character Device API Versions
//!
//! This crate supports both versions of the Linux GPIO character device API:
//!
//! - **uAPI v1** (Linux 4.8+): The original GPIO character device interface
//! - **uAPI v2** (Linux 5.10+): The newer interface with improved line configuration
//!
//! The appropriate API version is automatically detected and used at runtime.
//! This ensures compatibility with both older systems (e.g., Raspberry Pi with
//! older kernels) and newer systems with the latest kernel features.
//!
//! # Known Working Platforms
//!
//! - Raspberry Pi (all models)
//! - BeagleBone
//! - Any platform with GPIO accessible via /dev/gpiochip interface

pub mod device;
pub mod error;

// Re-exports
pub use device::{LinuxGpioSpi, LinuxGpioSpiConfig, parse_options};
pub use error::{LinuxGpioError, Result};


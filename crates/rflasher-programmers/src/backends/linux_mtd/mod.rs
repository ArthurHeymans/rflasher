//! `rflasher_programmers::linux_mtd` - Linux MTD (Memory Technology Device) support
//!
//! This crate provides support for accessing flash chips via the Linux MTD
//! subsystem. MTD devices are exposed at `/dev/mtdN` and provide high-level
//! read/write/erase operations, abstracting away the underlying flash protocol.
//!
//! # Overview
//!
//! The Linux MTD subsystem handles the low-level flash protocol, timing, and
//! protection. This makes it suitable for:
//!
//! - On-board SPI flash connected via the Linux SPI subsystem
//! - BIOS/firmware flash on some x86 systems
//! - NOR flash on embedded systems
//!
//! Unlike `linux_spi`, which provides raw SPI access, `linux_mtd` provides
//! an opaque interface with higher-level operations.
//!
//! # Example
//!
//! ```ignore
//! use rflasher_programmers::linux_mtd::{LinuxMtd, LinuxMtdConfig};
//! use rflasher_core::programmer::OpaqueMaster;
//!
//! // Open MTD device 0
//! let config = LinuxMtdConfig::new(0);
//! let mut mtd = LinuxMtd::open(&config)?;
//!
//! // Read first 4KB
//! let mut buffer = vec![0u8; 4096];
//! mtd.read(0, &mut buffer)?;
//!
//! // Get device info
//! let info = mtd.info();
//! println!("Device: {}", info.name);
//! println!("Size: {} bytes", info.total_size);
//! println!("Erase size: {} bytes", info.erase_size);
//! ```
//!
//! # Usage with rflasher CLI
//!
//! ```bash
//! # Probe using MTD device 0
//! rflasher probe -p linux_mtd:dev=0
//!
//! # Read entire flash
//! rflasher read -p linux_mtd:dev=0 -o flash_backup.bin
//!
//! # Write new firmware
//! rflasher write -p linux_mtd:dev=0 -i firmware.bin
//! ```
//!
//! # System Requirements
//!
//! - Linux kernel with MTD support (`CONFIG_MTD`)
//! - MTD NOR flash driver for your specific flash controller
//! - Read/write access to `/dev/mtdN` device
//! - May require root access or udev rules
//!
//! # Device Discovery
//!
//! List available MTD devices:
//! ```bash
//! cat /proc/mtd
//! # or
//! ls -la /dev/mtd*
//! ```
//!
//! View device details:
//! ```bash
//! cat /sys/class/mtd/mtd0/name
//! cat /sys/class/mtd/mtd0/size
//! cat /sys/class/mtd/mtd0/erasesize
//! cat /sys/class/mtd/mtd0/type  # should be "nor"
//! ```

pub mod device;
pub mod error;

// Re-exports
pub use device::{LinuxMtd, LinuxMtdConfig, MtdInfo, parse_options};
pub use error::{LinuxMtdError, Result};

// ---------------------------------------------------------------------------
// Option schema shared by the CLI and the web frontend.
// The table sits next to the parser it describes; `crate::catalog` only
// aggregates these modules for listing and validation.
// ---------------------------------------------------------------------------

pub mod schema {
    #[allow(unused_imports)]
    use crate::catalog::{Choice, OptionKind, OptionSpec, Scope, choice};

    pub const OPTIONS: &[OptionSpec] = &[OptionSpec {
        key: "dev",
        label: "MTD device",
        help: "MTD device number (e.g. 0 for /dev/mtd0)",
        kind: OptionKind::Int { min: 0, max: 255 },
        default: None,
        scope: Scope::NativeOnly,
    }];

    pub fn validate_options(options: &[(&str, &str)]) -> std::result::Result<(), String> {
        crate::linux_mtd::parse_options(options)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

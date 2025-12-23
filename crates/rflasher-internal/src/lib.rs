//! rflasher-internal - Internal chipset flash programmer
//!
//! This crate provides support for internal SPI controllers in Intel and AMD chipsets.
//!
//! # Overview
//!
//! Modern chipsets include integrated SPI controllers that can be used to access
//! the system flash chip. This is the "internal programmer" mode used by flashprog
//! and similar tools.
//!
//! # Supported Chipsets
//!
//! ## Intel
//! - ICH7 through ICH10
//! - 5 Series through 9 Series (Ibex Peak, Cougar Point, Panther Point, etc.)
//! - 100 Series through 500 Series (Sunrise Point, Cannon Point, Tiger Point, etc.)
//! - Atom platforms (Bay Trail, Apollo Lake, Gemini Lake, Elkhart Lake)
//! - Server platforms (C620 Lewisburg, C740 Emmitsburg)
//! - Latest platforms (Meteor Lake, Lunar Lake, Arrow Lake)
//!
//! ## AMD
//! - Renoir/Cezanne (FCH 790b rev 0x51)
//! - Pinnacle Ridge (FCH 790b rev 0x59)
//! - Raven Ridge/Matisse/Starship (FCH 790b rev 0x61)
//! - Raphael/Mendocino/Phoenix/Rembrandt (FCH 790b rev 0x71)
//!
//! # Warnings
//!
//! Many chipsets are marked as "untested" or have support that depends on
//! configuration (Intel Flash Descriptor settings). When an untested chipset
//! is detected, a warning will be logged. Users should report success or
//! failure to help improve testing coverage.
//!
//! # References
//!
//! - flashprog/ichspi.c - Intel SPI controller implementation
//! - flashprog/amd_spi100.c - AMD SPI100 controller implementation
//! - flashprog/chipset_enable.c - Chipset detection and enabling
//! - flashprog/ich_descriptors.c - Intel Flash Descriptor parsing

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod amd_enable;
pub mod amd_pci;
pub mod amd_spi100;
pub mod chipset;
pub mod controller;
pub mod error;
pub mod ich_regs;
pub mod ichspi;
pub mod intel_pci;
pub mod pci;
pub mod physmap;
pub mod programmer;

pub use amd_enable::{enable_amd_spi100, AmdSpi100Info};
pub use amd_pci::{AmdChipset, AmdChipsetEnable, AMD_CHIPSETS, AMD_VID};
pub use amd_spi100::Spi100Controller;
pub use chipset::{BusType, ChipsetEnable, IchChipset, TestStatus};
pub use error::{InternalError, PciAccessError};
pub use ichspi::{IchSpiController, SpiMode};
pub use intel_pci::{find_chipset, INTEL_CHIPSETS, INTEL_VID};
pub use pci::{
    find_amd_chipset, find_intel_chipset, scan_for_amd_chipsets, scan_for_intel_chipsets,
    scan_pci_bus, DetectedAmdChipset, PciDevice,
};
pub use physmap::PhysMap;
pub use programmer::{programmer_info, InternalOptions, InternalProgrammer};

// Re-export AnyDetectedChipset at crate root for convenience
pub use AnyDetectedChipset as AnyChipset;

/// Result type for internal programmer operations
pub type Result<T> = core::result::Result<T, InternalError>;

/// Information about a detected chipset
#[derive(Debug, Clone)]
pub struct DetectedChipset {
    /// The chipset enable entry from the database
    pub enable: &'static ChipsetEnable,
    /// PCI bus number
    pub bus: u8,
    /// PCI device number  
    pub device: u8,
    /// PCI function number
    pub function: u8,
    /// PCI revision ID
    pub revision_id: u8,
}

impl DetectedChipset {
    /// Returns the chipset name
    pub fn name(&self) -> &'static str {
        self.enable.device_name
    }

    /// Returns the vendor name
    pub fn vendor(&self) -> &'static str {
        self.enable.vendor_name
    }

    /// Returns the test status
    pub fn status(&self) -> TestStatus {
        self.enable.status
    }

    /// Returns the chipset type (for determining SPI engine variant)
    pub fn chipset_type(&self) -> IchChipset {
        self.enable.chipset
    }

    /// Returns true if this chipset should generate a warning
    pub fn should_warn(&self) -> bool {
        self.enable.status.should_warn()
    }

    /// Get the warning/status message for this chipset
    pub fn status_message(&self) -> Option<&'static str> {
        self.enable.status.message()
    }

    /// Log warnings for untested/dependent chipsets
    pub fn log_warnings(&self) {
        match self.enable.status {
            TestStatus::Untested => {
                log::warn!(
                    "Chipset {} {} ({:04x}:{:04x}) is UNTESTED.",
                    self.enable.vendor_name,
                    self.enable.device_name,
                    self.enable.vendor_id,
                    self.enable.device_id
                );
                log::warn!(
                    "If you are using an up-to-date version and were (not) able to \
                     successfully access flash with it, please report your results."
                );
            }
            TestStatus::Depends => {
                log::info!(
                    "Support for {} {} depends on configuration \
                     (e.g., Intel Flash Descriptor settings).",
                    self.enable.vendor_name,
                    self.enable.device_name
                );
            }
            TestStatus::Bad => {
                log::error!(
                    "Chipset {} {} is NOT SUPPORTED.",
                    self.enable.vendor_name,
                    self.enable.device_name
                );
            }
            _ => {}
        }
    }
}

/// Detected chipset type (Intel or AMD)
#[derive(Debug, Clone)]
pub enum AnyDetectedChipset {
    /// Intel chipset
    Intel(DetectedChipset),
    /// AMD chipset
    Amd(DetectedAmdChipset),
}

impl AnyDetectedChipset {
    /// Get the chipset name
    pub fn name(&self) -> &str {
        match self {
            Self::Intel(c) => c.name(),
            Self::Amd(c) => c.name(),
        }
    }

    /// Get the vendor name
    pub fn vendor(&self) -> &str {
        match self {
            Self::Intel(c) => c.vendor(),
            Self::Amd(c) => c.vendor(),
        }
    }

    /// Log warnings for the chipset
    pub fn log_warnings(&self) {
        match self {
            Self::Intel(c) => c.log_warnings(),
            Self::Amd(c) => c.log_warnings(),
        }
    }
}

/// Scan for supported chipsets (Intel or AMD)
///
/// This function scans the PCI bus for known Intel and AMD chipsets and returns
/// information about any detected chipset. It will log warnings for
/// untested or configuration-dependent chipsets.
///
/// # Returns
///
/// - `Ok(Some(detected))` - A supported chipset was found
/// - `Ok(None)` - No supported chipset was found
/// - `Err(error)` - An error occurred during detection
///
/// # Errors
///
/// - `InternalError::MultipleChipsets` - Multiple supported chipsets found
/// - `InternalError::PciAccess` - Failed to access PCI bus
///
/// # Example
///
/// ```ignore
/// match detect_chipset()? {
///     Some(chipset) => {
///         println!("Found: {} {}", chipset.vendor(), chipset.name());
///         chipset.log_warnings();
///     }
///     None => {
///         println!("No supported chipset found");
///     }
/// }
/// ```
pub fn detect_chipset() -> Result<Option<AnyDetectedChipset>> {
    // Try Intel first
    if let Some(intel) = find_intel_chipset()? {
        log::info!("Found Intel chipset: {} {}", intel.vendor(), intel.name());
        return Ok(Some(AnyDetectedChipset::Intel(intel)));
    }

    // Try AMD
    if let Some(amd) = find_amd_chipset()? {
        log::info!("Found AMD chipset: {} {}", amd.vendor(), amd.name());
        return Ok(Some(AnyDetectedChipset::Amd(amd)));
    }

    Ok(None)
}

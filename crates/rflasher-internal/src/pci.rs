//! Intel and AMD chipset detection over generic PCI devices.
//!
//! PCI enumeration and configuration-space access live in [`rflasher_pci`].
//! This module deliberately contains only rflasher-internal chipset policy.

use crate::DetectedChipset;
use crate::amd_pci::{AMD_VID, AmdChipsetEnable, find_chipset as find_amd_chipset_entry};
use crate::error::InternalError;
use crate::intel_pci::{INTEL_VID, find_chipset};

pub use rflasher_pci::PciDevice;

fn detected_intel_from_device(dev: &PciDevice) -> Option<DetectedChipset> {
    if dev.vendor_id != INTEL_VID {
        return None;
    }

    find_chipset(dev.vendor_id, dev.device_id, Some(dev.revision_id)).map(|enable| {
        DetectedChipset {
            enable,
            domain: dev.address.segment(),
            bus: dev.address.bus(),
            device: dev.address.device(),
            function: dev.address.function(),
            revision_id: dev.revision_id,
        }
    })
}

fn detected_amd_from_device(dev: &PciDevice) -> Option<DetectedAmdChipset> {
    if dev.vendor_id != AMD_VID && dev.vendor_id != 0x1002 {
        return None;
    }

    find_amd_chipset_entry(dev.vendor_id, dev.device_id, dev.revision_id).map(|enable| {
        DetectedAmdChipset {
            enable,
            domain: dev.address.segment(),
            bus: dev.address.bus(),
            device: dev.address.device(),
            function: dev.address.function(),
            revision_id: dev.revision_id,
        }
    })
}

/// Finds a single Intel chipset in a caller-provided PCI device list.
pub fn find_intel_chipset_in_devices(
    devices: &[PciDevice],
) -> Result<Option<DetectedChipset>, InternalError> {
    find_intel_chipset_in_iter(devices.iter().copied())
}

/// Finds a single Intel chipset in a caller-provided PCI device iterator.
pub fn find_intel_chipset_in_iter<I>(devices: I) -> Result<Option<DetectedChipset>, InternalError>
where
    I: IntoIterator<Item = PciDevice>,
{
    let mut found = None;
    for dev in devices {
        let Some(chipset) = detected_intel_from_device(&dev) else {
            continue;
        };
        if found.is_some() {
            return Err(InternalError::MultipleChipsets);
        }
        found = Some(chipset);
    }

    if let Some(chipset) = &found
        && chipset.enable.status.is_bad()
    {
        return Err(InternalError::UnsupportedChipset {
            vendor_id: chipset.enable.vendor_id,
            device_id: chipset.enable.device_id,
            name: chipset.enable.device_name,
        });
    }
    Ok(found)
}

/// Finds a single AMD chipset in a caller-provided PCI device list.
pub fn find_amd_chipset_in_devices(
    devices: &[PciDevice],
) -> Result<Option<DetectedAmdChipset>, InternalError> {
    find_amd_chipset_in_iter(devices.iter().copied())
}

/// Finds the first AMD chipset in a caller-provided PCI device iterator.
pub fn find_amd_chipset_in_iter<I>(devices: I) -> Result<Option<DetectedAmdChipset>, InternalError>
where
    I: IntoIterator<Item = PciDevice>,
{
    let mut found: Option<DetectedAmdChipset> = None;
    for device in devices {
        let Some(chipset) = detected_amd_from_device(&device) else {
            continue;
        };
        if let Some(first) = &found {
            log::warn!(
                "Multiple AMD chipsets found; using {} {} at {:02x}:{:02x}.{:x} and ignoring {} {} at {:02x}:{:02x}.{:x}",
                first.vendor(),
                first.name(),
                first.bus,
                first.device,
                first.function,
                chipset.vendor(),
                chipset.name(),
                chipset.bus,
                chipset.device,
                chipset.function,
            );
        } else {
            found = Some(chipset);
        }
    }
    Ok(found)
}

/// Scans Linux sysfs for PCI devices.
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn scan_pci_bus() -> Result<alloc::vec::Vec<PciDevice>, InternalError> {
    rflasher_pci::SysfsPci::system()
        .enumerate()
        .map_err(InternalError::from)
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn scan_pci_bus() -> Result<alloc::vec::Vec<PciDevice>, InternalError> {
    Err(InternalError::NotSupported(
        "PCI scanning only supported on Linux",
    ))
}

/// Scan Linux PCI devices for supported Intel chipsets.
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn scan_for_intel_chipsets() -> Result<alloc::vec::Vec<DetectedChipset>, InternalError> {
    let devices = scan_pci_bus()?;
    Ok(devices
        .iter()
        .filter_map(detected_intel_from_device)
        .collect())
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn scan_for_intel_chipsets() -> Result<alloc::vec::Vec<DetectedChipset>, InternalError> {
    Err(InternalError::NotSupported(
        "PCI scanning only supported on Linux",
    ))
}

/// Finds one supported Intel chipset on Linux.
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn find_intel_chipset() -> Result<Option<DetectedChipset>, InternalError> {
    find_intel_chipset_in_iter(scan_pci_bus()?)
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn find_intel_chipset() -> Result<Option<DetectedChipset>, InternalError> {
    Err(InternalError::NotSupported(
        "PCI scanning only supported on Linux",
    ))
}

/// Information about a detected AMD chipset.
#[derive(Debug, Clone)]
pub struct DetectedAmdChipset {
    /// The chipset enable entry from the database.
    pub enable: &'static AmdChipsetEnable,
    /// PCI segment/domain.
    pub domain: u16,
    /// PCI bus number.
    pub bus: u8,
    /// PCI device number.
    pub device: u8,
    /// PCI function number.
    pub function: u8,
    /// PCI revision ID.
    pub revision_id: u8,
}

impl DetectedAmdChipset {
    pub fn name(&self) -> &'static str {
        self.enable.device_name
    }
    pub fn vendor(&self) -> &'static str {
        self.enable.vendor_name
    }
    pub fn status(&self) -> crate::chipset::TestStatus {
        self.enable.status
    }
    pub fn chipset_type(&self) -> crate::amd_pci::AmdChipset {
        self.enable.chipset
    }
    pub fn should_warn(&self) -> bool {
        self.enable.status.should_warn()
    }
    pub fn status_message(&self) -> Option<&'static str> {
        self.enable.status.message()
    }

    pub fn log_warnings(&self) {
        use crate::chipset::TestStatus;
        match self.enable.status {
            TestStatus::Untested => log::warn!(
                "Chipset {} {} ({:04x}:{:04x} rev {:02x}) is UNTESTED.",
                self.enable.vendor_name,
                self.enable.device_name,
                self.enable.vendor_id,
                self.enable.device_id,
                self.revision_id
            ),
            TestStatus::Depends => log::info!(
                "Support for {} {} depends on configuration (e.g., BIOS settings, flash descriptor).",
                self.enable.vendor_name,
                self.enable.device_name
            ),
            TestStatus::Bad => log::error!(
                "Chipset {} {} is NOT SUPPORTED.",
                self.enable.vendor_name,
                self.enable.device_name
            ),
            _ => {}
        }
    }
}

/// Scan Linux PCI devices for supported AMD chipsets.
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn scan_for_amd_chipsets() -> Result<alloc::vec::Vec<DetectedAmdChipset>, InternalError> {
    Ok(scan_pci_bus()?
        .iter()
        .filter_map(detected_amd_from_device)
        .collect())
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn scan_for_amd_chipsets() -> Result<alloc::vec::Vec<DetectedAmdChipset>, InternalError> {
    Err(InternalError::NotSupported(
        "PCI scanning only supported on Linux",
    ))
}

/// Finds the first supported AMD chipset on Linux.
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn find_amd_chipset() -> Result<Option<DetectedAmdChipset>, InternalError> {
    find_amd_chipset_in_iter(scan_pci_bus()?)
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn find_amd_chipset() -> Result<Option<DetectedAmdChipset>, InternalError> {
    Err(InternalError::NotSupported(
        "PCI scanning only supported on Linux",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rflasher_pci::PciAddress;

    fn pci_device(vendor_id: u16, device_id: u16, revision_id: u8) -> PciDevice {
        PciDevice {
            address: PciAddress::new(0, 0, 0x1f, 0),
            vendor_id,
            device_id,
            revision_id,
            class: 0,
        }
    }

    #[test]
    fn finds_an_intel_chipset() {
        let chipset = find_intel_chipset_in_devices(&[pci_device(INTEL_VID, 0x0f1c, 0)])
            .unwrap()
            .unwrap();
        assert_eq!(chipset.enable.vendor_id, INTEL_VID);
    }

    #[test]
    fn rejects_multiple_intel_chipsets() {
        let devices = [
            pci_device(INTEL_VID, 0x0f1c, 0),
            pci_device(INTEL_VID, 0x1c44, 0),
        ];
        assert!(matches!(
            find_intel_chipset_in_devices(&devices),
            Err(InternalError::MultipleChipsets)
        ));
    }

    #[test]
    fn finds_an_amd_chipset() {
        let chipset = find_amd_chipset_in_devices(&[pci_device(AMD_VID, 0x790b, 0x51)])
            .unwrap()
            .unwrap();
        assert_eq!(chipset.revision_id, 0x51);
    }

    #[test]
    fn multiple_amd_chipsets_keep_the_first_match() {
        let chipset = find_amd_chipset_in_devices(&[
            pci_device(AMD_VID, 0x790b, 0x51),
            pci_device(0x1002, 0x438d, 0),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(chipset.enable.device_id, 0x790b);
    }
}

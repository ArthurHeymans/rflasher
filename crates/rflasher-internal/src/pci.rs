//! PCI device scanning and access
//!
//! This module provides PCI device scanning functionality using the Linux
//! sysfs interface (/sys/bus/pci/devices).

#[cfg(all(feature = "std", target_os = "linux"))]
use std::fs;
#[cfg(all(feature = "std", target_os = "linux"))]
use std::path::Path;

use crate::error::{InternalError, PciAccessError};
use crate::intel_pci::{find_chipset, INTEL_VID};
use crate::DetectedChipset;

/// PCI device information
#[derive(Debug, Clone)]
pub struct PciDevice {
    /// PCI domain (usually 0)
    pub domain: u16,
    /// PCI bus number
    pub bus: u8,
    /// PCI device (slot) number
    pub device: u8,
    /// PCI function number
    pub function: u8,
    /// Vendor ID
    pub vendor_id: u16,
    /// Device ID
    pub device_id: u16,
    /// Revision ID
    pub revision_id: u8,
    /// Class code (3 bytes, upper 24 bits)
    pub class: u32,
}

impl PciDevice {
    /// Check if this device matches a vendor/device ID pair
    pub fn matches(&self, vendor_id: u16, device_id: u16) -> bool {
        self.vendor_id == vendor_id && self.device_id == device_id
    }

    /// Get the BDF (Bus:Device.Function) string
    pub fn bdf(&self) -> alloc::string::String {
        alloc::format!("{:02x}:{:02x}.{:x}", self.bus, self.device, self.function)
    }
}

extern crate alloc;

/// Scan the PCI bus for devices
///
/// This uses the Linux sysfs interface to enumerate PCI devices.
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn scan_pci_bus() -> Result<alloc::vec::Vec<PciDevice>, InternalError> {
    let pci_path = Path::new("/sys/bus/pci/devices");
    
    if !pci_path.exists() {
        return Err(InternalError::PciAccess(PciAccessError::Init));
    }
    
    let mut devices = alloc::vec::Vec::new();
    
    let entries = fs::read_dir(pci_path)
        .map_err(|_| InternalError::PciAccess(PciAccessError::Scan))?;
    
    for entry in entries {
        let entry = entry.map_err(|_| InternalError::PciAccess(PciAccessError::Scan))?;
        let path = entry.path();
        
        // Parse the device name (format: "0000:00:1f.0")
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        
        if let Some(dev) = parse_pci_device(&path, &name_str) {
            devices.push(dev);
        }
    }
    
    Ok(devices)
}

/// Parse a PCI device from sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
fn parse_pci_device(path: &Path, name: &str) -> Option<PciDevice> {
    // Parse domain:bus:device.function from name
    // Format: "0000:00:1f.0"
    let parts: alloc::vec::Vec<&str> = name.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    
    let domain = u16::from_str_radix(parts[0], 16).ok()?;
    let bus = u8::from_str_radix(parts[1], 16).ok()?;
    
    // Parse device.function
    let dev_func: alloc::vec::Vec<&str> = parts[2].split('.').collect();
    if dev_func.len() != 2 {
        return None;
    }
    
    let device = u8::from_str_radix(dev_func[0], 16).ok()?;
    let function = u8::from_str_radix(dev_func[1], 16).ok()?;
    
    // Read vendor ID
    let vendor_id = read_sysfs_hex_u16(&path.join("vendor"))?;
    
    // Read device ID
    let device_id = read_sysfs_hex_u16(&path.join("device"))?;
    
    // Read revision (optional, default to 0)
    let revision_id = read_sysfs_hex_u8(&path.join("revision")).unwrap_or(0);
    
    // Read class (optional, default to 0)
    let class = read_sysfs_hex_u32(&path.join("class")).unwrap_or(0);
    
    Some(PciDevice {
        domain,
        bus,
        device,
        function,
        vendor_id,
        device_id,
        revision_id,
        class,
    })
}

/// Read a hex u16 value from a sysfs file
#[cfg(all(feature = "std", target_os = "linux"))]
fn read_sysfs_hex_u16(path: &Path) -> Option<u16> {
    let content = fs::read_to_string(path).ok()?;
    let content = content.trim();
    // Handle "0x" prefix
    let hex_str = content.strip_prefix("0x").unwrap_or(content);
    u16::from_str_radix(hex_str, 16).ok()
}

/// Read a hex u8 value from a sysfs file
#[cfg(all(feature = "std", target_os = "linux"))]
fn read_sysfs_hex_u8(path: &Path) -> Option<u8> {
    let content = fs::read_to_string(path).ok()?;
    let content = content.trim();
    let hex_str = content.strip_prefix("0x").unwrap_or(content);
    u8::from_str_radix(hex_str, 16).ok()
}

/// Read a hex u32 value from a sysfs file
#[cfg(all(feature = "std", target_os = "linux"))]
fn read_sysfs_hex_u32(path: &Path) -> Option<u32> {
    let content = fs::read_to_string(path).ok()?;
    let content = content.trim();
    let hex_str = content.strip_prefix("0x").unwrap_or(content);
    u32::from_str_radix(hex_str, 16).ok()
}

/// Scan for Intel chipsets and return any matches
///
/// This function scans the PCI bus and looks for known Intel chipsets
/// in the chipset enable table.
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn scan_for_intel_chipsets() -> Result<alloc::vec::Vec<DetectedChipset>, InternalError> {
    let devices = scan_pci_bus()?;
    let mut found = alloc::vec::Vec::new();
    
    for dev in devices {
        // Only check Intel devices
        if dev.vendor_id != INTEL_VID {
            continue;
        }
        
        // Look up in our chipset table
        if let Some(enable) = find_chipset(dev.vendor_id, dev.device_id, Some(dev.revision_id)) {
            log::debug!(
                "Found chipset {} {} at {:02x}:{:02x}.{:x}",
                enable.vendor_name,
                enable.device_name,
                dev.bus,
                dev.device,
                dev.function
            );
            
            found.push(DetectedChipset {
                enable,
                bus: dev.bus,
                device: dev.device,
                function: dev.function,
                revision_id: dev.revision_id,
            });
        }
    }
    
    Ok(found)
}

/// Find a single Intel chipset, warning about duplicates
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn find_intel_chipset() -> Result<Option<DetectedChipset>, InternalError> {
    let chipsets = scan_for_intel_chipsets()?;
    
    match chipsets.len() {
        0 => Ok(None),
        1 => {
            let chipset = chipsets.into_iter().next().unwrap();
            
            // Log info about the found chipset
            log::info!(
                "Found chipset \"{}\" {} with PCI ID {:04x}:{:04x}",
                chipset.enable.vendor_name,
                chipset.enable.device_name,
                chipset.enable.vendor_id,
                chipset.enable.device_id
            );
            
            // Log any warnings
            chipset.log_warnings();
            
            // Check if chipset is known bad
            if chipset.enable.status.is_bad() {
                return Err(InternalError::UnsupportedChipset {
                    vendor_id: chipset.enable.vendor_id,
                    device_id: chipset.enable.device_id,
                    name: chipset.enable.device_name,
                });
            }
            
            Ok(Some(chipset))
        }
        _ => {
            // Multiple chipsets found - warn and return error
            log::warn!("Multiple supported chipsets found:");
            for chipset in &chipsets {
                log::warn!(
                    "  {} {} at {:02x}:{:02x}.{:x}",
                    chipset.enable.vendor_name,
                    chipset.enable.device_name,
                    chipset.bus,
                    chipset.device,
                    chipset.function
                );
            }
            // For now, return an error - in the future we could allow selecting one
            // via programmer options
            Err(InternalError::MultipleChipsets)
        }
    }
}

// ============================================================================
// PCI Configuration Space Access
// ============================================================================

/// Read a byte from PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_read_config8(bus: u8, device: u8, function: u8, offset: u8) -> Result<u8, InternalError> {
    use std::io::Read;
    
    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );
    
    let mut file = std::fs::File::open(&path)
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigRead {
            bus, device, function, register: offset
        }))?;
    
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigRead {
            bus, device, function, register: offset
        }))?;
    
    let mut buf = [0u8; 1];
    file.read_exact(&mut buf)
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigRead {
            bus, device, function, register: offset
        }))?;
    
    Ok(buf[0])
}

/// Read a word from PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_read_config16(bus: u8, device: u8, function: u8, offset: u8) -> Result<u16, InternalError> {
    use std::io::Read;
    
    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );
    
    let mut file = std::fs::File::open(&path)
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigRead {
            bus, device, function, register: offset
        }))?;
    
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigRead {
            bus, device, function, register: offset
        }))?;
    
    let mut buf = [0u8; 2];
    file.read_exact(&mut buf)
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigRead {
            bus, device, function, register: offset
        }))?;
    
    Ok(u16::from_le_bytes(buf))
}

/// Read a dword from PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_read_config32(bus: u8, device: u8, function: u8, offset: u8) -> Result<u32, InternalError> {
    use std::io::Read;
    
    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );
    
    let mut file = std::fs::File::open(&path)
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigRead {
            bus, device, function, register: offset
        }))?;
    
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigRead {
            bus, device, function, register: offset
        }))?;
    
    let mut buf = [0u8; 4];
    file.read_exact(&mut buf)
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigRead {
            bus, device, function, register: offset
        }))?;
    
    Ok(u32::from_le_bytes(buf))
}

/// Write a byte to PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_write_config8(bus: u8, device: u8, function: u8, offset: u8, value: u8) -> Result<(), InternalError> {
    use std::io::Write;
    use std::fs::OpenOptions;
    
    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );
    
    let mut file = OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus, device, function, register: offset
        }))?;
    
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus, device, function, register: offset
        }))?;
    
    file.write_all(&[value])
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus, device, function, register: offset
        }))?;
    
    Ok(())
}

/// Write a word to PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_write_config16(bus: u8, device: u8, function: u8, offset: u8, value: u16) -> Result<(), InternalError> {
    use std::io::Write;
    use std::fs::OpenOptions;
    
    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );
    
    let mut file = OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus, device, function, register: offset
        }))?;
    
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus, device, function, register: offset
        }))?;
    
    file.write_all(&value.to_le_bytes())
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus, device, function, register: offset
        }))?;
    
    Ok(())
}

/// Write a dword to PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_write_config32(bus: u8, device: u8, function: u8, offset: u8, value: u32) -> Result<(), InternalError> {
    use std::io::Write;
    use std::fs::OpenOptions;
    
    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );
    
    let mut file = OpenOptions::new()
        .write(true)
        .open(&path)
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus, device, function, register: offset
        }))?;
    
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus, device, function, register: offset
        }))?;
    
    file.write_all(&value.to_le_bytes())
        .map_err(|_| InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus, device, function, register: offset
        }))?;
    
    Ok(())
}

// Non-Linux stubs
#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_read_config8(_bus: u8, _device: u8, _function: u8, _offset: u8) -> Result<u8, InternalError> {
    Err(InternalError::NotSupported("PCI config access only supported on Linux"))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_read_config16(_bus: u8, _device: u8, _function: u8, _offset: u8) -> Result<u16, InternalError> {
    Err(InternalError::NotSupported("PCI config access only supported on Linux"))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_read_config32(_bus: u8, _device: u8, _function: u8, _offset: u8) -> Result<u32, InternalError> {
    Err(InternalError::NotSupported("PCI config access only supported on Linux"))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_write_config8(_bus: u8, _device: u8, _function: u8, _offset: u8, _value: u8) -> Result<(), InternalError> {
    Err(InternalError::NotSupported("PCI config access only supported on Linux"))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_write_config16(_bus: u8, _device: u8, _function: u8, _offset: u8, _value: u16) -> Result<(), InternalError> {
    Err(InternalError::NotSupported("PCI config access only supported on Linux"))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_write_config32(_bus: u8, _device: u8, _function: u8, _offset: u8, _value: u32) -> Result<(), InternalError> {
    Err(InternalError::NotSupported("PCI config access only supported on Linux"))
}

// Non-Linux stubs for scan functions
#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn scan_pci_bus() -> Result<alloc::vec::Vec<PciDevice>, InternalError> {
    Err(InternalError::NotSupported("PCI scanning only supported on Linux"))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn scan_for_intel_chipsets() -> Result<alloc::vec::Vec<DetectedChipset>, InternalError> {
    Err(InternalError::NotSupported("PCI scanning only supported on Linux"))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn find_intel_chipset() -> Result<Option<DetectedChipset>, InternalError> {
    Err(InternalError::NotSupported("PCI scanning only supported on Linux"))
}

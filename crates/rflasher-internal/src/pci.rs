//! PCI device scanning and access
//!
//! This module provides PCI device scanning functionality using the Linux
//! sysfs interface (/sys/bus/pci/devices).

#![cfg_attr(
    not(all(feature = "std", target_os = "linux")),
    allow(dead_code, unused_imports)
)]

#[cfg(all(feature = "std", target_os = "linux"))]
use std::fs;
#[cfg(all(feature = "std", target_os = "linux"))]
use std::path::Path;

use crate::DetectedChipset;
use crate::amd_pci::{AMD_VID, AmdChipsetEnable, find_chipset as find_amd_chipset_entry};
use crate::error::{InternalError, PciAccessError};
use crate::intel_pci::{INTEL_VID, find_chipset};

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

fn detected_intel_from_device(dev: &PciDevice) -> Option<DetectedChipset> {
    if dev.vendor_id != INTEL_VID {
        return None;
    }

    find_chipset(dev.vendor_id, dev.device_id, Some(dev.revision_id)).map(|enable| {
        DetectedChipset {
            enable,
            domain: dev.domain,
            bus: dev.bus,
            device: dev.device,
            function: dev.function,
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
            domain: dev.domain,
            bus: dev.bus,
            device: dev.device,
            function: dev.function,
            revision_id: dev.revision_id,
        }
    })
}

/// Finds a single Intel chipset in a caller-provided PCI device list.
///
/// This is the embedded-friendly detection path: firmware can provide devices
/// from its own PCI scanner and avoid Linux sysfs enumeration entirely.
pub fn find_intel_chipset_in_devices(
    devices: &[PciDevice],
) -> Result<Option<DetectedChipset>, InternalError> {
    find_intel_chipset_in_iter(devices.iter().cloned())
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
    find_amd_chipset_in_iter(devices.iter().cloned())
}

/// Finds a single AMD chipset in a caller-provided PCI device iterator.
///
/// This preserves the existing Linux behavior for multiple AMD matches by
/// returning the first match instead of failing.
pub fn find_amd_chipset_in_iter<I>(devices: I) -> Result<Option<DetectedAmdChipset>, InternalError>
where
    I: IntoIterator<Item = PciDevice>,
{
    Ok(devices
        .into_iter()
        .find_map(|dev| detected_amd_from_device(&dev)))
}

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

    let entries =
        fs::read_dir(pci_path).map_err(|_| InternalError::PciAccess(PciAccessError::Scan))?;

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
                domain: dev.domain,
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

// PCI Configuration Mechanism 1 I/O ports
const PCI_CONFIG_ADDR: u16 = 0xCF8;
const PCI_CONFIG_DATA: u16 = 0xCFC;

/// Build a PCI Configuration Mechanism 1 address
#[inline]
fn pci_cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

/// Read a dword from PCI config space using direct I/O port access (Mechanism 1)
///
/// This works even for hidden PCI devices that don't appear in sysfs.
/// Requires root/CAP_SYS_RAWIO and x86 architecture.
#[cfg(all(
    feature = "std",
    target_os = "linux",
    any(target_arch = "x86", target_arch = "x86_64")
))]
pub fn pci_read_config32_direct(
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
) -> Result<u32, InternalError> {
    // Request I/O port access permission for PCI config ports (0xCF8-0xCFF)
    // This requires CAP_SYS_RAWIO (usually root)
    let ret = unsafe { libc::iopl(3) };
    if ret != 0 {
        log::debug!(
            "iopl(3) failed with error: {}",
            std::io::Error::last_os_error()
        );
        return Err(InternalError::PciAccess(PciAccessError::ConfigRead {
            bus,
            device,
            function,
            register: offset as u16,
        }));
    }

    // Build the PCI config address
    let addr = pci_cfg_addr(bus, device, function, offset);

    // Use inline assembly for proper 32-bit I/O port access
    let data: u32;
    unsafe {
        // Write address to CONFIG_ADDRESS port (0xCF8)
        std::arch::asm!(
            "out dx, eax",
            in("dx") PCI_CONFIG_ADDR,
            in("eax") addr,
            options(nomem, nostack, preserves_flags)
        );

        // Read data from CONFIG_DATA port (0xCFC)
        std::arch::asm!(
            "in eax, dx",
            in("dx") PCI_CONFIG_DATA,
            out("eax") data,
            options(nomem, nostack, preserves_flags)
        );
    }

    Ok(data)
}

#[cfg(not(all(
    feature = "std",
    target_os = "linux",
    any(target_arch = "x86", target_arch = "x86_64")
)))]
pub fn pci_read_config32_direct(
    _bus: u8,
    _device: u8,
    _function: u8,
    _offset: u8,
) -> Result<u32, InternalError> {
    Err(InternalError::NotSupported(
        "Direct PCI access only supported on x86 Linux",
    ))
}

/// Read a byte from PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_read_config8(
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
) -> Result<u8, InternalError> {
    use std::io::Read;

    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );

    let mut file = std::fs::File::open(&path).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigRead {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| {
            InternalError::PciAccess(PciAccessError::ConfigRead {
                bus,
                device,
                function,
                register: offset as u16,
            })
        })?;

    let mut buf = [0u8; 1];
    file.read_exact(&mut buf).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigRead {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    Ok(buf[0])
}

/// Read a word from PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_read_config16(
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
) -> Result<u16, InternalError> {
    use std::io::Read;

    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );

    let mut file = std::fs::File::open(&path).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigRead {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| {
            InternalError::PciAccess(PciAccessError::ConfigRead {
                bus,
                device,
                function,
                register: offset as u16,
            })
        })?;

    let mut buf = [0u8; 2];
    file.read_exact(&mut buf).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigRead {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    Ok(u16::from_le_bytes(buf))
}

/// Read a dword from PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_read_config32(
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
) -> Result<u32, InternalError> {
    use std::io::Read;

    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );

    let mut file = std::fs::File::open(&path).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigRead {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| {
            InternalError::PciAccess(PciAccessError::ConfigRead {
                bus,
                device,
                function,
                register: offset as u16,
            })
        })?;

    let mut buf = [0u8; 4];
    file.read_exact(&mut buf).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigRead {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    Ok(u32::from_le_bytes(buf))
}

/// Write a byte to PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_write_config8(
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
    value: u8,
) -> Result<(), InternalError> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );

    let mut file = OpenOptions::new().write(true).open(&path).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| {
            InternalError::PciAccess(PciAccessError::ConfigWrite {
                bus,
                device,
                function,
                register: offset as u16,
            })
        })?;

    file.write_all(&[value]).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    Ok(())
}

/// Write a word to PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_write_config16(
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
    value: u16,
) -> Result<(), InternalError> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );

    let mut file = OpenOptions::new().write(true).open(&path).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| {
            InternalError::PciAccess(PciAccessError::ConfigWrite {
                bus,
                device,
                function,
                register: offset as u16,
            })
        })?;

    file.write_all(&value.to_le_bytes()).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    Ok(())
}

/// Write a dword to PCI configuration space via sysfs
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn pci_write_config32(
    bus: u8,
    device: u8,
    function: u8,
    offset: u8,
    value: u32,
) -> Result<(), InternalError> {
    use std::fs::OpenOptions;
    use std::io::Write;

    let path = format!(
        "/sys/bus/pci/devices/0000:{:02x}:{:02x}.{:x}/config",
        bus, device, function
    );

    let mut file = OpenOptions::new().write(true).open(&path).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset as u64))
        .map_err(|_| {
            InternalError::PciAccess(PciAccessError::ConfigWrite {
                bus,
                device,
                function,
                register: offset as u16,
            })
        })?;

    file.write_all(&value.to_le_bytes()).map_err(|_| {
        InternalError::PciAccess(PciAccessError::ConfigWrite {
            bus,
            device,
            function,
            register: offset as u16,
        })
    })?;

    Ok(())
}

// Non-Linux stubs
#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_read_config8(
    _bus: u8,
    _device: u8,
    _function: u8,
    _offset: u8,
) -> Result<u8, InternalError> {
    Err(InternalError::NotSupported(
        "PCI config access only supported on Linux",
    ))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_read_config16(
    _bus: u8,
    _device: u8,
    _function: u8,
    _offset: u8,
) -> Result<u16, InternalError> {
    Err(InternalError::NotSupported(
        "PCI config access only supported on Linux",
    ))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_read_config32(
    _bus: u8,
    _device: u8,
    _function: u8,
    _offset: u8,
) -> Result<u32, InternalError> {
    Err(InternalError::NotSupported(
        "PCI config access only supported on Linux",
    ))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_write_config8(
    _bus: u8,
    _device: u8,
    _function: u8,
    _offset: u8,
    _value: u8,
) -> Result<(), InternalError> {
    Err(InternalError::NotSupported(
        "PCI config access only supported on Linux",
    ))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_write_config16(
    _bus: u8,
    _device: u8,
    _function: u8,
    _offset: u8,
    _value: u16,
) -> Result<(), InternalError> {
    Err(InternalError::NotSupported(
        "PCI config access only supported on Linux",
    ))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn pci_write_config32(
    _bus: u8,
    _device: u8,
    _function: u8,
    _offset: u8,
    _value: u32,
) -> Result<(), InternalError> {
    Err(InternalError::NotSupported(
        "PCI config access only supported on Linux",
    ))
}

// Non-Linux stubs for scan functions
#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn scan_pci_bus() -> Result<alloc::vec::Vec<PciDevice>, InternalError> {
    Err(InternalError::NotSupported(
        "PCI scanning only supported on Linux",
    ))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn scan_for_intel_chipsets() -> Result<alloc::vec::Vec<DetectedChipset>, InternalError> {
    Err(InternalError::NotSupported(
        "PCI scanning only supported on Linux",
    ))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn find_intel_chipset() -> Result<Option<DetectedChipset>, InternalError> {
    Err(InternalError::NotSupported(
        "PCI scanning only supported on Linux",
    ))
}

// =============================================================================
// AMD Chipset Detection
// =============================================================================

/// Information about a detected AMD chipset
#[derive(Debug, Clone)]
pub struct DetectedAmdChipset {
    /// The chipset enable entry from the database
    pub enable: &'static AmdChipsetEnable,
    /// PCI segment/domain
    pub domain: u16,
    /// PCI bus number
    pub bus: u8,
    /// PCI device number
    pub device: u8,
    /// PCI function number
    pub function: u8,
    /// PCI revision ID
    pub revision_id: u8,
}

impl DetectedAmdChipset {
    /// Returns the chipset name
    pub fn name(&self) -> &'static str {
        self.enable.device_name
    }

    /// Returns the vendor name
    pub fn vendor(&self) -> &'static str {
        self.enable.vendor_name
    }

    /// Returns the test status
    pub fn status(&self) -> crate::chipset::TestStatus {
        self.enable.status
    }

    /// Returns the chipset type
    pub fn chipset_type(&self) -> crate::amd_pci::AmdChipset {
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
        use crate::chipset::TestStatus;
        match self.enable.status {
            TestStatus::Untested => {
                log::warn!(
                    "Chipset {} {} ({:04x}:{:04x} rev {:02x}) is UNTESTED.",
                    self.enable.vendor_name,
                    self.enable.device_name,
                    self.enable.vendor_id,
                    self.enable.device_id,
                    self.revision_id
                );
                log::warn!(
                    "If you are using an up-to-date version and were (not) able to \
                     successfully access flash with it, please report your results."
                );
            }
            TestStatus::Depends => {
                log::info!(
                    "Support for {} {} depends on configuration \
                     (e.g., BIOS settings, flash descriptor).",
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

/// Scan for AMD chipsets and return any matches
///
/// This function scans the PCI bus and looks for known AMD chipsets
/// in the chipset enable table.
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn scan_for_amd_chipsets() -> Result<alloc::vec::Vec<DetectedAmdChipset>, InternalError> {
    let devices = scan_pci_bus()?;
    let mut found = alloc::vec::Vec::new();

    for dev in devices {
        // Only check AMD devices (0x1022 and old ATI 0x1002)
        if dev.vendor_id != AMD_VID && dev.vendor_id != 0x1002 {
            continue;
        }

        // Look up in our chipset table
        if let Some(enable) = find_amd_chipset_entry(dev.vendor_id, dev.device_id, dev.revision_id)
        {
            log::debug!(
                "Found AMD chipset {} {} (rev {:02x}) at {:02x}:{:02x}.{:x}",
                enable.vendor_name,
                enable.device_name,
                dev.revision_id,
                dev.bus,
                dev.device,
                dev.function
            );

            found.push(DetectedAmdChipset {
                enable,
                domain: dev.domain,
                bus: dev.bus,
                device: dev.device,
                function: dev.function,
                revision_id: dev.revision_id,
            });
        }
    }

    Ok(found)
}

/// Find a single AMD chipset, warning about duplicates
///
/// This function scans for AMD chipsets and returns the first one found,
/// logging a warning if multiple chipsets are detected.
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn find_amd_chipset() -> Result<Option<DetectedAmdChipset>, InternalError> {
    let chipsets = scan_for_amd_chipsets()?;

    match chipsets.len() {
        0 => Ok(None),
        1 => Ok(Some(chipsets[0].clone())),
        _ => {
            log::warn!("Multiple AMD chipsets found:");
            for cs in &chipsets {
                log::warn!(
                    "  {} {} at {:02x}:{:02x}.{:x}",
                    cs.vendor(),
                    cs.name(),
                    cs.bus,
                    cs.device,
                    cs.function
                );
            }
            log::warn!(
                "Using first chipset: {} {}",
                chipsets[0].vendor(),
                chipsets[0].name()
            );
            Ok(Some(chipsets[0].clone()))
        }
    }
}

// Non-Linux stubs for AMD functions
#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn scan_for_amd_chipsets() -> Result<alloc::vec::Vec<DetectedAmdChipset>, InternalError> {
    Err(InternalError::NotSupported(
        "PCI scanning only supported on Linux",
    ))
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

    fn pci_device(vendor_id: u16, device_id: u16, revision_id: u8) -> PciDevice {
        PciDevice {
            domain: 0,
            bus: 0,
            device: 0x1f,
            function: 0,
            vendor_id,
            device_id,
            revision_id,
            class: 0,
        }
    }

    #[test]
    fn test_find_intel_chipset_in_devices_no_match() {
        let devices = [pci_device(0x1234, 0x5678, 0)];
        assert!(find_intel_chipset_in_devices(&devices).unwrap().is_none());
    }

    #[test]
    fn test_find_intel_chipset_in_devices_detects_match() {
        let devices = [pci_device(INTEL_VID, 0x0f1c, 0)];
        let chipset = find_intel_chipset_in_devices(&devices).unwrap().unwrap();
        assert_eq!(chipset.enable.vendor_id, INTEL_VID);
        assert_eq!(chipset.enable.device_id, 0x0f1c);
        assert_eq!(chipset.bus, 0);
        assert_eq!(chipset.device, 0x1f);
    }

    #[test]
    fn test_find_intel_chipset_in_devices_rejects_multiple_matches() {
        let devices = [
            pci_device(INTEL_VID, 0x0f1c, 0),
            pci_device(INTEL_VID, 0x1c44, 0),
        ];
        let err = find_intel_chipset_in_devices(&devices).unwrap_err();
        assert!(matches!(err, InternalError::MultipleChipsets));
    }

    #[test]
    fn test_find_amd_chipset_in_devices_detects_revision_match() {
        let devices = [pci_device(AMD_VID, 0x790b, 0x51)];
        let chipset = find_amd_chipset_in_devices(&devices).unwrap().unwrap();
        assert_eq!(chipset.enable.vendor_id, AMD_VID);
        assert_eq!(chipset.enable.device_id, 0x790b);
        assert_eq!(chipset.revision_id, 0x51);
    }
}

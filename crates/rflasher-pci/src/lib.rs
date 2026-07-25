//! Pure-Rust PCI configuration-space types and access backends.
//!
//! This crate is `no_std` by default.  It re-exports [`pci_types`] for PCI
//! headers and capabilities, while [`PciConfigAccess`] adds the fallible
//! configuration-space access required by userspace backends.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

pub use pci_types;
pub use pci_types::{
    BaseClass, DeviceId, DeviceRevision, Interface, PciAddress, SubClass, VendorId,
};

/// A PCI function discovered by a platform-specific enumerator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciDevice {
    /// Full PCI segment:bus:device.function address.
    pub address: PciAddress,
    /// PCI vendor ID.
    pub vendor_id: VendorId,
    /// PCI device ID.
    pub device_id: DeviceId,
    /// PCI revision ID.
    pub revision_id: DeviceRevision,
    /// PCI class code, stored in the low 24 bits.
    pub class: u32,
}

impl PciDevice {
    /// Returns whether this function has the supplied vendor and device IDs.
    pub const fn matches(&self, vendor_id: VendorId, device_id: DeviceId) -> bool {
        self.vendor_id == vendor_id && self.device_id == device_id
    }

    /// Returns the PCI base class code.
    pub const fn base_class(&self) -> BaseClass {
        ((self.class >> 16) & 0xff) as BaseClass
    }

    /// Returns the PCI subclass code.
    pub const fn sub_class(&self) -> SubClass {
        ((self.class >> 8) & 0xff) as SubClass
    }

    /// Returns the PCI programming interface code.
    pub const fn interface(&self) -> Interface {
        (self.class & 0xff) as Interface
    }
}

/// Error produced by a PCI access backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PciError {
    /// The platform does not provide this PCI access mechanism.
    NotSupported(&'static str),
    /// A PCI bus scan could not be started or completed.
    Scan,
    /// A PCI configuration-space read failed.
    ConfigRead { address: PciAddress, offset: u16 },
    /// A PCI configuration-space write failed.
    ConfigWrite { address: PciAddress, offset: u16 },
    /// The address, register offset, or access width is invalid for the backend.
    InvalidAccess { address: PciAddress, offset: u16 },
}

impl core::fmt::Display for PciError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotSupported(message) => write!(f, "not supported: {message}"),
            Self::Scan => write!(f, "failed to scan PCI bus"),
            Self::ConfigRead { address, offset } => {
                write!(f, "failed to read PCI config at {address} reg {offset:#x}")
            }
            Self::ConfigWrite { address, offset } => {
                write!(f, "failed to write PCI config at {address} reg {offset:#x}")
            }
            Self::InvalidAccess { address, offset } => {
                write!(f, "invalid PCI config access at {address} reg {offset:#x}")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PciError {}

/// PCI Express Enhanced Configuration Access Mechanism (ECAM) backend.
///
/// The caller supplies a mapped ECAM region for one PCI segment.  This type
/// does not allocate and can therefore be used from firmware after the MMIO
/// mapping has been established.
#[derive(Clone, Copy, Debug)]
pub struct Ecam {
    base: *mut u8,
    segment: u16,
    bus_start: u8,
    bus_end: u8,
}

impl Ecam {
    /// Creates an ECAM backend.
    ///
    /// # Safety
    ///
    /// `base` must point to a valid, device-memory ECAM mapping for every bus
    /// in `bus_start..=bus_end`. The mapping must remain valid for the
    /// backend's lifetime, and callers must arrange any platform-specific
    /// synchronization required for configuration writes.
    pub const unsafe fn new(base: *mut u8, segment: u16, bus_start: u8, bus_end: u8) -> Self {
        Self {
            base,
            segment,
            bus_start,
            bus_end,
        }
    }

    fn register_address(
        &self,
        address: PciAddress,
        offset: u16,
        width: usize,
    ) -> Result<usize, PciError> {
        if address.segment() != self.segment
            || address.bus() < self.bus_start
            || address.bus() > self.bus_end
            || offset as usize + width > 4096
            || !(offset as usize).is_multiple_of(width)
        {
            return Err(PciError::InvalidAccess { address, offset });
        }

        Ok(self.base as usize
            + ((address.bus() - self.bus_start) as usize * 1024 * 1024)
            + (address.device() as usize * 32 * 1024)
            + (address.function() as usize * 4 * 1024)
            + offset as usize)
    }
}

impl PciConfigAccess for Ecam {
    type Error = PciError;

    fn read8(&self, address: PciAddress, offset: u16) -> Result<u8, Self::Error> {
        let ptr = self.register_address(address, offset, 1)? as *const u8;
        Ok(unsafe { core::ptr::read_volatile(ptr) })
    }

    fn read16(&self, address: PciAddress, offset: u16) -> Result<u16, Self::Error> {
        let ptr = self.register_address(address, offset, 2)? as *const u16;
        Ok(u16::from_le(unsafe { core::ptr::read_volatile(ptr) }))
    }

    fn read32(&self, address: PciAddress, offset: u16) -> Result<u32, Self::Error> {
        let ptr = self.register_address(address, offset, 4)? as *const u32;
        Ok(u32::from_le(unsafe { core::ptr::read_volatile(ptr) }))
    }

    fn write8(&self, address: PciAddress, offset: u16, value: u8) -> Result<(), Self::Error> {
        let ptr = self.register_address(address, offset, 1)? as *mut u8;
        unsafe { core::ptr::write_volatile(ptr, value) };
        Ok(())
    }

    fn write16(&self, address: PciAddress, offset: u16, value: u16) -> Result<(), Self::Error> {
        let ptr = self.register_address(address, offset, 2)? as *mut u16;
        unsafe { core::ptr::write_volatile(ptr, value.to_le()) };
        Ok(())
    }

    fn write32(&self, address: PciAddress, offset: u16, value: u32) -> Result<(), Self::Error> {
        let ptr = self.register_address(address, offset, 4)? as *mut u32;
        unsafe { core::ptr::write_volatile(ptr, value.to_le()) };
        Ok(())
    }
}

impl pci_types::ConfigRegionAccess for Ecam {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        self.read32(address, offset)
            .expect("invalid ECAM read through pci_types")
    }

    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        self.write32(address, offset, value)
            .expect("invalid ECAM write through pci_types");
    }
}

/// Fallible PCI configuration-space access.
///
/// The lower-level [`pci_types::ConfigRegionAccess`] trait is intentionally
/// infallible and unsafe, which is appropriate for firmware ECAM mappings.
/// This trait is for callers such as Linux userspace where configuration
/// accesses can fail normally.
pub trait PciConfigAccess {
    /// Backend-specific error type.
    type Error;

    /// Reads an 8-bit configuration register.
    fn read8(&self, address: PciAddress, offset: u16) -> Result<u8, Self::Error>;
    /// Reads a 16-bit configuration register.
    fn read16(&self, address: PciAddress, offset: u16) -> Result<u16, Self::Error>;
    /// Reads a 32-bit configuration register.
    fn read32(&self, address: PciAddress, offset: u16) -> Result<u32, Self::Error>;
    /// Writes an 8-bit configuration register.
    fn write8(&self, address: PciAddress, offset: u16, value: u8) -> Result<(), Self::Error>;
    /// Writes a 16-bit configuration register.
    fn write16(&self, address: PciAddress, offset: u16, value: u16) -> Result<(), Self::Error>;
    /// Writes a 32-bit configuration register.
    fn write32(&self, address: PciAddress, offset: u16, value: u32) -> Result<(), Self::Error>;
}

#[cfg(all(feature = "std", target_os = "linux"))]
mod linux {
    use super::{PciAddress, PciConfigAccess, PciDevice, PciError};
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Seek, Write};
    use std::path::{Path, PathBuf};

    /// Linux sysfs PCI backend.
    #[derive(Clone, Debug)]
    pub struct SysfsPci {
        root: PathBuf,
    }

    impl SysfsPci {
        /// Creates a backend for the system PCI sysfs tree.
        pub fn system() -> Self {
            Self::new("/sys/bus/pci/devices")
        }

        /// Creates a backend rooted at `root`.
        ///
        /// This is primarily useful for tests with a fixture sysfs tree.
        pub fn new(root: impl Into<PathBuf>) -> Self {
            Self { root: root.into() }
        }

        fn device_path(&self, address: PciAddress) -> PathBuf {
            self.root.join(format!(
                "{:04x}:{:02x}:{:02x}.{:x}",
                address.segment(),
                address.bus(),
                address.device(),
                address.function()
            ))
        }

        fn config_path(&self, address: PciAddress) -> PathBuf {
            self.device_path(address).join("config")
        }

        /// Enumerates PCI functions exposed by Linux sysfs.
        pub fn enumerate(&self) -> Result<Vec<PciDevice>, PciError> {
            let entries = fs::read_dir(&self.root).map_err(|_| PciError::Scan)?;
            let mut devices = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|_| PciError::Scan)?;
                if let Some(device) =
                    parse_device(&entry.path(), &entry.file_name().to_string_lossy())
                {
                    devices.push(device);
                }
            }
            Ok(devices)
        }

        fn read<const N: usize>(
            &self,
            address: PciAddress,
            offset: u16,
        ) -> Result<[u8; N], PciError> {
            let mut file = std::fs::File::open(self.config_path(address))
                .map_err(|_| PciError::ConfigRead { address, offset })?;
            file.seek(std::io::SeekFrom::Start(offset.into()))
                .map_err(|_| PciError::ConfigRead { address, offset })?;
            let mut bytes = [0; N];
            file.read_exact(&mut bytes)
                .map_err(|_| PciError::ConfigRead { address, offset })?;
            Ok(bytes)
        }

        fn write(&self, address: PciAddress, offset: u16, bytes: &[u8]) -> Result<(), PciError> {
            let mut file = OpenOptions::new()
                .write(true)
                .open(self.config_path(address))
                .map_err(|_| PciError::ConfigWrite { address, offset })?;
            file.seek(std::io::SeekFrom::Start(offset.into()))
                .map_err(|_| PciError::ConfigWrite { address, offset })?;
            file.write_all(bytes)
                .map_err(|_| PciError::ConfigWrite { address, offset })
        }
    }

    impl Default for SysfsPci {
        fn default() -> Self {
            Self::system()
        }
    }

    impl PciConfigAccess for SysfsPci {
        type Error = PciError;

        fn read8(&self, address: PciAddress, offset: u16) -> Result<u8, Self::Error> {
            Ok(self.read::<1>(address, offset)?[0])
        }
        fn read16(&self, address: PciAddress, offset: u16) -> Result<u16, Self::Error> {
            Ok(u16::from_le_bytes(self.read::<2>(address, offset)?))
        }
        fn read32(&self, address: PciAddress, offset: u16) -> Result<u32, Self::Error> {
            Ok(u32::from_le_bytes(self.read::<4>(address, offset)?))
        }
        fn write8(&self, address: PciAddress, offset: u16, value: u8) -> Result<(), Self::Error> {
            self.write(address, offset, &[value])
        }
        fn write16(&self, address: PciAddress, offset: u16, value: u16) -> Result<(), Self::Error> {
            self.write(address, offset, &value.to_le_bytes())
        }
        fn write32(&self, address: PciAddress, offset: u16, value: u32) -> Result<(), Self::Error> {
            self.write(address, offset, &value.to_le_bytes())
        }
    }

    fn read_hex(path: &Path) -> Option<String> {
        Some(
            fs::read_to_string(path)
                .ok()?
                .trim()
                .trim_start_matches("0x")
                .to_owned(),
        )
    }

    fn read_hex_u8(path: &Path) -> Option<u8> {
        u8::from_str_radix(&read_hex(path)?, 16).ok()
    }

    fn read_hex_u16(path: &Path) -> Option<u16> {
        u16::from_str_radix(&read_hex(path)?, 16).ok()
    }

    fn read_hex_u32(path: &Path) -> Option<u32> {
        u32::from_str_radix(&read_hex(path)?, 16).ok()
    }

    fn parse_device(path: &Path, name: &str) -> Option<PciDevice> {
        let (segment, rest) = name.split_once(':')?;
        let (bus, rest) = rest.split_once(':')?;
        let (device, function) = rest.split_once('.')?;
        let address = PciAddress::new(
            u16::from_str_radix(segment, 16).ok()?,
            u8::from_str_radix(bus, 16).ok()?,
            u8::from_str_radix(device, 16).ok()?,
            u8::from_str_radix(function, 16).ok()?,
        );
        Some(PciDevice {
            address,
            vendor_id: read_hex_u16(&path.join("vendor"))?,
            device_id: read_hex_u16(&path.join("device"))?,
            revision_id: read_hex_u8(&path.join("revision")).unwrap_or(0),
            class: read_hex_u32(&path.join("class")).unwrap_or(0),
        })
    }

    /// Reads a legacy PCI configuration dword through x86 configuration ports.
    ///
    /// This supports only segment zero and offsets below 256. It requires
    /// `CAP_SYS_RAWIO` (normally root) and serializes the global CF8/CFC pair.
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    pub fn read32_direct(address: PciAddress, offset: u16) -> Result<u32, PciError> {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        if address.segment() != 0 || offset > u8::MAX as u16 {
            return Err(PciError::NotSupported(
                "legacy PCI config access requires segment 0 and an offset below 256",
            ));
        }
        let _guard = LOCK.lock().expect("PCI configuration lock poisoned");
        if unsafe { libc::iopl(3) } != 0 {
            return Err(PciError::ConfigRead { address, offset });
        }
        let config_address = 0x8000_0000
            | ((address.bus() as u32) << 16)
            | ((address.device() as u32) << 11)
            | ((address.function() as u32) << 8)
            | ((offset as u32) & 0xfc);
        let value: u32;
        unsafe {
            core::arch::asm!("out dx, eax", in("dx") 0xcf8_u16, in("eax") config_address, options(nomem, nostack, preserves_flags));
            core::arch::asm!("in eax, dx", in("dx") 0xcfc_u16, out("eax") value, options(nomem, nostack, preserves_flags));
        }
        Ok(value)
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    pub fn read32_direct(_address: PciAddress, _offset: u16) -> Result<u32, PciError> {
        Err(PciError::NotSupported(
            "legacy PCI config access is only available on x86 Linux",
        ))
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
pub use linux::{SysfsPci, read32_direct};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_matches_ids() {
        let device = PciDevice {
            address: PciAddress::new(2, 0, 0x1f, 0),
            vendor_id: 0x1002,
            device_id: 0x438d,
            revision_id: 0,
            class: 0x010601,
        };
        assert!(device.matches(0x1002, 0x438d));
        assert!(!device.matches(0x1002, 0x438e));
        assert_eq!(device.base_class(), 0x01);
        assert_eq!(device.sub_class(), 0x06);
        assert_eq!(device.interface(), 0x01);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn ecam_access_uses_segment_relative_addresses() {
        use crate::PciConfigAccess as _;

        let mut memory = alloc::vec![0_u8; 1024 * 1024];
        let ecam = unsafe { Ecam::new(memory.as_mut_ptr(), 2, 3, 3) };
        let address = PciAddress::new(2, 3, 1, 2);
        ecam.write32(address, 0x00, 0x438d_1002).unwrap();
        ecam.write32(address, 0x10, 0x1234_5678).unwrap();

        assert_eq!(ecam.read32(address, 0x10).unwrap(), 0x1234_5678);
        assert_eq!(
            pci_types::PciHeader::new(address).id(ecam),
            (0x1002, 0x438d)
        );
        assert!(matches!(
            ecam.write32(address, 0x11, 0),
            Err(PciError::InvalidAccess { .. })
        ));
        assert!(matches!(
            ecam.read32(PciAddress::new(2, 4, 1, 2), 0x10),
            Err(PciError::InvalidAccess { .. })
        ));
    }

    #[cfg(all(feature = "std", target_os = "linux"))]
    #[test]
    fn sysfs_backend_enumerates_and_reads_a_fixture() {
        use crate::PciConfigAccess as _;
        use std::fs;

        let root = std::env::temp_dir().join(format!("rflasher-pci-test-{}", std::process::id()));
        let device = root.join("0002:03:1f.4");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&device).unwrap();
        fs::write(device.join("vendor"), "0x1002\n").unwrap();
        fs::write(device.join("device"), "0x438d\n").unwrap();
        fs::write(device.join("revision"), "0x41\n").unwrap();
        fs::write(device.join("class"), "0x010601\n").unwrap();
        let mut config = [0_u8; 64];
        config[0x10..0x12].copy_from_slice(&0x1234_u16.to_le_bytes());
        fs::write(device.join("config"), config).unwrap();

        let pci = SysfsPci::new(&root);
        let devices = pci.enumerate().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].address, PciAddress::new(2, 3, 0x1f, 4));
        assert_eq!(pci.read16(devices[0].address, 0x10).unwrap(), 0x1234);

        fs::remove_dir_all(root).unwrap();
    }
}

//! Host access abstractions for internal flash controllers.
//!
//! This module defines the small platform abstraction layer used by embedded
//! and userspace backends. Controller logic should depend on these traits
//! instead of directly opening Linux sysfs files, mapping `/dev/mem`, or
//! sleeping with `std`.

#![cfg_attr(
    not(any(all(feature = "std", target_os = "linux"), test)),
    allow(unused_imports)
)]

use crate::Result;
use crate::error::InternalError;

pub use rflasher_pci::{PciAddress, PciConfigAccess};

/// Provides volatile MMIO access to a mapped controller register window.
pub trait MmioAccess {
    /// Reads an 8-bit MMIO value at `offset`.
    fn read8(&self, offset: usize) -> u8;
    /// Reads a 16-bit MMIO value at `offset`.
    fn read16(&self, offset: usize) -> u16;
    /// Reads a 32-bit MMIO value at `offset`.
    fn read32(&self, offset: usize) -> u32;

    /// Writes an 8-bit MMIO value at `offset`.
    fn write8(&self, offset: usize, value: u8);
    /// Writes a 16-bit MMIO value at `offset`.
    fn write16(&self, offset: usize, value: u16);
    /// Writes a 32-bit MMIO value at `offset`.
    fn write32(&self, offset: usize, value: u32);
}

/// Provides platform services needed by internal flash controllers.
pub trait HostAccess: PciConfigAccess<Error = InternalError> {
    /// Mapped MMIO region type returned by [`HostAccess::map_mmio`].
    type MmioRegion: MmioAccess;

    /// Maps a physical MMIO range.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `phys_addr..phys_addr + size` is a valid
    /// MMIO range for the selected controller and that mapping it will not
    /// violate platform memory attributes or aliasing rules.
    unsafe fn map_mmio(&self, phys_addr: u64, size: usize) -> Result<Self::MmioRegion>;

    /// Delays for approximately `us` microseconds.
    fn delay_us(&self, us: u32);
}

/// Default PCI configuration-space backend for the current target.
///
/// On Linux userspace this uses sysfs config access and falls back to direct
/// x86 PCI config I/O for hidden devices. On unsupported targets it returns
/// [`InternalError::NotSupported`] via the PCI stubs; embedded callers should
/// provide their own [`PciConfigAccess`] implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultPciAccess;

impl PciConfigAccess for DefaultPciAccess {
    type Error = InternalError;

    fn read8(&self, address: PciAddress, offset: u16) -> Result<u8> {
        let _ = (address, offset);
        #[cfg(all(feature = "std", target_os = "linux"))]
        {
            rflasher_pci::SysfsPci::system()
                .read8(address, offset)
                .map_err(Into::into)
        }
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        Err(InternalError::NotSupported(
            "PCI config access not available",
        ))
    }

    fn read16(&self, address: PciAddress, offset: u16) -> Result<u16> {
        let _ = (address, offset);
        #[cfg(all(feature = "std", target_os = "linux"))]
        {
            rflasher_pci::SysfsPci::system()
                .read16(address, offset)
                .map_err(Into::into)
        }
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        Err(InternalError::NotSupported(
            "PCI config access not available",
        ))
    }

    fn read32(&self, address: PciAddress, offset: u16) -> Result<u32> {
        let _ = (address, offset);
        #[cfg(all(feature = "std", target_os = "linux"))]
        {
            rflasher_pci::SysfsPci::system()
                .read32(address, offset)
                .or_else(|error| {
                    if address.segment() == 0 {
                        rflasher_pci::read32_direct(address, offset)
                    } else {
                        Err(error)
                    }
                })
                .map_err(Into::into)
        }
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        Err(InternalError::NotSupported(
            "PCI config access not available",
        ))
    }

    fn write8(&self, address: PciAddress, offset: u16, value: u8) -> Result<()> {
        let _ = (address, offset, value);
        #[cfg(all(feature = "std", target_os = "linux"))]
        return rflasher_pci::SysfsPci::system()
            .write8(address, offset, value)
            .map_err(Into::into);
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        Err(InternalError::NotSupported(
            "PCI config access not available",
        ))
    }

    fn write16(&self, address: PciAddress, offset: u16, value: u16) -> Result<()> {
        let _ = (address, offset, value);
        #[cfg(all(feature = "std", target_os = "linux"))]
        return rflasher_pci::SysfsPci::system()
            .write16(address, offset, value)
            .map_err(Into::into);
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        Err(InternalError::NotSupported(
            "PCI config access not available",
        ))
    }

    fn write32(&self, address: PciAddress, offset: u16, value: u32) -> Result<()> {
        let _ = (address, offset, value);
        #[cfg(all(feature = "std", target_os = "linux"))]
        return rflasher_pci::SysfsPci::system()
            .write32(address, offset, value)
            .map_err(Into::into);
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        Err(InternalError::NotSupported(
            "PCI config access not available",
        ))
    }
}

/// Linux userspace host backend.
#[cfg(all(feature = "std", target_os = "linux"))]
#[derive(Debug, Default, Clone, Copy)]
pub struct LinuxHost;

#[cfg(all(feature = "std", target_os = "linux"))]
impl LinuxHost {
    /// Creates a Linux host backend.
    pub const fn new() -> Self {
        Self
    }

    /// Scans Linux sysfs for PCI devices.
    #[cfg(target_os = "linux")]
    pub fn scan_pci_bus(&self) -> Result<alloc::vec::Vec<crate::pci::PciDevice>> {
        crate::pci::scan_pci_bus()
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl PciConfigAccess for LinuxHost {
    type Error = InternalError;

    fn read8(&self, bdf: PciAddress, offset: u16) -> Result<u8> {
        DefaultPciAccess.read8(bdf, offset)
    }

    fn read16(&self, bdf: PciAddress, offset: u16) -> Result<u16> {
        DefaultPciAccess.read16(bdf, offset)
    }

    fn read32(&self, bdf: PciAddress, offset: u16) -> Result<u32> {
        DefaultPciAccess.read32(bdf, offset)
    }

    fn write8(&self, bdf: PciAddress, offset: u16, value: u8) -> Result<()> {
        DefaultPciAccess.write8(bdf, offset, value)
    }

    fn write16(&self, bdf: PciAddress, offset: u16, value: u16) -> Result<()> {
        DefaultPciAccess.write16(bdf, offset, value)
    }

    fn write32(&self, bdf: PciAddress, offset: u16, value: u32) -> Result<()> {
        DefaultPciAccess.write32(bdf, offset, value)
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl HostAccess for LinuxHost {
    type MmioRegion = crate::physmap::PhysMap;

    unsafe fn map_mmio(&self, phys_addr: u64, size: usize) -> Result<Self::MmioRegion> {
        // SAFETY: HostAccess::map_mmio has the same safety requirements as
        // PhysMap::new and forwards them to the caller.
        unsafe { crate::physmap::PhysMap::new(phys_addr, size) }
    }

    fn delay_us(&self, us: u32) {
        std::thread::sleep(std::time::Duration::from_micros(us as u64));
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::PciAccessError;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// Fake no-heap-shape host used by controller construction tests.
    #[derive(Default)]
    pub(crate) struct FakeHost {
        config: RefCell<BTreeMap<(PciAddress, u16), u32>>,
        writes: RefCell<Vec<(PciAddress, u16, u32, u8)>>,
        delays: RefCell<Vec<u32>>,
    }

    impl FakeHost {
        pub(crate) fn set_config32(&self, bdf: PciAddress, offset: u16, value: u32) {
            self.config.borrow_mut().insert((bdf, offset), value);
        }

        pub(crate) fn delay_log(&self) -> Vec<u32> {
            self.delays.borrow().clone()
        }
    }

    impl PciConfigAccess for FakeHost {
        type Error = InternalError;

        fn read8(&self, bdf: PciAddress, offset: u16) -> Result<u8> {
            let aligned = offset & !3;
            let shift = ((offset & 3) * 8) as u32;
            Ok(((self.read32(bdf, aligned)? >> shift) & 0xff) as u8)
        }

        fn read16(&self, bdf: PciAddress, offset: u16) -> Result<u16> {
            let aligned = offset & !3;
            let shift = ((offset & 2) * 8) as u32;
            Ok(((self.read32(bdf, aligned)? >> shift) & 0xffff) as u16)
        }

        fn read32(&self, bdf: PciAddress, offset: u16) -> Result<u32> {
            self.config.borrow().get(&(bdf, offset)).copied().ok_or({
                InternalError::PciAccess(PciAccessError::ConfigRead {
                    bus: bdf.bus(),
                    device: bdf.device(),
                    function: bdf.function(),
                    register: offset,
                })
            })
        }

        fn write8(&self, bdf: PciAddress, offset: u16, value: u8) -> Result<()> {
            self.writes
                .borrow_mut()
                .push((bdf, offset, value as u32, 1));
            Ok(())
        }

        fn write16(&self, bdf: PciAddress, offset: u16, value: u16) -> Result<()> {
            self.writes
                .borrow_mut()
                .push((bdf, offset, value as u32, 2));
            Ok(())
        }

        fn write32(&self, bdf: PciAddress, offset: u16, value: u32) -> Result<()> {
            self.writes.borrow_mut().push((bdf, offset, value, 4));
            self.config.borrow_mut().insert((bdf, offset), value);
            Ok(())
        }
    }

    pub(crate) struct FakeMmio;

    impl MmioAccess for FakeMmio {
        fn read8(&self, _offset: usize) -> u8 {
            0
        }
        fn read16(&self, _offset: usize) -> u16 {
            0
        }
        fn read32(&self, _offset: usize) -> u32 {
            0
        }
        fn write8(&self, _offset: usize, _value: u8) {}
        fn write16(&self, _offset: usize, _value: u16) {}
        fn write32(&self, _offset: usize, _value: u32) {}
    }

    impl HostAccess for FakeHost {
        type MmioRegion = FakeMmio;

        unsafe fn map_mmio(&self, _phys_addr: u64, _size: usize) -> Result<Self::MmioRegion> {
            Ok(FakeMmio)
        }

        fn delay_us(&self, us: u32) {
            self.delays.borrow_mut().push(us);
        }
    }

    #[test]
    fn pci_address_preserves_segment() {
        let bdf = PciAddress::new(2, 0, 0x1f, 5);
        assert_eq!(bdf.segment(), 2);
        assert_eq!(bdf.bus(), 0);
        assert_eq!(bdf.device(), 0x1f);
        assert_eq!(bdf.function(), 5);
    }

    #[test]
    fn test_fake_host_config_access_and_delay() {
        let host = FakeHost::default();
        let bdf = PciAddress::new(0, 0, 0x1f, 0);
        host.set_config32(bdf, 0x10, 0x1234_5678);

        assert_eq!(host.read8(bdf, 0x10).unwrap(), 0x78);
        assert_eq!(host.read16(bdf, 0x12).unwrap(), 0x1234);
        assert_eq!(host.read32(bdf, 0x10).unwrap(), 0x1234_5678);

        host.delay_us(7);
        assert_eq!(host.delay_log(), vec![7]);
    }
}

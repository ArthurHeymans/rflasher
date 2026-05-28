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
use crate::error::{InternalError, PciAccessError};

/// PCI segment:bus:device.function identifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Bdf {
    /// PCI segment/domain.
    pub segment: u16,
    /// PCI bus number.
    pub bus: u8,
    /// PCI device number.
    pub device: u8,
    /// PCI function number.
    pub function: u8,
}

impl Bdf {
    /// Creates a BDF in PCI segment 0.
    pub const fn new(bus: u8, device: u8, function: u8) -> Self {
        Self {
            segment: 0,
            bus,
            device,
            function,
        }
    }

    /// Creates a BDF with an explicit PCI segment/domain.
    pub const fn with_segment(segment: u16, bus: u8, device: u8, function: u8) -> Self {
        Self {
            segment,
            bus,
            device,
            function,
        }
    }
}

impl From<&crate::pci::PciDevice> for Bdf {
    fn from(device: &crate::pci::PciDevice) -> Self {
        Self::with_segment(device.domain, device.bus, device.device, device.function)
    }
}

/// Provides PCI configuration-space access for visible or hidden devices.
pub trait PciConfigAccess {
    /// Reads an 8-bit PCI config-space value.
    fn read8(&self, bdf: Bdf, offset: u16) -> Result<u8>;
    /// Reads a 16-bit PCI config-space value.
    fn read16(&self, bdf: Bdf, offset: u16) -> Result<u16>;
    /// Reads a 32-bit PCI config-space value.
    fn read32(&self, bdf: Bdf, offset: u16) -> Result<u32>;

    /// Writes an 8-bit PCI config-space value.
    fn write8(&self, bdf: Bdf, offset: u16, value: u8) -> Result<()>;
    /// Writes a 16-bit PCI config-space value.
    fn write16(&self, bdf: Bdf, offset: u16, value: u16) -> Result<()>;
    /// Writes a 32-bit PCI config-space value.
    fn write32(&self, bdf: Bdf, offset: u16, value: u32) -> Result<()>;
}

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
pub trait HostAccess: PciConfigAccess {
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

fn offset_to_u8(bdf: Bdf, offset: u16, write: bool) -> Result<u8> {
    if offset > u8::MAX as u16 {
        let error = if write {
            PciAccessError::ConfigWrite {
                bus: bdf.bus,
                device: bdf.device,
                function: bdf.function,
                register: offset,
            }
        } else {
            PciAccessError::ConfigRead {
                bus: bdf.bus,
                device: bdf.device,
                function: bdf.function,
                register: offset,
            }
        };
        return Err(InternalError::PciAccess(error));
    }

    Ok(offset as u8)
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
    fn read8(&self, bdf: Bdf, offset: u16) -> Result<u8> {
        let offset = offset_to_u8(bdf, offset, false)?;
        #[cfg(all(feature = "std", target_os = "linux"))]
        {
            crate::pci::pci_read_config8_at(bdf.segment, bdf.bus, bdf.device, bdf.function, offset)
        }
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        {
            crate::pci::pci_read_config8(bdf.bus, bdf.device, bdf.function, offset)
        }
    }

    fn read16(&self, bdf: Bdf, offset: u16) -> Result<u16> {
        let offset = offset_to_u8(bdf, offset, false)?;
        #[cfg(all(feature = "std", target_os = "linux"))]
        {
            crate::pci::pci_read_config16_at(bdf.segment, bdf.bus, bdf.device, bdf.function, offset)
        }
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        {
            crate::pci::pci_read_config16(bdf.bus, bdf.device, bdf.function, offset)
        }
    }

    fn read32(&self, bdf: Bdf, offset: u16) -> Result<u32> {
        let offset = offset_to_u8(bdf, offset, false)?;
        #[cfg(all(feature = "std", target_os = "linux"))]
        {
            crate::pci::pci_read_config32_at(bdf.segment, bdf.bus, bdf.device, bdf.function, offset)
                .or_else(|err| {
                    if bdf.segment == 0 {
                        crate::pci::pci_read_config32_direct(
                            bdf.bus,
                            bdf.device,
                            bdf.function,
                            offset,
                        )
                    } else {
                        Err(err)
                    }
                })
        }
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        {
            crate::pci::pci_read_config32(bdf.bus, bdf.device, bdf.function, offset).or_else(|_| {
                crate::pci::pci_read_config32_direct(bdf.bus, bdf.device, bdf.function, offset)
            })
        }
    }

    fn write8(&self, bdf: Bdf, offset: u16, value: u8) -> Result<()> {
        let offset = offset_to_u8(bdf, offset, true)?;
        #[cfg(all(feature = "std", target_os = "linux"))]
        {
            crate::pci::pci_write_config8_at(
                bdf.segment,
                bdf.bus,
                bdf.device,
                bdf.function,
                offset,
                value,
            )
        }
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        {
            crate::pci::pci_write_config8(bdf.bus, bdf.device, bdf.function, offset, value)
        }
    }

    fn write16(&self, bdf: Bdf, offset: u16, value: u16) -> Result<()> {
        let offset = offset_to_u8(bdf, offset, true)?;
        #[cfg(all(feature = "std", target_os = "linux"))]
        {
            crate::pci::pci_write_config16_at(
                bdf.segment,
                bdf.bus,
                bdf.device,
                bdf.function,
                offset,
                value,
            )
        }
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        {
            crate::pci::pci_write_config16(bdf.bus, bdf.device, bdf.function, offset, value)
        }
    }

    fn write32(&self, bdf: Bdf, offset: u16, value: u32) -> Result<()> {
        let offset = offset_to_u8(bdf, offset, true)?;
        #[cfg(all(feature = "std", target_os = "linux"))]
        {
            crate::pci::pci_write_config32_at(
                bdf.segment,
                bdf.bus,
                bdf.device,
                bdf.function,
                offset,
                value,
            )
        }
        #[cfg(not(all(feature = "std", target_os = "linux")))]
        {
            crate::pci::pci_write_config32(bdf.bus, bdf.device, bdf.function, offset, value)
        }
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
    fn read8(&self, bdf: Bdf, offset: u16) -> Result<u8> {
        DefaultPciAccess.read8(bdf, offset)
    }

    fn read16(&self, bdf: Bdf, offset: u16) -> Result<u16> {
        DefaultPciAccess.read16(bdf, offset)
    }

    fn read32(&self, bdf: Bdf, offset: u16) -> Result<u32> {
        DefaultPciAccess.read32(bdf, offset)
    }

    fn write8(&self, bdf: Bdf, offset: u16, value: u8) -> Result<()> {
        DefaultPciAccess.write8(bdf, offset, value)
    }

    fn write16(&self, bdf: Bdf, offset: u16, value: u16) -> Result<()> {
        DefaultPciAccess.write16(bdf, offset, value)
    }

    fn write32(&self, bdf: Bdf, offset: u16, value: u32) -> Result<()> {
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
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// Fake no-heap-shape host used by controller construction tests.
    #[derive(Default)]
    pub(crate) struct FakeHost {
        config: RefCell<BTreeMap<(Bdf, u16), u32>>,
        writes: RefCell<Vec<(Bdf, u16, u32, u8)>>,
        delays: RefCell<Vec<u32>>,
    }

    impl FakeHost {
        pub(crate) fn set_config32(&self, bdf: Bdf, offset: u16, value: u32) {
            self.config.borrow_mut().insert((bdf, offset), value);
        }

        pub(crate) fn delay_log(&self) -> Vec<u32> {
            self.delays.borrow().clone()
        }
    }

    impl PciConfigAccess for FakeHost {
        fn read8(&self, bdf: Bdf, offset: u16) -> Result<u8> {
            let aligned = offset & !3;
            let shift = ((offset & 3) * 8) as u32;
            Ok(((self.read32(bdf, aligned)? >> shift) & 0xff) as u8)
        }

        fn read16(&self, bdf: Bdf, offset: u16) -> Result<u16> {
            let aligned = offset & !3;
            let shift = ((offset & 2) * 8) as u32;
            Ok(((self.read32(bdf, aligned)? >> shift) & 0xffff) as u16)
        }

        fn read32(&self, bdf: Bdf, offset: u16) -> Result<u32> {
            self.config.borrow().get(&(bdf, offset)).copied().ok_or({
                InternalError::PciAccess(PciAccessError::ConfigRead {
                    bus: bdf.bus,
                    device: bdf.device,
                    function: bdf.function,
                    register: offset,
                })
            })
        }

        fn write8(&self, bdf: Bdf, offset: u16, value: u8) -> Result<()> {
            self.writes
                .borrow_mut()
                .push((bdf, offset, value as u32, 1));
            Ok(())
        }

        fn write16(&self, bdf: Bdf, offset: u16, value: u16) -> Result<()> {
            self.writes
                .borrow_mut()
                .push((bdf, offset, value as u32, 2));
            Ok(())
        }

        fn write32(&self, bdf: Bdf, offset: u16, value: u32) -> Result<()> {
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
    fn test_bdf_preserves_segment() {
        let bdf = Bdf::with_segment(2, 0, 0x1f, 5);
        assert_eq!(bdf.segment, 2);
        assert_eq!(bdf.bus, 0);
        assert_eq!(bdf.device, 0x1f);
        assert_eq!(bdf.function, 5);
    }

    #[test]
    fn test_fake_host_config_access_and_delay() {
        let host = FakeHost::default();
        let bdf = Bdf::new(0, 0x1f, 0);
        host.set_config32(bdf, 0x10, 0x1234_5678);

        assert_eq!(host.read8(bdf, 0x10).unwrap(), 0x78);
        assert_eq!(host.read16(bdf, 0x12).unwrap(), 0x1234);
        assert_eq!(host.read32(bdf, 0x10).unwrap(), 0x1234_5678);

        host.delay_us(7);
        assert_eq!(host.delay_log(), vec![7]);
    }
}

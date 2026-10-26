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

    /// Reads a 64-bit value from a data window that permits wide reads.
    /// Backends may override this with a single native 64-bit load.
    fn read64(&self, offset: usize) -> u64 {
        u64::from(self.read32(offset)) | (u64::from(self.read32(offset + 4)) << 32)
    }

    /// Copies from a data window using source-aligned 4- or 8-byte reads.
    /// Unaligned leading and trailing bytes are read individually, never
    /// accessing outside the requested range. Not for side-effecting registers.
    fn read_aligned(&self, mut offset: usize, dst: &mut [u8], alignment: usize) {
        assert!(matches!(alignment, 4 | 8));
        let prefix = ((alignment - offset % alignment) % alignment).min(dst.len());
        let (head, rest) = dst.split_at_mut(prefix);
        for byte in head {
            *byte = self.read8(offset);
            offset += 1;
        }
        let mut chunks = rest.chunks_exact_mut(alignment);
        for chunk in &mut chunks {
            if alignment == 8 {
                chunk.copy_from_slice(&self.read64(offset).to_le_bytes());
            } else {
                chunk.copy_from_slice(&self.read32(offset).to_le_bytes());
            }
            offset += alignment;
        }
        for byte in chunks.into_remainder() {
            *byte = self.read8(offset);
            offset += 1;
        }
    }

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
        let duration = std::time::Duration::from_micros(u64::from(us));
        // Scheduler sleeps can turn a 1 us SPI poll into tens of microseconds.
        // Only busy-wait for short controller polls, not flash program/erase waits.
        if us <= 10 {
            let start = std::time::Instant::now();
            while start.elapsed() < duration {
                core::hint::spin_loop();
            }
        } else {
            std::thread::sleep(duration);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::PciAccessError;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

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

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct FakeSpiCommand {
        pub opcode: u8,
        pub write_data: Vec<u8>,
        pub read_len: usize,
    }

    /// Register-backed fake controller window.
    ///
    /// Plain registers read back what was written (32-bit accesses share
    /// storage with the narrower ones by offset, which is all the tests
    /// need). On top of that it models the ICH9-style hardware sequencer just
    /// enough to observe erases: an erase cycle started through HSFC is
    /// recorded with the current FADDR and completes at once, and HSFS.BERASE
    /// reads back a per-partition value chosen through [`FakeMmio::set_berase`]
    /// (partitions split at the FPB boundary). SPI100 commands are recorded
    /// on trigger and return the configured response at the FIFO receive offset.
    #[derive(Default, Clone)]
    pub(crate) struct FakeMmio {
        regs: Arc<Mutex<BTreeMap<usize, u32>>>,
        writes: Arc<Mutex<Vec<(usize, u32, usize)>>>,
        blocked_write32: RefCell<Option<usize>>,
        bytes: RefCell<BTreeMap<usize, u8>>,
        reads: RefCell<Vec<(usize, usize)>>,
        berase: RefCell<[u16; 2]>,
        erase_cycles: RefCell<Vec<u32>>,
        spi_commands: RefCell<Vec<FakeSpiCommand>>,
        spi_response: RefCell<Vec<u8>>,
        descriptor: RefCell<Option<(usize, BTreeMap<u32, u32>)>>,
    }

    impl FakeMmio {
        pub(crate) fn set_descriptor(&self, fdoc: usize, signature: u32, map: u32, comp: u32) {
            *self.descriptor.borrow_mut() =
                Some((fdoc, [(0, signature), (4, map), (0x1000, comp)].into()));
        }

        /// Set the BERASE encodings reported below and from the FPB boundary
        pub(crate) fn set_berase(&self, low: u16, high: u16) {
            *self.berase.borrow_mut() = [low, high];
        }

        pub(crate) fn block_write32(&self, offset: usize) {
            *self.blocked_write32.borrow_mut() = Some(offset);
        }

        pub(crate) fn set32(&self, offset: usize, value: u32) {
            self.regs.lock().unwrap().insert(offset, value);
        }

        /// FADDR of every erase cycle started so far
        pub(crate) fn erase_cycles(&self) -> Vec<u32> {
            self.erase_cycles.borrow().clone()
        }

        pub(crate) fn spi_commands(&self) -> Vec<FakeSpiCommand> {
            self.spi_commands.borrow().clone()
        }

        pub(crate) fn set_spi_response(&self, data: &[u8]) {
            self.spi_response.replace(data.to_vec());
        }

        pub(crate) fn set_bytes(&self, offset: usize, data: &[u8]) {
            self.bytes
                .borrow_mut()
                .extend(data.iter().enumerate().map(|(i, &b)| (offset + i, b)));
        }

        pub(crate) fn writes(&self) -> Vec<(usize, u32, usize)> {
            self.writes.lock().unwrap().clone()
        }

        pub(crate) fn reads(&self) -> Vec<(usize, usize)> {
            self.reads.borrow().clone()
        }

        fn byte(&self, offset: usize) -> u8 {
            self.bytes
                .borrow()
                .get(&offset)
                .copied()
                .unwrap_or_else(|| self.reg(offset) as u8)
        }

        fn reg(&self, offset: usize) -> u32 {
            self.regs.lock().unwrap().get(&offset).copied().unwrap_or(0)
        }
    }

    impl MmioAccess for FakeMmio {
        fn read8(&self, offset: usize) -> u8 {
            self.reads.borrow_mut().push((offset, 1));
            self.byte(offset)
        }
        fn read16(&self, offset: usize) -> u16 {
            let value = self.reg(offset) as u16;
            if offset != crate::ich_regs::ICH9_REG_HSFS {
                return value;
            }
            let boundary =
                (self.reg(crate::ich_regs::ICH9_REG_FPB) & crate::ich_regs::FPB_FPBA) << 12;
            let fla = self.reg(crate::ich_regs::ICH9_REG_FADDR) & crate::ich_regs::ICH9_FADDR_FLA;
            let partition = usize::from(boundary != 0 && fla >= boundary);
            (value & !crate::ich_regs::HSFS_BERASE)
                | (self.berase.borrow()[partition] << crate::ich_regs::HSFS_BERASE_OFF)
        }
        fn read32(&self, offset: usize) -> u32 {
            self.reads.borrow_mut().push((offset, 4));
            if let Some((fdoc, values)) = &*self.descriptor.borrow()
                && offset == fdoc + 4
            {
                return values.get(&self.reg(*fdoc)).copied().unwrap_or(0);
            }
            if self.bytes.borrow().contains_key(&offset) {
                u32::from_le_bytes(core::array::from_fn(|i| self.byte(offset + i)))
            } else {
                self.reg(offset)
            }
        }
        fn read64(&self, offset: usize) -> u64 {
            self.reads.borrow_mut().push((offset, 8));
            u64::from_le_bytes(core::array::from_fn(|i| self.byte(offset + i)))
        }
        fn write8(&self, offset: usize, value: u8) {
            self.writes
                .lock()
                .unwrap()
                .push((offset, u32::from(value), 1));
            if offset >= 0x80 {
                self.bytes.borrow_mut().insert(offset, value);
            } else {
                self.set32(offset, value as u32);
            }
            // SPI100 CMD_TRIGGER.Execute: consume TX bytes and place the
            // response immediately after them, matching the hardware FIFO.
            if offset == 0x47 && value & 0x80 != 0 {
                let write_len = self.reg(0x48) as usize;
                let read_len = self.reg(0x4b) as usize;
                self.spi_commands.borrow_mut().push(FakeSpiCommand {
                    opcode: self.reg(0x45) as u8,
                    write_data: (0..write_len).map(|i| self.byte(0x80 + i)).collect(),
                    read_len,
                });
                let response = self.spi_response.borrow();
                for i in 0..read_len {
                    self.bytes
                        .borrow_mut()
                        .insert(0x80 + write_len + i, response.get(i).copied().unwrap_or(0));
                }
            }
        }
        fn write16(&self, offset: usize, value: u16) {
            self.writes
                .lock()
                .unwrap()
                .push((offset, u32::from(value), 2));
            use crate::ich_regs::*;
            match offset {
                // Status bits are write-1-to-clear.
                ICH9_REG_HSFS => {
                    let w1c = HSFS_FDONE | HSFS_FCERR | HSFS_AEL;
                    self.set32(offset, self.reg(offset) & !u32::from(value & w1c));
                }
                ICH9_REG_HSFC => {
                    self.set32(offset, u32::from(value & !HSFC_FGO));
                    let fcycle = (value & HSFC_FCYCLE) >> HSFC_FCYCLE_OFF;
                    if value & HSFC_FGO != 0 {
                        if fcycle == 0x3 {
                            self.erase_cycles
                                .borrow_mut()
                                .push(self.reg(ICH9_REG_FADDR) & ICH9_FADDR_FLA);
                        }
                        self.set32(
                            ICH9_REG_HSFS,
                            self.reg(ICH9_REG_HSFS) | u32::from(HSFS_FDONE),
                        );
                    }
                }
                _ => self.set32(offset, value as u32),
            }
        }
        fn write32(&self, offset: usize, value: u32) {
            self.writes.lock().unwrap().push((offset, value, 4));
            if *self.blocked_write32.borrow() != Some(offset) {
                self.set32(offset, value);
            }
        }
    }

    impl HostAccess for FakeHost {
        type MmioRegion = FakeMmio;

        unsafe fn map_mmio(&self, _phys_addr: u64, _size: usize) -> Result<Self::MmioRegion> {
            Ok(FakeMmio::default())
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
    fn aligned_data_reads_preserve_bytes_and_stay_within_the_requested_range() {
        let source: Vec<u8> = (0..96).map(|i| (i * 17) as u8).collect();
        for alignment in [4, 8] {
            for start in 0..alignment {
                for len in 0..=71 {
                    let memory = FakeMmio::default();
                    memory.set_bytes(0, &source);
                    let mut data = vec![0xcc; len + 2];
                    memory.read_aligned(start, &mut data[1..len + 1], alignment);
                    assert_eq!(&data[1..len + 1], &source[start..start + len]);
                    assert_eq!((data[0], data[len + 1]), (0xcc, 0xcc));
                    let reads = memory.reads();
                    assert!(reads.iter().all(|&(offset, width)| {
                        offset >= start
                            && offset + width <= start + len
                            && (width == 1 || (width == alignment && offset % alignment == 0))
                    }));
                    let prefix = ((alignment - start % alignment) % alignment).min(len);
                    assert_eq!(
                        reads
                            .iter()
                            .filter(|&&(_, width)| width == alignment)
                            .count(),
                        (len - prefix) / alignment
                    );
                }
            }
        }
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

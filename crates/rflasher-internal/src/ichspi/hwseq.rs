//! Hardware-sequencing flash operations (read, write, erase, geometry).

use super::{
    Deadline, HWSEQ_MAX_DATA, HWSEQ_ONLY_4K_ERASE_SIZE, IchSpiController, berase_block_size,
    uniform_erase_size,
};
use crate::chipset::IchChipset;
use crate::error::InternalError;
use crate::host::{HostAccess, MmioAccess};
use crate::ich_regs::*;

use rflasher_core::programmer::FlashAccess;

impl<H: HostAccess> IchSpiController<H> {
    // ========================================================================
    // Hardware Sequencing Operations
    // ========================================================================

    /// Read the descriptor through the controller's observability registers.
    /// Only generations whose FLCOMP encoding is documented by flashprog are
    /// handled; later engines retain the region-based fallback.
    pub(super) fn descriptor_flash_size(&self) -> Result<Option<u32>, InternalError> {
        if !self.desc_valid {
            return Err(InternalError::InvalidDescriptor);
        }
        let new_density = match self.generation {
            IchChipset::Ich9
            | IchChipset::Ich10
            | IchChipset::Series5IbexPeak
            | IchChipset::Series6CougarPoint
            | IchChipset::Series7PantherPoint
            | IchChipset::BayTrail => false,
            IchChipset::Series8LynxPoint
            | IchChipset::Series8LynxPointLp
            | IchChipset::Series8Wellsburg
            | IchChipset::Series9WildcatPoint
            | IchChipset::Series9WildcatPointLp
            | IchChipset::Series100SunrisePoint => true,
            _ => return Ok(None),
        };
        let (fdoc, fdod) = if self.generation.is_pch100_compatible() {
            (0xb4, 0xb8)
        } else {
            (0xb0, 0xb4)
        };
        let read = |selector| {
            self.spibar.write32(fdoc, selector);
            self.spibar.read32(fdod)
        };
        let saved_selector = self.spibar.read32(fdoc);
        let signature = read(0);
        let map = read(4);
        let comp = read(1 << 12);
        self.spibar.write32(fdoc, saved_selector);
        if signature != 0x0ff0_a55a {
            return Err(InternalError::InvalidDescriptor);
        }
        let count = (map >> 8) & 3;
        if count > 1 {
            return Err(InternalError::InvalidDescriptor);
        }
        let width = if new_density { 4 } else { 3 };
        let mask = (1 << width) - 1;
        let max = if new_density { 7 } else { 5 };
        let density = |index: u32| -> Option<u32> {
            let code = (comp >> (index * width)) & mask;
            if code > max {
                None
            } else {
                Some(1u32 << (19 + code))
            }
        };
        let first = density(0).ok_or(InternalError::InvalidDescriptor)?;
        let second = if count == 1 {
            density(1).ok_or(InternalError::InvalidDescriptor)?
        } else {
            0
        };
        let size = first
            .checked_add(second)
            .ok_or(InternalError::InvalidDescriptor)?;
        if u64::from(size) > u64::from(self.hwseq.addr_mask) + 1 {
            return Err(InternalError::NotSupported(
                "Flash exceeds hardware sequencing address range",
            ));
        }
        Ok(Some(size))
    }

    /// Reject ranges that FADDR would truncate before touching controller registers.
    pub(super) fn hwseq_check_range(&self, addr: u32, len: usize) -> Result<(), InternalError> {
        let end = u64::try_from(len)
            .ok()
            .and_then(|len| (addr as u64).checked_add(len));
        if addr & !self.hwseq.addr_mask != 0
            || end.is_none_or(|end| end > self.hwseq.addr_mask as u64 + 1)
        {
            return Err(InternalError::Io(
                "Hardware sequencing address out of range",
            ));
        }
        Ok(())
    }

    /// Set the flash address for hardware sequencing
    #[inline(always)]
    pub(super) fn hwseq_set_addr(&self, addr: u32) {
        self.spibar
            .write32(ICH9_REG_FADDR, addr & self.hwseq.addr_mask);
    }

    /// Wait for hardware sequencing cycle to complete
    ///
    /// Polls for FDONE or FCERR in HSFS register.
    #[inline(always)]
    pub(super) fn hwseq_wait_for_cycle(&self, timeout_us: u32) -> Result<(), InternalError> {
        let done_or_err = HSFS_FDONE | HSFS_FCERR;

        let mut deadline = Deadline::new(timeout_us);

        loop {
            let hsfs = self.spibar.read16(ICH9_REG_HSFS);

            if hsfs & done_or_err != 0 {
                // Clear status bits by writing 1s to them (W1C)
                self.spibar.write16(ICH9_REG_HSFS, hsfs);

                if hsfs & HSFS_FCERR != 0 {
                    return Err(InternalError::Io("Hardware sequencing cycle error"));
                }

                return Ok(());
            }

            if deadline.expired() {
                return Err(InternalError::Io("Hardware sequencing timeout"));
            }

            deadline.wait_step(&self.host);
        }
    }

    /// Read data using hardware sequencing
    ///
    /// This is the main read path - optimized for throughput.
    pub fn hwseq_read(&self, addr: u32, buf: &mut [u8]) -> Result<(), InternalError> {
        let len = buf.len();
        self.hwseq_check_range(addr, len)?;
        if len == 0 {
            return Ok(());
        }

        let mut offset = 0;
        let mut current_addr = addr as u64;

        // Clear FDONE, FCERR, AEL by writing 1s to them (do once at start)
        self.spibar
            .write16(ICH9_REG_HSFS, self.spibar.read16(ICH9_REG_HSFS));

        while offset < len {
            // Calculate block size (max 64 bytes, respect 256-byte page boundaries)
            let remaining = len - offset;
            let page_remaining = 256 - (current_addr as usize & 0xFF);
            let block_len = remaining.min(HWSEQ_MAX_DATA).min(page_remaining);

            self.hwseq_set_addr(current_addr as u32);

            // Set up read cycle using read-modify-write to preserve reserved bits
            let mut hsfc = self.spibar.read16(ICH9_REG_HSFC);
            hsfc &= !self.hwseq.hsfc_fcycle; // Clear FCYCLE (0 = read)
            hsfc &= !HSFC_FDBC; // Clear byte count
            hsfc |= ((block_len - 1) as u16) << HSFC_FDBC_OFF; // Set byte count
            hsfc |= HSFC_FGO; // Start
            self.spibar.write16(ICH9_REG_HSFC, hsfc);

            // Wait for completion (30 second timeout)
            self.hwseq_wait_for_cycle(30_000_000)?;

            // Read data from FDATA registers
            self.read_data(&mut buf[offset..offset + block_len]);

            offset += block_len;
            current_addr += block_len as u64;
        }

        Ok(())
    }

    /// Write data using hardware sequencing
    ///
    /// This is the main write path - optimized for throughput.
    pub fn hwseq_write(&mut self, addr: u32, data: &[u8]) -> Result<(), InternalError> {
        let len = data.len();
        self.hwseq_check_range(addr, len)?;
        if len == 0 {
            return Ok(());
        }

        // Refuse before the first chunk, not part-way through.
        self.check_range(FlashAccess::Modify, addr, len as u64)?;
        self.mutation_gate()?;
        let mut offset = 0;
        let mut current_addr = addr as u64;

        // Clear FDONE, FCERR, AEL by writing 1s to them (do once at start)
        self.spibar
            .write16(ICH9_REG_HSFS, self.spibar.read16(ICH9_REG_HSFS));

        while offset < len {
            // Calculate block size (max 64 bytes, respect 256-byte page boundaries)
            let remaining = len - offset;
            let page_remaining = 256 - (current_addr as usize & 0xFF);
            let block_len = remaining.min(HWSEQ_MAX_DATA).min(page_remaining);

            self.hwseq_set_addr(current_addr as u32);

            // Fill data registers first (before starting cycle)
            self.fill_data(&data[offset..offset + block_len]);

            // Set up write cycle using read-modify-write to preserve reserved bits
            let mut hsfc = self.spibar.read16(ICH9_REG_HSFC);
            hsfc &= !self.hwseq.hsfc_fcycle; // Clear FCYCLE
            hsfc |= 0x2 << HSFC_FCYCLE_OFF; // Set write cycle
            hsfc &= !HSFC_FDBC; // Clear byte count
            hsfc |= ((block_len - 1) as u16) << HSFC_FDBC_OFF; // Set byte count
            hsfc |= HSFC_FGO; // Start
            self.spibar.write16(ICH9_REG_HSFC, hsfc);

            // Wait for completion (30 second timeout)
            self.hwseq_wait_for_cycle(30_000_000)?;

            offset += block_len;
            current_addr += block_len as u64;
        }

        Ok(())
    }

    /// Size of the erase block the sequencer will erase at `addr`.
    ///
    /// PCH100 and later always erase 4 KiB. Older controllers erase whatever
    /// HSFS.BERASE says, and BERASE depends on the flash partition FADDR
    /// points into (see `hwseq_erase_geometry`), so FADDR is loaded first;
    /// this mirrors flashprog's `ich_hwseq_get_erase_block_size`.
    pub(super) fn hwseq_erase_block_size(&self, addr: u32) -> u32 {
        if self.hwseq.only_4k {
            return HWSEQ_ONLY_4K_ERASE_SIZE;
        }
        self.hwseq_set_addr(addr);
        berase_block_size(self.spibar.read16(ICH9_REG_HSFS))
    }

    /// Uniform erase block size to plan erases with, for a flash of `flash_size` bytes.
    ///
    /// Erase cycles always erase the *whole* block containing FADDR, and the
    /// block size is a property of the hardware (BERASE, per flash
    /// partition), not of the request. Planning with any other size would
    /// erase data next to the requested range, so it is read here instead of
    /// assumed.
    pub fn hwseq_erase_geometry(&self, flash_size: u32) -> Result<u32, InternalError> {
        let boundary = if self.hwseq.only_4k {
            0
        } else {
            (self.spibar.read32(ICH9_REG_FPB) & FPB_FPBA) << 12
        };
        let low = self.hwseq_erase_block_size(0);
        let high = if boundary != 0 && boundary < flash_size {
            self.hwseq_erase_block_size(boundary)
        } else {
            low
        };
        uniform_erase_size(low, high, boundary).ok_or(InternalError::NotSupported(
            "Hardware sequencing reports different erase block sizes for the two flash partitions",
        ))
    }

    /// Erase blocks using hardware sequencing
    ///
    /// `addr` and `len` must consist of whole hardware erase blocks. This is
    /// verified for the entire range before the first cycle is started.
    pub fn hwseq_erase(&mut self, addr: u32, len: u32) -> Result<(), InternalError> {
        self.hwseq_check_range(addr, len as usize)?;

        let end_addr = addr as u64 + len as u64;

        // Each cycle erases the block containing FADDR whatever we asked for,
        // so refuse anything that is not exactly a sequence of whole blocks.
        let mut current_addr = addr as u64;
        while current_addr < end_addr {
            let erase_size = self.hwseq_erase_block_size(current_addr as u32) as u64;
            if !current_addr.is_multiple_of(erase_size) || current_addr + erase_size > end_addr {
                return Err(InternalError::Io(
                    "Erase address/length not aligned to the controller's erase block size",
                ));
            }
            current_addr += erase_size;
        }

        if len == 0 {
            return Ok(());
        }
        // Whole hardware blocks (checked above), so this is what is erased.
        self.check_range(FlashAccess::Modify, addr, len.into())?;
        self.mutation_gate()?;
        // Clear FDONE, FCERR, AEL by writing 1s to them (do once at start)
        self.spibar
            .write16(ICH9_REG_HSFS, self.spibar.read16(ICH9_REG_HSFS));

        let mut current_addr = addr as u64;
        while current_addr < end_addr {
            let erase_size = self.hwseq_erase_block_size(current_addr as u32) as u64;
            self.hwseq_set_addr(current_addr as u32);

            // Set up erase cycle using read-modify-write to preserve reserved bits
            let mut hsfc = self.spibar.read16(ICH9_REG_HSFC);
            hsfc &= !self.hwseq.hsfc_fcycle; // Clear FCYCLE
            hsfc |= 0x3 << HSFC_FCYCLE_OFF; // Set erase cycle
            hsfc |= HSFC_FGO; // Start
            self.spibar.write16(ICH9_REG_HSFC, hsfc);

            // Wait for completion (60 second timeout for erase)
            self.hwseq_wait_for_cycle(60_000_000)?;

            current_addr += erase_size;
        }

        Ok(())
    }

    /// Read data from FDATA registers
    ///
    /// Optimized to read full 32-bit words and extract bytes.
    /// This is performance-critical for flash read operations.
    #[inline(always)]
    pub(super) fn read_data(&self, buf: &mut [u8]) {
        let len = buf.len();
        let mut offset = 0;

        // Process full 32-bit words
        while offset + 4 <= len {
            let temp = self.spibar.read32(ICH9_REG_FDATA0 + offset);
            // Use native endianness (x86 is little-endian, matching flash byte order)
            buf[offset] = temp as u8;
            buf[offset + 1] = (temp >> 8) as u8;
            buf[offset + 2] = (temp >> 16) as u8;
            buf[offset + 3] = (temp >> 24) as u8;
            offset += 4;
        }

        // Handle remaining bytes (0-3)
        if offset < len {
            let temp = self.spibar.read32(ICH9_REG_FDATA0 + offset);
            let remaining = len - offset;
            if remaining > 0 {
                buf[offset] = temp as u8;
            }
            if remaining > 1 {
                buf[offset + 1] = (temp >> 8) as u8;
            }
            if remaining > 2 {
                buf[offset + 2] = (temp >> 16) as u8;
            }
        }
    }

    /// Fill FDATA registers with data
    ///
    /// Optimized to write full 32-bit words.
    /// This is performance-critical for flash write operations.
    #[inline(always)]
    pub(super) fn fill_data(&self, data: &[u8]) {
        let len = data.len();
        if len == 0 {
            return;
        }

        let mut offset = 0;

        // Process full 32-bit words
        while offset + 4 <= len {
            let temp = (data[offset] as u32)
                | ((data[offset + 1] as u32) << 8)
                | ((data[offset + 2] as u32) << 16)
                | ((data[offset + 3] as u32) << 24);
            self.spibar.write32(ICH9_REG_FDATA0 + offset, temp);
            offset += 4;
        }

        // Handle remaining bytes (0-3)
        if offset < len {
            let mut temp: u32 = 0;
            let remaining = len - offset;
            if remaining > 0 {
                temp |= data[offset] as u32;
            }
            if remaining > 1 {
                temp |= (data[offset + 1] as u32) << 8;
            }
            if remaining > 2 {
                temp |= (data[offset + 2] as u32) << 16;
            }
            self.spibar.write32(ICH9_REG_FDATA0 + offset, temp);
        }
    }
}

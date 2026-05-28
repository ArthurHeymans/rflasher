//! Physical memory mapping for MMIO access.
//!
//! Linux userspace maps controller registers through `/dev/mem`. Firmware
//! builds use direct physical-address MMIO and rely on the caller/platform to
//! have installed suitable mappings and memory attributes.

use crate::error::InternalError;

/// A mapped region of physical memory.
pub struct PhysMap {
    /// Pointer to the mapped memory.
    ptr: *mut u8,
    /// Requested window size.
    size: usize,
    /// Actual mapped size, after host page alignment.
    #[cfg(all(feature = "std", target_os = "linux"))]
    map_size: usize,
    /// Physical address, for reporting and Linux unmapping.
    phys_addr: u64,
}

impl PhysMap {
    /// Maps a physical MMIO range on Linux through `/dev/mem`.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the requested physical range is a valid
    /// MMIO window and that accessing it with volatile loads/stores will not
    /// violate platform memory attributes or device ownership rules.
    #[cfg(all(feature = "std", target_os = "linux"))]
    pub unsafe fn new(phys_addr: u64, size: usize) -> Result<Self, InternalError> {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_SYNC)
            .open("/dev/mem")
            .map_err(|_| InternalError::MemoryMap {
                address: phys_addr,
                size,
            })?;

        // SAFETY: sysconf is thread-safe and does not touch Rust-managed memory.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let page_mask = page_size - 1;
        let offset = (phys_addr as usize) & page_mask;
        let aligned_addr = phys_addr & !(page_mask as u64);
        let aligned_size = size + offset;
        let map_size = (aligned_size + page_mask) & !page_mask;

        // SAFETY: arguments are derived from the requested physical range; the
        // caller is responsible for only mapping valid MMIO.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                aligned_addr as libc::off_t,
            )
        };

        if ptr == libc::MAP_FAILED {
            return Err(InternalError::MemoryMap {
                address: phys_addr,
                size,
            });
        }

        // SAFETY: `offset` is within the mapped page-aligned range.
        let adjusted_ptr = unsafe { (ptr as *mut u8).add(offset) };

        Ok(Self {
            ptr: adjusted_ptr,
            size,
            map_size,
            phys_addr,
        })
    }

    /// Uses direct physical-address MMIO for firmware/embedded builds.
    ///
    /// # Safety
    ///
    /// The caller must ensure this physical range is valid, already mapped
    /// with suitable device memory attributes, and exclusively owned for the
    /// volatile accesses performed through this object.
    #[cfg(not(feature = "std"))]
    pub unsafe fn new(phys_addr: u64, size: usize) -> Result<Self, InternalError> {
        if phys_addr == 0 || size == 0 {
            return Err(InternalError::MemoryMap {
                address: phys_addr,
                size,
            });
        }

        Ok(Self {
            ptr: phys_addr as *mut u8,
            size,
            phys_addr,
        })
    }

    /// Reports unsupported physical mapping on non-Linux std targets.
    ///
    /// # Safety
    ///
    /// This target does not create a mapping, but the constructor is unsafe to
    /// keep the API consistent with targets that do.
    #[cfg(all(feature = "std", not(target_os = "linux")))]
    pub unsafe fn new(phys_addr: u64, size: usize) -> Result<Self, InternalError> {
        let _ = (phys_addr, size);
        Err(InternalError::NotSupported(
            "Physical memory mapping not available on this target",
        ))
    }

    /// Reads an 8-bit value from the mapped region.
    #[inline(always)]
    pub fn read8(&self, offset: usize) -> u8 {
        assert!(offset < self.size, "8-bit MMIO read out of range");
        // SAFETY: `ptr` points at a valid MMIO window for this mapping.
        unsafe { core::ptr::read_volatile(self.ptr.add(offset)) }
    }

    /// Reads a 16-bit value from the mapped region.
    #[inline(always)]
    pub fn read16(&self, offset: usize) -> u16 {
        assert!(
            offset <= self.size.saturating_sub(2),
            "16-bit MMIO read out of range"
        );
        assert!(offset & 1 == 0, "unaligned 16-bit MMIO read");
        // SAFETY: bounds/alignment are checked above; MMIO validity is a
        // constructor invariant.
        unsafe { core::ptr::read_volatile(self.ptr.add(offset) as *const u16) }
    }

    /// Reads a 32-bit value from the mapped region.
    #[inline(always)]
    pub fn read32(&self, offset: usize) -> u32 {
        assert!(
            offset <= self.size.saturating_sub(4),
            "32-bit MMIO read out of range"
        );
        assert!(offset & 3 == 0, "unaligned 32-bit MMIO read");
        // SAFETY: bounds/alignment are checked above; MMIO validity is a
        // constructor invariant.
        unsafe { core::ptr::read_volatile(self.ptr.add(offset) as *const u32) }
    }

    /// Writes an 8-bit value to the mapped region.
    #[inline(always)]
    pub fn write8(&self, offset: usize, value: u8) {
        assert!(offset < self.size, "8-bit MMIO write out of range");
        // SAFETY: `ptr` points at a valid MMIO window for this mapping.
        unsafe {
            core::ptr::write_volatile(self.ptr.add(offset), value);
        }
    }

    /// Writes a 16-bit value to the mapped region.
    #[inline(always)]
    pub fn write16(&self, offset: usize, value: u16) {
        assert!(
            offset <= self.size.saturating_sub(2),
            "16-bit MMIO write out of range"
        );
        assert!(offset & 1 == 0, "unaligned 16-bit MMIO write");
        // SAFETY: bounds/alignment are checked above; MMIO validity is a
        // constructor invariant.
        unsafe {
            core::ptr::write_volatile(self.ptr.add(offset) as *mut u16, value);
        }
    }

    /// Writes a 32-bit value to the mapped region.
    #[inline(always)]
    pub fn write32(&self, offset: usize, value: u32) {
        assert!(
            offset <= self.size.saturating_sub(4),
            "32-bit MMIO write out of range"
        );
        assert!(offset & 3 == 0, "unaligned 32-bit MMIO write");
        // SAFETY: bounds/alignment are checked above; MMIO validity is a
        // constructor invariant.
        unsafe {
            core::ptr::write_volatile(self.ptr.add(offset) as *mut u32, value);
        }
    }

    /// Returns the physical address of this mapping.
    pub fn phys_addr(&self) -> u64 {
        self.phys_addr
    }

    /// Returns the size of this mapping.
    pub fn size(&self) -> usize {
        self.size
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl Drop for PhysMap {
    fn drop(&mut self) {
        // SAFETY: sysconf is thread-safe and does not touch Rust-managed memory.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let page_mask = page_size - 1;
        let offset = (self.phys_addr as usize) & page_mask;
        // SAFETY: `ptr` was adjusted by exactly this offset in `new`.
        let original_ptr = unsafe { self.ptr.sub(offset) };

        // SAFETY: this unmaps the same range created by mmap in `new`.
        unsafe {
            libc::munmap(original_ptr as *mut libc::c_void, self.map_size);
        }
    }
}

// Send + Sync are safe because these pointers refer to MMIO registers, not
// Rust-owned memory. Hardware/register-level synchronization is the caller's
// responsibility.
unsafe impl Send for PhysMap {}
unsafe impl Sync for PhysMap {}

impl crate::host::MmioAccess for PhysMap {
    fn read8(&self, offset: usize) -> u8 {
        self.read8(offset)
    }

    fn read16(&self, offset: usize) -> u16 {
        self.read16(offset)
    }

    fn read32(&self, offset: usize) -> u32 {
        self.read32(offset)
    }

    fn write8(&self, offset: usize, value: u8) {
        self.write8(offset, value);
    }

    fn write16(&self, offset: usize, value: u16) {
        self.write16(offset, value);
    }

    fn write32(&self, offset: usize, value: u32) {
        self.write32(offset, value);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn test_physmap_create() {
        // Requires root and /dev/mem on Linux or a mapped MMIO address in firmware.
    }
}

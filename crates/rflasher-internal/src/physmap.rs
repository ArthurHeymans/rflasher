//! Physical memory mapping for MMIO access
//!
//! This module provides safe wrappers around physical memory mapping
//! using /dev/mem on Linux. This is required to access the SPI controller
//! registers in the chipset.
//!
//! # Safety
//!
//! Accessing physical memory is inherently unsafe and requires root privileges.
//! The mapping functions ensure proper alignment and size constraints.

use crate::error::InternalError;

/// A mapped region of physical memory
#[cfg(all(feature = "std", target_os = "linux"))]
pub struct PhysMap {
    /// Pointer to the mapped memory
    ptr: *mut u8,
    /// Size of the mapping
    size: usize,
    /// Physical address (for error reporting)
    phys_addr: u64,
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl PhysMap {
    /// Map a region of physical memory for MMIO access
    ///
    /// # Arguments
    ///
    /// * `phys_addr` - Physical address to map
    /// * `size` - Size of the region to map
    ///
    /// # Safety
    ///
    /// The caller must ensure that:
    /// - The physical address range is valid and safe to access
    /// - No other code is accessing the same region
    /// - The region corresponds to MMIO registers (not RAM)
    pub fn new(phys_addr: u64, size: usize) -> Result<Self, InternalError> {
        use std::fs::OpenOptions;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        // Open /dev/mem with O_SYNC for uncached access (required for MMIO)
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_SYNC)
            .open("/dev/mem")
            .map_err(|_| InternalError::MemoryMap {
                address: phys_addr,
                size,
            })?;

        // Calculate page-aligned address and offset
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let page_mask = page_size - 1;
        let offset = (phys_addr as usize) & page_mask;
        let aligned_addr = phys_addr & !(page_mask as u64);
        let aligned_size = size + offset;

        // Round up size to page boundary
        let map_size = (aligned_size + page_mask) & !page_mask;

        // mmap the region
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

        // Adjust pointer to account for page alignment offset
        let adjusted_ptr = unsafe { (ptr as *mut u8).add(offset) };

        Ok(Self {
            ptr: adjusted_ptr,
            size: map_size,
            phys_addr,
        })
    }

    /// Read an 8-bit value from the mapped region
    ///
    /// # Safety
    ///
    /// The offset must be within the mapped region.
    #[inline]
    pub fn read8(&self, offset: usize) -> u8 {
        debug_assert!(offset < self.size);
        unsafe {
            core::ptr::read_volatile(self.ptr.add(offset))
        }
    }

    /// Read a 16-bit value from the mapped region
    ///
    /// # Safety
    ///
    /// The offset must be within the mapped region and properly aligned.
    #[inline]
    pub fn read16(&self, offset: usize) -> u16 {
        debug_assert!(offset + 2 <= self.size);
        debug_assert!(offset & 1 == 0, "unaligned 16-bit read");
        unsafe {
            core::ptr::read_volatile(self.ptr.add(offset) as *const u16)
        }
    }

    /// Read a 32-bit value from the mapped region
    ///
    /// # Safety
    ///
    /// The offset must be within the mapped region and properly aligned.
    #[inline]
    pub fn read32(&self, offset: usize) -> u32 {
        debug_assert!(offset + 4 <= self.size);
        debug_assert!(offset & 3 == 0, "unaligned 32-bit read");
        unsafe {
            core::ptr::read_volatile(self.ptr.add(offset) as *const u32)
        }
    }

    /// Write an 8-bit value to the mapped region
    ///
    /// # Safety
    ///
    /// The offset must be within the mapped region.
    #[inline]
    pub fn write8(&self, offset: usize, value: u8) {
        debug_assert!(offset < self.size);
        unsafe {
            core::ptr::write_volatile(self.ptr.add(offset), value);
        }
    }

    /// Write a 16-bit value to the mapped region
    ///
    /// # Safety
    ///
    /// The offset must be within the mapped region and properly aligned.
    #[inline]
    pub fn write16(&self, offset: usize, value: u16) {
        debug_assert!(offset + 2 <= self.size);
        debug_assert!(offset & 1 == 0, "unaligned 16-bit write");
        unsafe {
            core::ptr::write_volatile(self.ptr.add(offset) as *mut u16, value);
        }
    }

    /// Write a 32-bit value to the mapped region
    ///
    /// # Safety
    ///
    /// The offset must be within the mapped region and properly aligned.
    #[inline]
    pub fn write32(&self, offset: usize, value: u32) {
        debug_assert!(offset + 4 <= self.size);
        debug_assert!(offset & 3 == 0, "unaligned 32-bit write");
        unsafe {
            core::ptr::write_volatile(self.ptr.add(offset) as *mut u32, value);
        }
    }

    /// Get the physical address of this mapping
    pub fn phys_addr(&self) -> u64 {
        self.phys_addr
    }

    /// Get the size of this mapping
    pub fn size(&self) -> usize {
        self.size
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl Drop for PhysMap {
    fn drop(&mut self) {
        // Calculate the original mmap pointer (before offset adjustment)
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let page_mask = page_size - 1;
        let offset = (self.phys_addr as usize) & page_mask;
        let original_ptr = unsafe { self.ptr.sub(offset) };

        unsafe {
            libc::munmap(original_ptr as *mut libc::c_void, self.size);
        }
    }
}

// Send + Sync are safe because we're accessing MMIO registers which
// don't have the usual memory aliasing concerns
#[cfg(all(feature = "std", target_os = "linux"))]
unsafe impl Send for PhysMap {}
#[cfg(all(feature = "std", target_os = "linux"))]
unsafe impl Sync for PhysMap {}

// Stub for non-Linux platforms
#[cfg(not(all(feature = "std", target_os = "linux")))]
pub struct PhysMap {
    _private: (),
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
impl PhysMap {
    pub fn new(phys_addr: u64, size: usize) -> Result<Self, InternalError> {
        Err(InternalError::NotSupported("Physical memory mapping only supported on Linux"))
    }

    pub fn read8(&self, _offset: usize) -> u8 { 0 }
    pub fn read16(&self, _offset: usize) -> u16 { 0 }
    pub fn read32(&self, _offset: usize) -> u32 { 0 }
    pub fn write8(&self, _offset: usize, _value: u8) {}
    pub fn write16(&self, _offset: usize, _value: u16) {}
    pub fn write32(&self, _offset: usize, _value: u32) {}
    pub fn phys_addr(&self) -> u64 { 0 }
    pub fn size(&self) -> usize { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires root and /dev/mem access
    fn test_physmap_create() {
        // This test would need to map a safe address
        // For now, just verify the struct compiles
    }
}

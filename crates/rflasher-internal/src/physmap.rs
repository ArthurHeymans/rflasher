//! Physical memory mapping for MMIO access.
//!
//! Linux userspace maps controller registers through `/dev/mem`. Firmware
//! builds use direct physical-address MMIO and rely on the caller/platform to
//! have installed suitable mappings and memory attributes.

use crate::error::InternalError;
#[cfg(all(feature = "std", target_os = "linux"))]
use crate::error::RestrictedResource;

#[cfg(all(feature = "std", target_os = "linux"))]
fn map_physmap_error(error: std::io::Error, address: u64, size: usize) -> InternalError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        InternalError::PermissionDenied {
            resource: RestrictedResource::DevMem { address, size },
        }
    } else {
        InternalError::MemoryMap { address, size }
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
fn compute_mmap_range(
    phys_addr: u64,
    size: usize,
    page_size: libc::c_long,
) -> Option<(libc::off_t, usize, usize)> {
    if size == 0 || page_size <= 0 {
        return None;
    }
    let page_size = usize::try_from(page_size).ok()?;
    if !page_size.is_power_of_two() {
        return None;
    }
    // Ensure `[phys_addr, phys_addr + size)` does not wrap 64-bit physical space.
    let size_u64 = u64::try_from(size).ok()?;
    phys_addr.checked_add(size_u64 - 1)?;

    let page_mask = page_size - 1;
    let offset = (phys_addr & (page_mask as u64)) as usize;
    let aligned_addr = phys_addr & !(page_mask as u64);
    let aligned_off = libc::off_t::try_from(aligned_addr).ok()?;
    let aligned_size = size.checked_add(offset)?;
    let map_size = aligned_size.checked_add(page_mask)? & !page_mask;
    if map_size == 0 || map_size > isize::MAX as usize {
        return None;
    }
    Some((aligned_off, offset, map_size))
}

#[cfg(any(not(feature = "std"), test))]
fn validate_firmware_range(phys_addr: u64, size: usize) -> Option<usize> {
    if phys_addr == 0 || size == 0 || size > isize::MAX as usize {
        return None;
    }
    let base = usize::try_from(phys_addr).ok()?;
    base.checked_add(size)?;
    Some(base)
}

#[inline(always)]
fn checked_mmio_ptr<T>(base: *mut u8, size: usize, offset: usize) -> *mut T {
    let end = offset
        .checked_add(core::mem::size_of::<T>())
        .expect("MMIO offset overflow");
    assert!(end <= size, "MMIO access out of range");
    // Use `wrapping_add` rather than `add`: on firmware targets `base` is an
    // integer-derived MMIO address rather than a Rust allocation, so `ptr::add`'s
    // in-allocated-object precondition does not apply. Bounds were verified above.
    let ptr = base.wrapping_add(offset) as *mut T;
    assert!(ptr.is_aligned(), "unaligned MMIO access");
    ptr
}

/// A mapped region of physical memory.
pub struct PhysMap {
    /// Pointer to the mapped memory (adjusted into the requested window).
    ptr: *mut u8,
    /// Requested window size.
    size: usize,
    /// Actual mapped size, after host page alignment.
    #[cfg(all(feature = "std", target_os = "linux"))]
    map_size: usize,
    /// Base pointer returned by mmap, before page-offset adjustment.
    #[cfg(all(feature = "std", target_os = "linux"))]
    map_ptr: *mut libc::c_void,
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

        // SAFETY: sysconf is thread-safe and does not touch Rust-managed memory.
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let (aligned_off, offset, map_size) = compute_mmap_range(phys_addr, size, page_size)
            .ok_or(InternalError::MemoryMap {
                address: phys_addr,
                size,
            })?;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_SYNC)
            .open("/dev/mem")
            .map_err(|error| map_physmap_error(error, phys_addr, size))?;

        // SAFETY: arguments are derived from the requested physical range; the
        // caller is responsible for only mapping valid MMIO.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                aligned_off,
            )
        };

        if ptr == libc::MAP_FAILED {
            return Err(map_physmap_error(
                std::io::Error::last_os_error(),
                phys_addr,
                size,
            ));
        }

        // SAFETY: `offset` is within the mapped page-aligned range.
        let adjusted_ptr = unsafe { (ptr as *mut u8).add(offset) };

        Ok(Self {
            ptr: adjusted_ptr,
            size,
            map_size,
            map_ptr: ptr,
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
        let base = validate_firmware_range(phys_addr, size).ok_or(InternalError::MemoryMap {
            address: phys_addr,
            size,
        })?;

        Ok(Self {
            ptr: base as *mut u8,
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

    #[inline(always)]
    fn ptr<T>(&self, offset: usize) -> *mut T {
        checked_mmio_ptr::<T>(self.ptr, self.size, offset)
    }

    /// Reads an 8-bit value from the mapped region.
    #[inline(always)]
    pub fn read8(&self, offset: usize) -> u8 {
        let ptr = self.ptr::<u8>(offset);
        // SAFETY: bounds and alignment are checked by `ptr`; MMIO validity is
        // a constructor invariant.
        unsafe { core::ptr::read_volatile(ptr) }
    }

    /// Reads a 16-bit value from the mapped region.
    #[inline(always)]
    pub fn read16(&self, offset: usize) -> u16 {
        let ptr = self.ptr::<u16>(offset);
        // SAFETY: bounds and alignment are checked by `ptr`; MMIO validity is
        // a constructor invariant.
        unsafe { core::ptr::read_volatile(ptr) }
    }

    /// Reads a 32-bit value from the mapped region.
    #[inline(always)]
    pub fn read32(&self, offset: usize) -> u32 {
        let ptr = self.ptr::<u32>(offset);
        // SAFETY: bounds and alignment are checked by `ptr`; MMIO validity is
        // a constructor invariant.
        unsafe { core::ptr::read_volatile(ptr) }
    }

    /// Reads an aligned 64-bit value from a mapped data window.
    #[inline(always)]
    pub fn read64(&self, offset: usize) -> u64 {
        let ptr = self.ptr::<u64>(offset);
        // SAFETY: bounds and alignment are checked by `ptr`; MMIO validity is
        // a constructor invariant. Callers use this only for flash data windows.
        unsafe { core::ptr::read_volatile(ptr) }
    }

    /// Writes an 8-bit value to the mapped region.
    #[inline(always)]
    pub fn write8(&self, offset: usize, value: u8) {
        let ptr = self.ptr::<u8>(offset);
        // SAFETY: bounds and alignment are checked by `ptr`; MMIO validity is
        // a constructor invariant.
        unsafe {
            core::ptr::write_volatile(ptr, value);
        }
    }

    /// Writes a 16-bit value to the mapped region.
    #[inline(always)]
    pub fn write16(&self, offset: usize, value: u16) {
        let ptr = self.ptr::<u16>(offset);
        // SAFETY: bounds and alignment are checked by `ptr`; MMIO validity is
        // a constructor invariant.
        unsafe {
            core::ptr::write_volatile(ptr, value);
        }
    }

    /// Writes a 32-bit value to the mapped region.
    #[inline(always)]
    pub fn write32(&self, offset: usize, value: u32) {
        let ptr = self.ptr::<u32>(offset);
        // SAFETY: bounds and alignment are checked by `ptr`; MMIO validity is
        // a constructor invariant.
        unsafe {
            core::ptr::write_volatile(ptr, value);
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
        // SAFETY: this unmaps the exact range created by mmap in `new`,
        // using the base pointer stored at construction time.
        unsafe {
            libc::munmap(self.map_ptr, self.map_size);
        }
    }
}

// Moving an owned MMIO mapping between threads does not create concurrent
// register access. `PhysMap` is deliberately not `Sync`: shared unsynchronized
// access to stateful controller registers is unsafe.
unsafe impl Send for PhysMap {}

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

    fn read64(&self, offset: usize) -> u64 {
        self.read64(offset)
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
    use super::{checked_mmio_ptr, validate_firmware_range};
    #[cfg(all(feature = "std", target_os = "linux"))]
    use super::{compute_mmap_range, map_physmap_error};
    #[cfg(all(feature = "std", target_os = "linux"))]
    use crate::error::{InternalError, RestrictedResource};

    #[cfg(all(feature = "std", target_os = "linux"))]
    #[test]
    fn permission_errors_retain_the_requested_mapping_range() {
        let error = map_physmap_error(
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            0xfedc_0000,
            8192,
        );

        assert!(matches!(
            error,
            InternalError::PermissionDenied {
                resource: RestrictedResource::DevMem {
                    address: 0xfedc_0000,
                    size: 8192,
                }
            }
        ));
    }

    #[cfg(all(feature = "std", target_os = "linux"))]
    #[test]
    fn non_permission_errors_remain_memory_map_failures() {
        let error = map_physmap_error(
            std::io::Error::from(std::io::ErrorKind::InvalidInput),
            0xfedc_0000,
            4096,
        );

        assert!(matches!(
            error,
            InternalError::MemoryMap {
                address: 0xfedc_0000,
                size: 4096,
            }
        ));
    }

    #[cfg(all(feature = "std", target_os = "linux"))]
    #[test]
    fn compute_mmap_range_handles_unaligned_and_invalid_inputs() {
        assert_eq!(
            compute_mmap_range(0xfedc_3800, 0x200, 4096),
            Some((0xfedc_3000, 0x800, 4096))
        );
        assert_eq!(
            compute_mmap_range(0xfedc_3f00, 0x200, 4096),
            Some((0xfedc_3000, 0xf00, 8192))
        );
        // Zero size, invalid page size, overflow, and negative off_t rejected.
        assert_eq!(compute_mmap_range(0xfedc_0000, 0, 4096), None);
        assert_eq!(compute_mmap_range(0xfedc_0000, 4096, -1), None);
        assert_eq!(compute_mmap_range(0xfedc_0000, 4096, 0), None);
        assert_eq!(compute_mmap_range(0xfedc_0000, 4096, 3000), None);
        assert_eq!(compute_mmap_range(0xfedc_0000, usize::MAX, 4096), None);
        assert_eq!(compute_mmap_range(u64::MAX, 4, 4096), None);
        assert_eq!(compute_mmap_range(u64::MAX - 4095, 4096, 4096), None);
    }

    #[test]
    fn validate_firmware_range_checks_bounds() {
        assert_eq!(validate_firmware_range(0, 4096), None);
        assert_eq!(validate_firmware_range(0xfedc_0000, 0), None);
        assert_eq!(validate_firmware_range(u64::MAX, 16), None);
        assert_eq!(
            validate_firmware_range(0xfedc_0000, 4096),
            Some(0xfedc_0000)
        );
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn checked_mmio_ptr_rejects_short_region_for_u16() {
        let mut buf = [0u64; 1];
        let _ = checked_mmio_ptr::<u16>(buf.as_mut_ptr().cast(), 1, 0);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn checked_mmio_ptr_rejects_short_region_for_u32() {
        let mut buf = [0u64; 1];
        let _ = checked_mmio_ptr::<u32>(buf.as_mut_ptr().cast(), 3, 0);
    }

    #[test]
    #[should_panic(expected = "overflow")]
    fn checked_mmio_ptr_rejects_offset_overflow() {
        let mut buf = [0u64; 2];
        let _ = checked_mmio_ptr::<u32>(buf.as_mut_ptr().cast(), 16, usize::MAX);
    }

    #[test]
    #[should_panic(expected = "unaligned")]
    fn checked_mmio_ptr_checks_effective_pointer_alignment() {
        let mut buf = [0u64; 2];
        // Base pointer itself is misaligned by 1 byte, even though offset == 0.
        let unaligned_base = unsafe { buf.as_mut_ptr().cast::<u8>().add(1) };
        let _ = checked_mmio_ptr::<u64>(unaligned_base, 8, 0);
    }

    #[test]
    fn checked_mmio_ptr_accepts_aligned_in_bounds_access() {
        let mut buf = [0u64; 2];
        let base = buf.as_mut_ptr().cast::<u8>();
        let p64 = checked_mmio_ptr::<u64>(base, 16, 8);
        assert!(p64.is_aligned());
        let p32 = checked_mmio_ptr::<u32>(base, 16, 12);
        assert!(p32.is_aligned());
        let p16 = checked_mmio_ptr::<u16>(base, 16, 14);
        assert!(p16.is_aligned());
    }
}

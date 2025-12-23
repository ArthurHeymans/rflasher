//! Unified flash operations that work with any FlashDevice
//!
//! This module provides high-level operations (smart write, layout-based
//! operations, verification) that work with any type implementing the
//! `FlashDevice` trait.

use alloc::vec;
use alloc::vec::Vec;

use crate::chip::WriteGranularity;
use crate::error::{Error, Result};
use crate::flash::device::FlashDevice;
use crate::layout::{Layout, LayoutError, Region};

// =============================================================================
// Constants
// =============================================================================

/// The erased value for flash memory (all bits set)
const ERASED_VALUE: u8 = 0xFF;

/// Default read chunk size
const READ_CHUNK_SIZE: usize = 4096;

// =============================================================================
// Smart write support types
// =============================================================================

/// Determine if an erase is required to transition from `have` to `want`
///
/// Flash memory can only change bits from 1 to 0 during writes. To change
/// bits from 0 to 1, an erase is required (which sets all bits to 1).
pub fn need_erase(have: &[u8], want: &[u8], granularity: WriteGranularity) -> bool {
    assert_eq!(have.len(), want.len());

    match granularity {
        WriteGranularity::Bit => {
            // For bit-granularity, we can only clear bits (1->0).
            // We need erase if any bit needs to go from 0->1
            have.iter().zip(want.iter()).any(|(h, w)| (h & w) != *w)
        }
        WriteGranularity::Byte | WriteGranularity::Page => {
            // For byte/page granularity, if bytes differ, the old byte must be
            // in erased state (0xFF) to allow writing the new value
            have.iter().zip(want.iter()).any(|(h, w)| {
                if h == w {
                    false // No change needed
                } else {
                    *h != ERASED_VALUE // Need erase if not already erased
                }
            })
        }
    }
}

/// Check if a range of data needs to be written (differs from current contents)
#[inline]
pub fn need_write(have: &[u8], want: &[u8]) -> bool {
    have != want
}

/// A contiguous range of bytes that needs to be written
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteRange {
    /// Start offset within the compared buffers
    pub start: u32,
    /// Length in bytes
    pub len: u32,
}

/// Find all contiguous ranges of changed bytes
pub fn get_all_write_ranges(have: &[u8], want: &[u8]) -> Vec<WriteRange> {
    assert_eq!(have.len(), want.len());

    let mut ranges = Vec::new();
    let mut i = 0;

    while i < have.len() {
        // Find start of changed region
        while i < have.len() && have[i] == want[i] {
            i += 1;
        }
        if i >= have.len() {
            break;
        }

        let start = i;

        // Find end of changed region
        while i < have.len() && have[i] != want[i] {
            i += 1;
        }

        ranges.push(WriteRange {
            start: start as u32,
            len: (i - start) as u32,
        });
    }

    ranges
}

/// Statistics from a smart write operation
#[derive(Debug, Clone, Default)]
pub struct WriteStats {
    /// Number of bytes that were different
    pub bytes_changed: usize,
    /// Number of erase operations performed
    pub erases_performed: usize,
    /// Total bytes erased
    pub bytes_erased: usize,
    /// Number of write operations performed
    pub writes_performed: usize,
    /// Total bytes written
    pub bytes_written: usize,
    /// Whether any flash operations were performed
    pub flash_modified: bool,
}

/// Callback for progress reporting during operations
pub trait WriteProgress {
    /// Called when starting to read current flash contents
    fn reading(&mut self, total_bytes: usize);

    /// Called to update read progress
    fn read_progress(&mut self, bytes_read: usize);

    /// Called when starting erase operations
    fn erasing(&mut self, blocks_to_erase: usize, bytes_to_erase: usize);

    /// Called after each block is erased
    fn erase_progress(&mut self, blocks_erased: usize, bytes_erased: usize);

    /// Called when starting write operations
    fn writing(&mut self, bytes_to_write: usize);

    /// Called to update write progress
    fn write_progress(&mut self, bytes_written: usize);

    /// Called when the operation is complete
    fn complete(&mut self, stats: &WriteStats);
}

/// A no-op progress reporter
pub struct NoProgress;

impl WriteProgress for NoProgress {
    fn reading(&mut self, _total_bytes: usize) {}
    fn read_progress(&mut self, _bytes_read: usize) {}
    fn erasing(&mut self, _blocks_to_erase: usize, _bytes_to_erase: usize) {}
    fn erase_progress(&mut self, _blocks_erased: usize, _bytes_erased: usize) {}
    fn writing(&mut self, _bytes_to_write: usize) {}
    fn write_progress(&mut self, _bytes_written: usize) {}
    fn complete(&mut self, _stats: &WriteStats) {}
}

// =============================================================================
// Block planning
// =============================================================================

/// Information about a block that may need to be erased
#[derive(Debug, Clone)]
struct BlockPlan {
    /// Start address of the block
    start: u32,
    /// Size of the block
    size: u32,
    /// Whether this block needs to be erased
    needs_erase: bool,
    /// Whether this block needs to be written
    needs_write: bool,
}

/// Plan erase and write operations for a data range
fn plan_blocks(
    have: &[u8],
    want: &[u8],
    base_addr: u32,
    block_size: u32,
    granularity: WriteGranularity,
) -> Vec<BlockPlan> {
    assert_eq!(have.len(), want.len());

    let total_len = have.len() as u32;
    let mut plans = Vec::new();

    // Calculate the first block boundary at or before base_addr
    let first_block_start = (base_addr / block_size) * block_size;
    let mut current_addr = first_block_start;

    while current_addr < base_addr + total_len {
        let block_end = current_addr + block_size;

        // Calculate the overlap between this block and our data
        let overlap_start = current_addr.max(base_addr);
        let overlap_end = block_end.min(base_addr + total_len);

        if overlap_start >= overlap_end {
            current_addr = block_end;
            continue;
        }

        // Convert to buffer indices
        let buf_start = (overlap_start - base_addr) as usize;
        let buf_end = (overlap_end - base_addr) as usize;

        let have_slice = &have[buf_start..buf_end];
        let want_slice = &want[buf_start..buf_end];

        let needs_write = need_write(have_slice, want_slice);
        let needs_erase = needs_write && need_erase(have_slice, want_slice, granularity);

        if needs_write {
            plans.push(BlockPlan {
                start: current_addr,
                size: block_size,
                needs_erase,
                needs_write,
            });
        }

        current_addr = block_end;
    }

    plans
}

// =============================================================================
// Unified operations
// =============================================================================

/// Read flash contents into a buffer
///
/// This is a convenience function that reads with progress reporting.
pub fn read_with_progress<D: FlashDevice, P: WriteProgress>(
    device: &mut D,
    buf: &mut [u8],
    progress: &mut P,
) -> Result<()> {
    let total = buf.len();
    progress.reading(total);

    let mut bytes_read = 0;
    while bytes_read < total {
        let chunk_size = core::cmp::min(READ_CHUNK_SIZE, total - bytes_read);
        device.read(
            bytes_read as u32,
            &mut buf[bytes_read..bytes_read + chunk_size],
        )?;
        bytes_read += chunk_size;
        progress.read_progress(bytes_read);
    }

    Ok(())
}

/// Perform a smart write operation that minimizes flash operations
///
/// This function compares the current flash contents with the desired contents
/// and only erases/writes the regions that actually need to change.
///
/// # Algorithm
/// 1. Read current flash contents
/// 2. Compare with desired contents to find changed blocks
/// 3. For each changed block, determine if erase is needed
/// 4. Erase only the blocks that need erasing
/// 5. Write only the bytes that are different
///
/// # Arguments
/// * `device` - Flash device to write to
/// * `data` - Desired flash contents (must match device size)
/// * `progress` - Progress callback
///
/// # Returns
/// Statistics about the operations performed
pub fn smart_write<D: FlashDevice, P: WriteProgress>(
    device: &mut D,
    data: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    let flash_size = device.size() as usize;

    if data.len() != flash_size {
        return Err(Error::BufferTooSmall);
    }

    let block_size = device.erase_granularity();
    let granularity = device.write_granularity();

    let mut stats = WriteStats::default();

    // Step 1: Read current flash contents
    progress.reading(flash_size);
    let mut current = vec![0u8; flash_size];

    let mut bytes_read = 0;
    while bytes_read < flash_size {
        let chunk_size = core::cmp::min(READ_CHUNK_SIZE, flash_size - bytes_read);
        device.read(
            bytes_read as u32,
            &mut current[bytes_read..bytes_read + chunk_size],
        )?;
        bytes_read += chunk_size;
        progress.read_progress(bytes_read);
    }

    // Step 2: Plan block operations
    let plans = plan_blocks(&current, data, 0, block_size, granularity);

    if plans.is_empty() {
        // Nothing to do - flash already matches
        progress.complete(&stats);
        return Ok(stats);
    }

    // Calculate statistics
    stats.bytes_changed = get_all_write_ranges(&current, data)
        .iter()
        .map(|r| r.len as usize)
        .sum();

    let blocks_to_erase: Vec<_> = plans.iter().filter(|p| p.needs_erase).collect();
    let _blocks_to_write: Vec<_> = plans.iter().filter(|p| p.needs_write).collect();

    // Step 3: Erase blocks that need it
    if !blocks_to_erase.is_empty() {
        let bytes_to_erase: usize = blocks_to_erase.iter().map(|b| b.size as usize).sum();
        progress.erasing(blocks_to_erase.len(), bytes_to_erase);

        for (i, block) in blocks_to_erase.iter().enumerate() {
            device.erase(block.start, block.size)?;

            // Update our view of current contents
            let buf_start = block.start as usize;
            let buf_end = (block.start + block.size) as usize;
            if buf_end <= current.len() {
                for byte in &mut current[buf_start..buf_end] {
                    *byte = ERASED_VALUE;
                }
            }

            stats.erases_performed += 1;
            stats.bytes_erased += block.size as usize;
            progress.erase_progress(i + 1, stats.bytes_erased);
        }
        stats.flash_modified = true;
    }

    // Step 4: Write blocks that differ
    // Re-calculate write ranges after erasing
    let write_ranges = get_all_write_ranges(&current, data);

    if !write_ranges.is_empty() {
        let bytes_to_write: usize = write_ranges.iter().map(|r| r.len as usize).sum();
        progress.writing(bytes_to_write);

        let mut bytes_written = 0;

        for range in &write_ranges {
            let write_data = &data[range.start as usize..(range.start + range.len) as usize];
            device.write(range.start, write_data)?;

            bytes_written += range.len as usize;
            progress.write_progress(bytes_written);
            stats.writes_performed += 1;
        }

        stats.bytes_written = bytes_written;
        stats.flash_modified = true;
    }

    progress.complete(&stats);
    Ok(stats)
}

/// Perform a smart write operation for a specific region
///
/// Similar to `smart_write` but only operates on a specific region of flash.
pub fn smart_write_region<D: FlashDevice, P: WriteProgress>(
    device: &mut D,
    addr: u32,
    data: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    if !device.is_valid_range(addr, data.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    let block_size = device.erase_granularity();
    let granularity = device.write_granularity();

    let mut stats = WriteStats::default();

    // Step 1: Read current contents of the region
    progress.reading(data.len());
    let mut current = vec![0u8; data.len()];

    let mut bytes_read = 0;
    while bytes_read < data.len() {
        let chunk_size = core::cmp::min(READ_CHUNK_SIZE, data.len() - bytes_read);
        device.read(
            addr + bytes_read as u32,
            &mut current[bytes_read..bytes_read + chunk_size],
        )?;
        bytes_read += chunk_size;
        progress.read_progress(bytes_read);
    }

    // Step 2: Plan block operations
    let plans = plan_blocks(&current, data, addr, block_size, granularity);

    if plans.is_empty() {
        progress.complete(&stats);
        return Ok(stats);
    }

    stats.bytes_changed = get_all_write_ranges(&current, data)
        .iter()
        .map(|r| r.len as usize)
        .sum();

    let blocks_to_erase: Vec<_> = plans.iter().filter(|p| p.needs_erase).collect();

    // Step 3: Erase blocks that need it
    if !blocks_to_erase.is_empty() {
        let bytes_to_erase: usize = blocks_to_erase.iter().map(|b| b.size as usize).sum();
        progress.erasing(blocks_to_erase.len(), bytes_to_erase);

        for (i, block) in blocks_to_erase.iter().enumerate() {
            // Handle data outside our region but inside the erase block
            let block_end = block.start + block.size;

            // Read data before our region (if block extends before)
            if block.start < addr {
                let preserve_len = (addr - block.start) as usize;
                let mut preserve_data = vec![0u8; preserve_len];
                device.read(block.start, &mut preserve_data)?;

                // Erase and restore
                device.erase(block.start, block.size)?;
                device.write(block.start, &preserve_data)?;
            }
            // Handle data after our region (if block extends after)
            else if block_end > addr + data.len() as u32 {
                let region_end = addr + data.len() as u32;
                let preserve_start = region_end;
                let preserve_len = (block_end - region_end) as usize;

                let mut preserve_data = vec![0u8; preserve_len];
                device.read(preserve_start, &mut preserve_data)?;

                device.erase(block.start, block.size)?;
                device.write(preserve_start, &preserve_data)?;
            } else {
                // Block is entirely within our region
                device.erase(block.start, block.size)?;
            }

            // Update our view of current contents
            let rel_start = block.start.saturating_sub(addr) as usize;
            let rel_end =
                ((block.start + block.size).saturating_sub(addr) as usize).min(current.len());
            for byte in &mut current[rel_start..rel_end] {
                *byte = ERASED_VALUE;
            }

            stats.erases_performed += 1;
            stats.bytes_erased += block.size as usize;
            progress.erase_progress(i + 1, stats.bytes_erased);
        }
        stats.flash_modified = true;
    }

    // Step 4: Write changed bytes
    let write_ranges = get_all_write_ranges(&current, data);

    if !write_ranges.is_empty() {
        let bytes_to_write: usize = write_ranges.iter().map(|r| r.len as usize).sum();
        progress.writing(bytes_to_write);

        let mut bytes_written = 0;

        for range in &write_ranges {
            let write_data = &data[range.start as usize..(range.start + range.len) as usize];
            device.write(addr + range.start, write_data)?;

            bytes_written += range.len as usize;
            progress.write_progress(bytes_written);
            stats.writes_performed += 1;
        }

        stats.bytes_written = bytes_written;
        stats.flash_modified = true;
    }

    progress.complete(&stats);
    Ok(stats)
}

/// Perform a smart write operation for all included regions in a layout
///
/// # Arguments
/// * `device` - Flash device to write to
/// * `layout` - Layout with regions marked as included
/// * `image` - Full flash image (must be at least device size)
/// * `progress` - Progress callback
///
/// # Returns
/// Combined statistics about all operations performed
pub fn smart_write_by_layout<D: FlashDevice, P: WriteProgress>(
    device: &mut D,
    layout: &Layout,
    image: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    let flash_size = device.size();

    // Validate layout against device
    layout.validate(flash_size).map_err(|e| match e {
        LayoutError::RegionOutOfBounds => Error::AddressOutOfBounds,
        LayoutError::ChipSizeMismatch { .. } => Error::AddressOutOfBounds,
        _ => Error::LayoutError,
    })?;

    // Image must cover the device
    if image.len() < flash_size as usize {
        return Err(Error::BufferTooSmall);
    }

    // Collect included regions
    let included: Vec<_> = layout.included_regions().collect();
    if included.is_empty() {
        let stats = WriteStats::default();
        progress.complete(&stats);
        return Ok(stats);
    }

    let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();
    let mut combined_stats = WriteStats::default();
    let mut overall_bytes_read = 0usize;

    // Report total reading
    progress.reading(total_bytes);

    // Process each region
    for region in &included {
        let region_data = &image[region.start as usize..=region.end as usize];

        // Create a wrapper progress that offsets the overall progress
        struct OffsetProgress<'a, P: WriteProgress> {
            inner: &'a mut P,
            read_offset: usize,
        }

        impl<P: WriteProgress> WriteProgress for OffsetProgress<'_, P> {
            fn reading(&mut self, _total_bytes: usize) {}
            fn read_progress(&mut self, bytes_read: usize) {
                self.inner.read_progress(self.read_offset + bytes_read);
            }
            fn erasing(&mut self, blocks_to_erase: usize, bytes_to_erase: usize) {
                self.inner.erasing(blocks_to_erase, bytes_to_erase);
            }
            fn erase_progress(&mut self, blocks_erased: usize, bytes_erased: usize) {
                self.inner.erase_progress(blocks_erased, bytes_erased);
            }
            fn writing(&mut self, bytes_to_write: usize) {
                self.inner.writing(bytes_to_write);
            }
            fn write_progress(&mut self, bytes_written: usize) {
                self.inner.write_progress(bytes_written);
            }
            fn complete(&mut self, _stats: &WriteStats) {}
        }

        let mut offset_progress = OffsetProgress {
            inner: progress,
            read_offset: overall_bytes_read,
        };

        let stats = smart_write_region(device, region.start, region_data, &mut offset_progress)?;

        // Accumulate stats
        combined_stats.bytes_changed += stats.bytes_changed;
        combined_stats.erases_performed += stats.erases_performed;
        combined_stats.bytes_erased += stats.bytes_erased;
        combined_stats.writes_performed += stats.writes_performed;
        combined_stats.bytes_written += stats.bytes_written;
        combined_stats.flash_modified |= stats.flash_modified;

        overall_bytes_read += region.size() as usize;
    }

    progress.complete(&combined_stats);
    Ok(combined_stats)
}

/// Read all included regions from flash into a buffer
///
/// Regions that are not included will be left unchanged in the buffer.
pub fn read_by_layout<D: FlashDevice>(
    device: &mut D,
    layout: &Layout,
    buffer: &mut [u8],
) -> Result<()> {
    let flash_size = device.size();

    // Validate layout against device
    layout.validate(flash_size).map_err(|e| match e {
        LayoutError::RegionOutOfBounds => Error::AddressOutOfBounds,
        LayoutError::ChipSizeMismatch { .. } => Error::AddressOutOfBounds,
        _ => Error::LayoutError,
    })?;

    if buffer.len() < flash_size as usize {
        return Err(Error::BufferTooSmall);
    }

    // Read each included region
    for region in layout.included_regions() {
        let region_buf = &mut buffer[region.start as usize..=region.end as usize];
        device.read(region.start, region_buf)?;
    }

    Ok(())
}

/// Erase all included regions in a layout
pub fn erase_by_layout<D: FlashDevice>(device: &mut D, layout: &Layout) -> Result<()> {
    let flash_size = device.size();

    layout.validate(flash_size).map_err(|e| match e {
        LayoutError::RegionOutOfBounds => Error::AddressOutOfBounds,
        LayoutError::ChipSizeMismatch { .. } => Error::AddressOutOfBounds,
        _ => Error::LayoutError,
    })?;

    for region in layout.included_regions() {
        erase_region(device, region)?;
    }

    Ok(())
}

/// Erase a single region
///
/// This handles region boundaries that don't align with erase block boundaries
/// by preserving data outside the region.
pub fn erase_region<D: FlashDevice>(device: &mut D, region: &Region) -> Result<()> {
    if !device.is_valid_range(region.start, region.size() as usize) {
        return Err(Error::AddressOutOfBounds);
    }

    let block_size = device.erase_granularity();

    // Calculate first and last blocks
    let first_block_start = (region.start / block_size) * block_size;
    let mut current_addr = first_block_start;

    while current_addr <= region.end {
        let block_end = current_addr + block_size - 1;
        let is_unaligned = current_addr < region.start || block_end > region.end;

        if is_unaligned {
            // Need to preserve data outside the region
            let mut backup = vec![ERASED_VALUE; block_size as usize];

            // Read data before region (to preserve)
            if region.start > current_addr {
                let len = (region.start - current_addr) as usize;
                device.read(current_addr, &mut backup[..len])?;
            }

            // Read data after region (to preserve)
            if block_end > region.end {
                let start = region.end + 1;
                let rel_start = (start - current_addr) as usize;
                let len = (block_end - region.end) as usize;
                device.read(start, &mut backup[rel_start..rel_start + len])?;
            }

            // Erase the block
            device.erase(current_addr, block_size)?;

            // Write back preserved data
            if region.start > current_addr {
                let len = (region.start - current_addr) as usize;
                device.write(current_addr, &backup[..len])?;
            }
            if block_end > region.end {
                let start = region.end + 1;
                let rel_start = (start - current_addr) as usize;
                let len = (block_end - region.end) as usize;
                device.write(start, &backup[rel_start..rel_start + len])?;
            }
        } else {
            // Block is aligned with region, just erase it
            device.erase(current_addr, block_size)?;
        }

        current_addr += block_size;
    }

    Ok(())
}

/// Verify flash contents match the expected data
///
/// # Arguments
/// * `device` - Flash device to verify
/// * `expected` - Expected data
/// * `addr` - Starting address (0 for full flash)
///
/// # Returns
/// `Ok(())` if verification passes, `Err(VerifyError)` if mismatch detected
pub fn verify<D: FlashDevice>(device: &mut D, expected: &[u8], addr: u32) -> Result<()> {
    if !device.is_valid_range(addr, expected.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    let mut buf = vec![0u8; READ_CHUNK_SIZE];
    let mut offset = 0usize;

    while offset < expected.len() {
        let chunk_size = core::cmp::min(READ_CHUNK_SIZE, expected.len() - offset);
        let chunk_buf = &mut buf[..chunk_size];
        device.read(addr + offset as u32, chunk_buf)?;

        let expected_chunk = &expected[offset..offset + chunk_size];
        if chunk_buf != expected_chunk {
            return Err(Error::VerifyError);
        }

        offset += chunk_size;
    }

    Ok(())
}

/// Verify all included regions match expected data
pub fn verify_by_layout<D: FlashDevice>(
    device: &mut D,
    layout: &Layout,
    expected: &[u8],
) -> Result<()> {
    let flash_size = device.size();

    layout.validate(flash_size).map_err(|e| match e {
        LayoutError::RegionOutOfBounds => Error::AddressOutOfBounds,
        LayoutError::ChipSizeMismatch { .. } => Error::AddressOutOfBounds,
        _ => Error::LayoutError,
    })?;

    if expected.len() < flash_size as usize {
        return Err(Error::BufferTooSmall);
    }

    for region in layout.included_regions() {
        let expected_region = &expected[region.start as usize..=region.end as usize];
        verify(device, expected_region, region.start)?;
    }

    Ok(())
}

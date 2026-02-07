//! Unified flash operations that work with any FlashDevice
//!
//! This module provides high-level operations (smart write, layout-based
//! operations, verification) that work with any type implementing the
//! `FlashDevice` trait.
//!
//! The smart write support types (`WriteStats`, `WriteProgress`, `NoProgress`,
//! `WriteRange`, `need_erase`, `need_write`, `get_all_write_ranges`) are
//! re-exported from `operations.rs` to avoid duplication.

use alloc::vec;
use alloc::vec::Vec;

use crate::error::{Error, Result};
use crate::flash::device::FlashDevice;
use crate::flash::operations::{plan_optimal_erase, plan_optimal_erase_region};
use crate::layout::{Layout, LayoutError, Region};
use maybe_async::maybe_async;

// =============================================================================
// Re-exports from operations.rs
// =============================================================================

// Re-export smart write support types from operations.rs
// These are the canonical definitions - no duplication needed
pub use crate::flash::operations::{
    get_all_write_ranges, need_write, NoProgress, WriteProgress, WriteRange, WriteStats,
};

// =============================================================================
// Constants
// =============================================================================

/// The erased value for flash memory (all bits set)
const ERASED_VALUE: u8 = 0xFF;

/// Default read chunk size
const READ_CHUNK_SIZE: usize = 4096;

// =============================================================================
// Unified operations
// =============================================================================

/// Read flash contents into a buffer
///
/// This is a convenience function that reads with progress reporting.
#[maybe_async]
pub async fn read_with_progress<D: FlashDevice, P: WriteProgress>(
    device: &mut D,
    buf: &mut [u8],
    progress: &mut P,
) -> Result<()> {
    let total = buf.len();
    progress.reading(total);

    let mut bytes_read = 0;
    while bytes_read < total {
        let chunk_size = core::cmp::min(READ_CHUNK_SIZE, total - bytes_read);
        device
            .read(
                bytes_read as u32,
                &mut buf[bytes_read..bytes_read + chunk_size],
            )
            .await?;
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
/// 2. Use optimal erase algorithm to plan erase operations (minimizes operations
///    by using larger erase blocks when >50% of sub-blocks need erasing)
/// 3. Erase only the blocks that need erasing
/// 4. Write only the bytes that are different
///
/// # Arguments
/// * `device` - Flash device to write to
/// * `data` - Desired flash contents (must match device size)
/// * `progress` - Progress callback
///
/// # Returns
/// Statistics about the operations performed
#[maybe_async]
pub async fn smart_write<D: FlashDevice + ?Sized, P: WriteProgress>(
    device: &mut D,
    data: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    let flash_size = device.size();

    if data.len() != flash_size as usize {
        return Err(Error::BufferTooSmall);
    }

    // Clone erase blocks to avoid borrow checker issues
    let erase_blocks: Vec<_> = device.erase_blocks().to_vec();
    let granularity = device.write_granularity();

    let mut stats = WriteStats::default();

    // Step 1: Read current flash contents
    progress.reading(flash_size as usize);
    let mut current = vec![0u8; flash_size as usize];

    let mut bytes_read = 0;
    while bytes_read < flash_size as usize {
        let chunk_size = core::cmp::min(READ_CHUNK_SIZE, flash_size as usize - bytes_read);
        device
            .read(
                bytes_read as u32,
                &mut current[bytes_read..bytes_read + chunk_size],
            )
            .await?;
        bytes_read += chunk_size;
        progress.read_progress(bytes_read);
    }

    // Check if any changes are needed
    if !need_write(&current, data) {
        // Nothing to do - flash already matches
        progress.complete(&stats);
        return Ok(stats);
    }

    // Calculate statistics
    stats.bytes_changed = get_all_write_ranges(&current, data)
        .iter()
        .map(|r| r.len as usize)
        .sum();

    // Step 2: Plan optimal erase operations
    // This uses the hierarchical algorithm that minimizes erase operations
    // by promoting to larger blocks when >50% of sub-blocks need erasing
    let erase_ops = plan_optimal_erase(
        &erase_blocks,
        flash_size,
        Some(&current),
        Some(data),
        0,
        flash_size - 1,
        granularity,
    );

    // Step 3: Erase blocks that need it
    if !erase_ops.is_empty() {
        let bytes_to_erase: usize = erase_ops.iter().map(|op| op.size as usize).sum();
        progress.erasing(erase_ops.len(), bytes_to_erase);

        for (i, op) in erase_ops.iter().enumerate() {
            device.erase(op.start, op.size).await?;

            // Update our view of current contents
            let buf_start = op.start as usize;
            let buf_end = (op.start + op.size) as usize;
            if buf_end <= current.len() {
                current[buf_start..buf_end].fill(ERASED_VALUE);
            }

            stats.erases_performed += 1;
            stats.bytes_erased += op.size as usize;
            progress.erase_progress(i + 1, stats.bytes_erased);
        }
        stats.flash_modified = true;
    }

    // Step 4: Write bytes that differ
    // Re-calculate write ranges after erasing
    let write_ranges = get_all_write_ranges(&current, data);

    if !write_ranges.is_empty() {
        let bytes_to_write: usize = write_ranges.iter().map(|r| r.len as usize).sum();
        progress.writing(bytes_to_write);

        let mut bytes_written = 0;

        for range in &write_ranges {
            let write_data = &data[range.start as usize..(range.start + range.len) as usize];
            device.write(range.start, write_data).await?;

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
/// Uses the optimal erase algorithm to minimize erase operations.
#[maybe_async]
pub async fn smart_write_region<D: FlashDevice + ?Sized, P: WriteProgress>(
    device: &mut D,
    addr: u32,
    data: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    if data.is_empty() {
        let stats = WriteStats::default();
        progress.complete(&stats);
        return Ok(stats);
    }

    if !device.is_valid_range(addr, data.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    let flash_size = device.size();
    // Clone erase blocks to avoid borrow checker issues
    let erase_blocks: Vec<_> = device.erase_blocks().to_vec();
    let granularity = device.write_granularity();
    // Safe: data.len() > 0 guaranteed by the early return above
    let region_end = addr + data.len() as u32 - 1;

    let mut stats = WriteStats::default();

    // Step 1: Read current contents of the region
    progress.reading(data.len());
    let mut current = vec![0u8; data.len()];

    let mut bytes_read = 0;
    while bytes_read < data.len() {
        let chunk_size = core::cmp::min(READ_CHUNK_SIZE, data.len() - bytes_read);
        device
            .read(
                addr + bytes_read as u32,
                &mut current[bytes_read..bytes_read + chunk_size],
            )
            .await?;
        bytes_read += chunk_size;
        progress.read_progress(bytes_read);
    }

    // Check if any changes are needed
    if !need_write(&current, data) {
        progress.complete(&stats);
        return Ok(stats);
    }

    stats.bytes_changed = get_all_write_ranges(&current, data)
        .iter()
        .map(|r| r.len as usize)
        .sum();

    // Step 2: Plan optimal erase operations for this region
    // The optimal erase algorithm will only select blocks fully within the region
    // for promotion (the >50% heuristic checks block boundaries)
    let erase_ops = plan_optimal_erase(
        &erase_blocks,
        flash_size,
        Some(&current),
        Some(data),
        addr,
        region_end,
        granularity,
    );

    // Step 3: Erase blocks that need it
    if !erase_ops.is_empty() {
        let bytes_to_erase: usize = erase_ops.iter().map(|op| op.size as usize).sum();
        progress.erasing(erase_ops.len(), bytes_to_erase);

        for (i, op) in erase_ops.iter().enumerate() {
            // Handle data outside our region but inside the erase block.
            // A block may straddle the start, the end, or both boundaries
            // of our region, so we must check each side independently.
            let block_end = op.start + op.size;
            let region_end_addr = addr + data.len() as u32;

            let extends_before = op.start < addr;
            let extends_after = block_end > region_end_addr;

            // Read data before our region (if block extends before)
            let pre_data = if extends_before {
                let preserve_len = (addr - op.start) as usize;
                let mut buf = vec![0u8; preserve_len];
                device.read(op.start, &mut buf).await?;
                Some(buf)
            } else {
                None
            };

            // Read data after our region (if block extends after)
            let post_data = if extends_after {
                let preserve_len = (block_end - region_end_addr) as usize;
                let mut buf = vec![0u8; preserve_len];
                device.read(region_end_addr, &mut buf).await?;
                Some(buf)
            } else {
                None
            };

            // Erase the block
            device.erase(op.start, op.size).await?;

            // Restore preserved data
            if let Some(ref buf) = pre_data {
                if let Err(e) = device.write(op.start, buf).await {
                    log::error!(
                        "Failed to restore {} bytes at 0x{:08X} after erase — data may be lost: {}",
                        buf.len(), op.start, e
                    );
                    return Err(e);
                }
            }
            if let Some(ref buf) = post_data {
                if let Err(e) = device.write(region_end_addr, buf).await {
                    log::error!(
                        "Failed to restore {} bytes at 0x{:08X} after erase — data may be lost: {}",
                        buf.len(), region_end_addr, e
                    );
                    return Err(e);
                }
            }

            // Update our view of current contents
            let rel_start = op.start.saturating_sub(addr) as usize;
            let rel_end = ((op.start + op.size).saturating_sub(addr) as usize).min(current.len());
            current[rel_start..rel_end].fill(ERASED_VALUE);

            stats.erases_performed += 1;
            stats.bytes_erased += op.size as usize;
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
            device.write(addr + range.start, write_data).await?;

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
#[maybe_async]
pub async fn smart_write_by_layout<D: FlashDevice + ?Sized, P: WriteProgress>(
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

        let stats =
            smart_write_region(device, region.start, region_data, &mut offset_progress).await?;

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
#[maybe_async]
pub async fn read_by_layout<D: FlashDevice>(
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
        device.read(region.start, region_buf).await?;
    }

    Ok(())
}

/// Erase all included regions in a layout
#[maybe_async]
pub async fn erase_by_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    layout: &Layout,
) -> Result<()> {
    let flash_size = device.size();

    layout.validate(flash_size).map_err(|e| match e {
        LayoutError::RegionOutOfBounds => Error::AddressOutOfBounds,
        LayoutError::ChipSizeMismatch { .. } => Error::AddressOutOfBounds,
        _ => Error::LayoutError,
    })?;

    for region in layout.included_regions() {
        erase_region(device, region).await?;
    }

    Ok(())
}

/// Erase a single region
///
/// This uses the optimal erase algorithm to minimize the number of erase operations.
/// It handles region boundaries that don't align with erase block boundaries
/// by preserving data outside the region.
#[maybe_async]
pub async fn erase_region<D: FlashDevice + ?Sized>(device: &mut D, region: &Region) -> Result<()> {
    if !device.is_valid_range(region.start, region.size() as usize) {
        return Err(Error::AddressOutOfBounds);
    }

    let flash_size = device.size();
    // Clone erase blocks to avoid borrow checker issues
    let erase_blocks: Vec<_> = device.erase_blocks().to_vec();

    // Plan optimal erase operations for this region
    let erase_ops = plan_optimal_erase_region(&erase_blocks, flash_size, region.start, region.end);

    for op in &erase_ops {
        let block_end = op.start + op.size - 1;
        let is_unaligned = op.start < region.start || block_end > region.end;

        if is_unaligned {
            // Need to preserve data outside the region
            let mut backup = vec![ERASED_VALUE; op.size as usize];

            // Read data before region (to preserve)
            if region.start > op.start {
                let len = (region.start - op.start) as usize;
                device.read(op.start, &mut backup[..len]).await?;
            }

            // Read data after region (to preserve)
            if block_end > region.end {
                let start = region.end + 1;
                let rel_start = (start - op.start) as usize;
                let len = (block_end - region.end) as usize;
                device
                    .read(start, &mut backup[rel_start..rel_start + len])
                    .await?;
            }

            // Erase the block
            device.erase(op.start, op.size).await?;

            // Write back preserved data
            if region.start > op.start {
                let len = (region.start - op.start) as usize;
                device.write(op.start, &backup[..len]).await?;
            }
            if block_end > region.end {
                let start = region.end + 1;
                let rel_start = (start - op.start) as usize;
                let len = (block_end - region.end) as usize;
                device
                    .write(start, &backup[rel_start..rel_start + len])
                    .await?;
            }
        } else {
            // Block is aligned with region, just erase it
            device.erase(op.start, op.size).await?;
        }
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
#[maybe_async]
pub async fn verify<D: FlashDevice>(device: &mut D, expected: &[u8], addr: u32) -> Result<()> {
    if !device.is_valid_range(addr, expected.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    let mut buf = vec![0u8; READ_CHUNK_SIZE];
    let mut offset = 0usize;

    while offset < expected.len() {
        let chunk_size = core::cmp::min(READ_CHUNK_SIZE, expected.len() - offset);
        let chunk_buf = &mut buf[..chunk_size];
        device.read(addr + offset as u32, chunk_buf).await?;

        let expected_chunk = &expected[offset..offset + chunk_size];
        if chunk_buf != expected_chunk {
            return Err(Error::VerifyError { addr: addr + offset as u32 });
        }

        offset += chunk_size;
    }

    Ok(())
}

/// Verify all included regions match expected data
#[maybe_async]
pub async fn verify_by_layout<D: FlashDevice>(
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
        verify(device, expected_region, region.start).await?;
    }

    Ok(())
}

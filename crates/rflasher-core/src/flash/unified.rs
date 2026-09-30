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

use super::policy::{MutationPolicy, preflight, region_layout};
use crate::error::{Error, Result};
use crate::flash::device::FlashDevice;
use crate::flash::operations::{
    coalesce_write_ranges, plan_optimal_erase, plan_optimal_erase_region,
};
use crate::layout::{Layout, LayoutError, Region};

/// Full-chip write with explicit mutation authorizations.
pub async fn smart_write_with_policy<D: FlashDevice + ?Sized, P: WriteProgress>(
    device: &mut D,
    data: &[u8],
    progress: &mut P,
    policy: &mut MutationPolicy<'_>,
) -> Result<WriteStats> {
    if data.len() != device.size() as usize {
        return Err(Error::BufferTooSmall);
    }
    let layout = region_layout(0, device.size())?;
    preflight(device, &layout, policy).await?;
    smart_write_region_prepared(device, 0, data, progress).await
}

/// Write a region after shared policy, erase-footprint and recovery preflight.
pub async fn smart_write_region_with_policy<D: FlashDevice + ?Sized, P: WriteProgress>(
    device: &mut D,
    addr: u32,
    data: &[u8],
    progress: &mut P,
    policy: &mut MutationPolicy<'_>,
) -> Result<WriteStats> {
    if !data.is_empty() {
        let len = u32::try_from(data.len()).map_err(|_| Error::AddressOutOfBounds)?;
        preflight(device, &region_layout(addr, len)?, policy).await?;
    }
    smart_write_region_prepared(device, addr, data, progress).await
}

/// Preflight all included regions before writing any of them.
pub async fn smart_write_by_layout_with_policy<D: FlashDevice + ?Sized, P: WriteProgress>(
    device: &mut D,
    layout: &Layout,
    image: &[u8],
    progress: &mut P,
    policy: &mut MutationPolicy<'_>,
) -> Result<WriteStats> {
    if image.len() < device.size() as usize {
        return Err(Error::BufferTooSmall);
    }
    preflight(device, layout, policy).await?;
    smart_write_by_layout_prepared(device, layout, image, progress).await
}

/// Preflight all included regions and recovery storage before the first erase.
pub async fn erase_by_layout_with_policy<D: FlashDevice + ?Sized>(
    device: &mut D,
    layout: &Layout,
    policy: &mut MutationPolicy<'_>,
) -> Result<()> {
    preflight(device, layout, policy).await?;
    erase_by_layout_prepared(device, layout).await
}

/// Erase a region with explicit policy and verified neighbor restoration.
pub async fn erase_region_with_policy<D: FlashDevice + ?Sized>(
    device: &mut D,
    region: &Region,
    policy: &mut MutationPolicy<'_>,
) -> Result<()> {
    let mut layout = Layout::new();
    let mut requested = region.clone();
    requested.included = true;
    layout.add_region(requested);
    preflight(device, &layout, policy).await?;
    erase_region_prepared(device, region).await
}

/// Write a region with safe defaults; cross-boundary erases need a recovery policy.
pub async fn smart_write_region<D: FlashDevice + ?Sized, P: WriteProgress>(
    device: &mut D,
    addr: u32,
    data: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    smart_write_region_with_policy(device, addr, data, progress, &mut MutationPolicy::default())
        .await
}

/// Write included layout regions with safe-default authorization.
pub async fn smart_write_by_layout<D: FlashDevice + ?Sized, P: WriteProgress>(
    device: &mut D,
    layout: &Layout,
    image: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    smart_write_by_layout_with_policy(
        device,
        layout,
        image,
        progress,
        &mut MutationPolicy::default(),
    )
    .await
}

/// Erase included layout regions with safe-default authorization.
pub async fn erase_by_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    layout: &Layout,
) -> Result<()> {
    erase_by_layout_with_policy(device, layout, &mut MutationPolicy::default()).await
}

/// Erase one region with safe defaults; never rely on RAM-only neighbor backup.
pub async fn erase_region<D: FlashDevice + ?Sized>(device: &mut D, region: &Region) -> Result<()> {
    erase_region_with_policy(device, region, &mut MutationPolicy::default()).await
}

// =============================================================================
// Re-exports from operations.rs
// =============================================================================

// Re-export smart write support types from operations.rs
// These are the canonical definitions - no duplication needed
pub use crate::flash::operations::{
    NoProgress, WriteProgress, WriteRange, WriteStats, get_all_write_ranges, need_write,
};

// =============================================================================
// Constants
// =============================================================================

/// The erased value for flash memory (all bits set)
const ERASED_VALUE: u8 = 0xFF;

/// Default read chunk size for progress reporting.
///
/// Each chunk results in a separate `FlashDevice::read()` call, which for
/// hardware-accelerated programmers (e.g. Dediprog) issues a new CMD_READ
/// control transfer. Larger chunks amortize that overhead.
/// 256 KiB gives ~64 progress updates for a 16 MiB flash.
const READ_CHUNK_SIZE: usize = 256 * 1024;

/// Maximum size per `FlashDevice::write()` call during smart write.
///
/// After coalescing, write ranges can be very large (potentially the entire
/// flash). Splitting them into sub-chunks ensures regular progress updates
/// and keeps USB transfers manageable. Must be a multiple of the page size
/// (256 bytes) so writes stay page-aligned for the bulk transfer path.
///
/// 256 KiB = 1024 pages, giving ~64 progress updates for a 16 MiB flash.
const WRITE_CHUNK_SIZE: usize = 256 * 1024;

// =============================================================================
// Unified operations
// =============================================================================

/// Read flash contents into a buffer
///
/// This is a convenience function that reads with progress reporting.
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
pub async fn smart_write<D: FlashDevice + ?Sized, P: WriteProgress>(
    device: &mut D,
    data: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    let flash_size = device.size();

    if data.len() != flash_size as usize {
        return Err(Error::BufferTooSmall);
    }

    smart_write_with_policy(device, data, progress, &mut MutationPolicy::default()).await
}

/// Perform a smart write operation for a specific region
///
/// Similar to `smart_write` but only operates on a specific region of flash.
/// Uses the optimal erase algorithm to minimize erase operations.
async fn smart_write_region_prepared<D: FlashDevice + ?Sized, P: WriteProgress>(
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
    let page_size = device.page_size();
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
    )?;

    // Validate the executable plan before the first erase, not one command
    // at a time after earlier blocks have already been destroyed.
    for op in &erase_ops {
        device.validate_erase_operation(op)?;
    }

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
            device.erase_operation(op).await?;
            verify_erased(device, op.start, op.size).await?;

            // Restore preserved data
            if let Some(ref buf) = pre_data
                && let Err(e) = device.write(op.start, buf).await
            {
                log::error!(
                    "Failed to restore {} bytes at 0x{:08X} after erase — data may be lost: {}",
                    buf.len(),
                    op.start,
                    e
                );
                return Err(e);
            }
            if let Some(ref buf) = post_data
                && let Err(e) = device.write(region_end_addr, buf).await
            {
                log::error!(
                    "Failed to restore {} bytes at 0x{:08X} after erase — data may be lost: {}",
                    buf.len(),
                    region_end_addr,
                    e
                );
                return Err(e);
            }

            if let Some(ref buf) = pre_data {
                verify(device, buf, op.start).await?;
            }
            if let Some(ref buf) = post_data {
                verify(device, buf, region_end_addr).await?;
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

    // Step 4: Write pages that differ
    // Coalesce to page boundaries so bulk transfer paths are used.
    let write_ranges = get_all_write_ranges(&current, data);
    let data_len = data.len() as u32;
    let write_ranges = coalesce_write_ranges(&write_ranges, page_size, data_len);

    if !write_ranges.is_empty() {
        let bytes_to_write: usize = write_ranges.iter().map(|r| r.len as usize).sum();
        progress.writing(bytes_to_write);

        let mut bytes_written = 0;

        for range in &write_ranges {
            // Split large ranges into sub-chunks for progress reporting.
            let range_start = range.start as usize;
            let range_end = range_start + range.len as usize;
            let mut offset = range_start;

            while offset < range_end {
                let chunk_len = (range_end - offset).min(WRITE_CHUNK_SIZE);
                device
                    .write(addr + offset as u32, &data[offset..offset + chunk_len])
                    .await?;
                offset += chunk_len;
                bytes_written += chunk_len;
                stats.writes_performed += 1;
                progress.write_progress(bytes_written);
            }
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
async fn smart_write_by_layout_prepared<D: FlashDevice + ?Sized, P: WriteProgress>(
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

    // Reject protected regions before processing any included region.
    if layout.included_regions().any(|region| region.readonly) {
        return Err(Error::RegionProtected);
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
            smart_write_region_prepared(device, region.start, region_data, &mut offset_progress)
                .await?;

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
async fn erase_by_layout_prepared<D: FlashDevice + ?Sized>(
    device: &mut D,
    layout: &Layout,
) -> Result<()> {
    let flash_size = device.size();

    layout.validate(flash_size).map_err(|e| match e {
        LayoutError::RegionOutOfBounds => Error::AddressOutOfBounds,
        LayoutError::ChipSizeMismatch { .. } => Error::AddressOutOfBounds,
        _ => Error::LayoutError,
    })?;

    // Reject protected regions before erasing any included region.
    if layout.included_regions().any(|region| region.readonly) {
        return Err(Error::RegionProtected);
    }

    for region in layout.included_regions() {
        erase_region_prepared(device, region).await?;
    }

    Ok(())
}

/// Erase a single region
///
/// This uses the optimal erase algorithm to minimize the number of erase operations.
/// It handles region boundaries that don't align with erase block boundaries
/// by preserving data outside the region.
async fn erase_region_prepared<D: FlashDevice + ?Sized>(
    device: &mut D,
    region: &Region,
) -> Result<()> {
    if !device.is_valid_range(region.start, region.size() as usize) {
        return Err(Error::AddressOutOfBounds);
    }

    let flash_size = device.size();
    // Clone erase blocks to avoid borrow checker issues
    let erase_blocks: Vec<_> = device.erase_blocks().to_vec();

    // Plan optimal erase operations for this region
    let erase_ops = plan_optimal_erase_region(&erase_blocks, flash_size, region.start, region.end)?;

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
            device.erase_operation(op).await?;
            verify_erased(device, op.start, op.size).await?;

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
            if region.start > op.start {
                verify(
                    device,
                    &backup[..(region.start - op.start) as usize],
                    op.start,
                )
                .await?;
            }
            if block_end > region.end {
                let start = region.end + 1;
                let offset = (start - op.start) as usize;
                verify(device, &backup[offset..], start).await?;
            }
        } else {
            // Block is aligned with region, just erase it
            device.erase_operation(op).await?;
            verify_erased(device, op.start, op.size).await?;
        }
    }

    Ok(())
}

async fn verify_erased<D: FlashDevice + ?Sized>(device: &mut D, addr: u32, len: u32) -> Result<()> {
    let mut buf = vec![0; READ_CHUNK_SIZE.min(len as usize)];
    let mut offset = 0;
    while offset < len {
        let count = (len - offset).min(buf.len() as u32) as usize;
        device.read(addr + offset, &mut buf[..count]).await?;
        if let Some(index) = buf[..count].iter().position(|b| *b != ERASED_VALUE) {
            return Err(Error::VerifyError {
                addr: addr + offset + index as u32,
            });
        }
        offset += count as u32;
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
pub async fn verify<D: FlashDevice + ?Sized>(
    device: &mut D,
    expected: &[u8],
    addr: u32,
) -> Result<()> {
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
            // Locate the first differing byte so the reported address is exact
            let rel = chunk_buf
                .iter()
                .zip(expected_chunk.iter())
                .position(|(got, want)| got != want)
                .expect("chunks differ, so a differing byte must exist");
            return Err(Error::VerifyError {
                addr: addr + (offset + rel) as u32,
            });
        }

        offset += chunk_size;
    }

    Ok(())
}

/// Verify all included regions match expected data
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chip::{EraseBlock, WriteGranularity};
    use futures_lite::future::block_on;

    struct FakeFlash {
        bytes: Vec<u8>,
        blocks: Vec<EraseBlock>,
        mutations: usize,
    }

    impl FakeFlash {
        fn new(size: u32) -> Self {
            Self {
                bytes: vec![0; size as usize],
                blocks: vec![EraseBlock::with_count(0x20, 16, size / 16)],
                mutations: 0,
            }
        }
    }

    impl FlashDevice for FakeFlash {
        fn size(&self) -> u32 {
            self.bytes.len() as u32
        }
        fn erase_granularity(&self) -> u32 {
            16
        }
        fn write_granularity(&self) -> WriteGranularity {
            WriteGranularity::Byte
        }
        fn erase_blocks(&self) -> &[EraseBlock] {
            &self.blocks
        }
        async fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
            buf.copy_from_slice(&self.bytes[addr as usize..addr as usize + buf.len()]);
            Ok(())
        }
        async fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
            self.mutations += 1;
            self.bytes[addr as usize..addr as usize + data.len()].copy_from_slice(data);
            Ok(())
        }
        async fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
            self.mutations += 1;
            self.bytes[addr as usize..(addr + len) as usize].fill(0xff);
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestBackup {
        image: Vec<u8>,
        fail: bool,
    }
    impl super::super::RecoveryBackup for TestBackup {
        fn persist(&mut self, image: &[u8]) -> Result<()> {
            if self.fail {
                return Err(Error::IoError);
            }
            self.image = image.to_vec();
            Ok(())
        }
    }

    #[test]
    fn on_device_ifd_cannot_be_hidden_by_a_renamed_or_full_chip_layout() {
        let mut flash = FakeFlash::new(16384);
        flash.bytes[..4].copy_from_slice(&0x0ff0_a55au32.to_le_bytes());
        flash.bytes[4..8].copy_from_slice(&((2u32 << 24) | (4 << 16)).to_le_bytes());
        flash.bytes[0x40..0x44].copy_from_slice(&0u32.to_le_bytes());
        flash.bytes[0x44..0x48].copy_from_slice(&((3u32 << 16) | 2).to_le_bytes()); // BIOS 8..16K
        flash.bytes[0x48..0x4c].copy_from_slice(&((1u32 << 16) | 1).to_le_bytes()); // ME 4..8K
        let mut whole = MutationPolicy {
            allow_full_chip: true,
            ..Default::default()
        };
        assert!(matches!(
            block_on(smart_write_with_policy(
                &mut flash,
                &vec![0xff; 16384],
                &mut NoProgress,
                &mut whole
            )),
            Err(Error::RegionProtected)
        ));
        assert_eq!(
            block_on(erase_region(
                &mut flash,
                &Region::new("innocent", 4096, 8191)
            )),
            Err(Error::RegionProtected)
        );
        assert_eq!(flash.mutations, 0);
        block_on(erase_region(&mut flash, &Region::new("bios", 8192, 16383))).unwrap();
        assert!(flash.mutations > 0);
    }

    #[test]
    fn unusable_on_device_ifd_needs_dangerous_authorization_but_not_more() {
        // A descriptor whose BIOS region lies beyond this chip, as on a
        // damaged image or on the first chip of a two-chip board.
        let mut flash = FakeFlash::new(16384);
        flash.bytes[..4].copy_from_slice(&0x0ff0_a55au32.to_le_bytes());
        flash.bytes[4..8].copy_from_slice(&((2u32 << 24) | (4 << 16)).to_le_bytes());
        flash.bytes[0x44..0x48].copy_from_slice(&((0x7ffu32 << 16) | 0x10).to_le_bytes());
        let image = vec![0xff; 16384];
        let mut policy = MutationPolicy {
            allow_full_chip: true,
            ..Default::default()
        };
        assert_eq!(
            block_on(smart_write_with_policy(
                &mut flash,
                &image,
                &mut NoProgress,
                &mut policy
            ))
            .unwrap_err(),
            Error::RegionProtected
        );
        assert_eq!(flash.mutations, 0);
        policy.allow_dangerous = true;
        block_on(smart_write_with_policy(
            &mut flash,
            &image,
            &mut NoProgress,
            &mut policy,
        ))
        .unwrap();
        assert_eq!(flash.bytes, image);
    }

    #[test]
    fn missing_or_failed_recovery_is_refused_before_mutation() {
        let mut flash = FakeFlash::new(64);
        let region = Region::new("partial", 4, 11);
        assert_eq!(
            block_on(erase_region(&mut flash, &region)),
            Err(Error::RegionProtected)
        );
        let mut sink = TestBackup {
            fail: true,
            ..Default::default()
        };
        assert_eq!(
            block_on(erase_region_with_policy(
                &mut flash,
                &region,
                &mut MutationPolicy {
                    recovery: Some(&mut sink),
                    ..Default::default()
                }
            )),
            Err(Error::IoError)
        );
        assert_eq!(flash.mutations, 0);
    }

    #[test]
    fn whole_chip_and_dangerous_acknowledgements_are_independent() {
        let mut flash = FakeFlash::new(64);
        let mut region = Region::new("whole", 0, 63);
        region.dangerous = true;
        assert_eq!(
            block_on(erase_region_with_policy(
                &mut flash,
                &region,
                &mut MutationPolicy {
                    allow_full_chip: true,
                    ..Default::default()
                }
            )),
            Err(Error::RegionProtected)
        );
        assert_eq!(
            block_on(erase_region_with_policy(
                &mut flash,
                &region,
                &mut MutationPolicy {
                    allow_dangerous: true,
                    ..Default::default()
                }
            )),
            Err(Error::RegionProtected)
        );
        assert_eq!(flash.mutations, 0);
    }

    #[derive(Default)]
    struct TraceProgress {
        reading: Vec<usize>,
        read_progress: Vec<usize>,
        erasing: Vec<(usize, usize)>,
        erase_progress: Vec<(usize, usize)>,
        writing: Vec<usize>,
        write_progress: Vec<usize>,
        completions: usize,
    }

    impl WriteProgress for TraceProgress {
        fn reading(&mut self, total: usize) {
            self.reading.push(total);
        }
        fn read_progress(&mut self, bytes: usize) {
            self.read_progress.push(bytes);
        }
        fn erasing(&mut self, count: usize, bytes: usize) {
            self.erasing.push((count, bytes));
        }
        fn erase_progress(&mut self, count: usize, bytes: usize) {
            self.erase_progress.push((count, bytes));
        }
        fn writing(&mut self, bytes: usize) {
            self.writing.push(bytes);
        }
        fn write_progress(&mut self, bytes: usize) {
            self.write_progress.push(bytes);
        }
        fn complete(&mut self, _: &WriteStats) {
            self.completions += 1;
        }
    }

    #[test]
    fn full_and_partial_smart_write_share_stats_and_progress_engine() {
        let mut full = FakeFlash::new(64);
        let mut full_progress = TraceProgress::default();
        let full_stats = block_on(smart_write_with_policy(
            &mut full,
            &[0xff; 64],
            &mut full_progress,
            &mut MutationPolicy {
                allow_full_chip: true,
                ..Default::default()
            },
        ))
        .unwrap();
        assert_eq!(full_stats.bytes_changed, 64);
        assert_eq!(full_stats.bytes_erased, 64);
        assert_eq!(full_stats.erases_performed, 4);
        assert_eq!(full_progress.reading, vec![64]);
        assert_eq!(full_progress.read_progress, vec![64]);
        assert_eq!(full_progress.erasing, vec![(4, 64)]);
        assert_eq!(full_progress.erase_progress.last(), Some(&(4, 64)));
        assert!(full_progress.writing.is_empty());
        assert!(full_progress.write_progress.is_empty());
        assert_eq!(full_progress.completions, 1);

        let mut partial = FakeFlash::new(64);
        let mut partial_progress = TraceProgress::default();
        let mut backup = TestBackup::default();
        let stats = block_on(smart_write_region_with_policy(
            &mut partial,
            4,
            &[0xff; 8],
            &mut partial_progress,
            &mut MutationPolicy {
                recovery: Some(&mut backup),
                ..Default::default()
            },
        ))
        .unwrap();
        assert_eq!(backup.image, vec![0; 64]);
        assert_eq!(stats.bytes_changed, 8);
        assert_eq!(stats.bytes_erased, 16);
        assert_eq!(stats.erases_performed, 1);
        assert_eq!(partial_progress.reading, vec![8]);
        assert_eq!(partial_progress.read_progress, vec![8]);
        assert_eq!(partial_progress.erasing, vec![(1, 16)]);
        assert_eq!(partial_progress.erase_progress, vec![(1, 16)]);
        assert_eq!(partial_progress.completions, 1);
        assert_eq!(&partial.bytes[..4], &[0; 4]);
        assert_eq!(&partial.bytes[4..12], &[0xff; 8]);
        assert_eq!(&partial.bytes[12..], &[0; 52]);
    }

    #[test]
    fn full_smart_write_reports_write_progress_and_stats() {
        let mut device = FakeFlash::new(64);
        device.bytes.fill(0xff);
        let mut data = vec![0xff; 64];
        data[7] = 0x12;
        let mut progress = TraceProgress::default();
        let stats = block_on(smart_write_with_policy(
            &mut device,
            &data,
            &mut progress,
            &mut MutationPolicy {
                allow_full_chip: true,
                ..Default::default()
            },
        ))
        .unwrap();
        assert_eq!(stats.bytes_changed, 1);
        assert_eq!(stats.bytes_erased, 0);
        assert_eq!(stats.bytes_written, 1);
        assert_eq!(stats.writes_performed, 1);
        assert!(stats.flash_modified);
        assert_eq!(progress.writing, vec![1]);
        assert_eq!(progress.write_progress, vec![1]);
        assert_eq!(progress.completions, 1);
        assert_eq!(device.bytes, data);
    }

    #[test]
    fn nonuniform_only_erase_fails_instead_of_succeeding_without_erasing() {
        use crate::chip::EraseRegion;

        let mut device = FakeFlash::new(64);
        device.blocks = vec![EraseBlock::with_regions(
            0x20,
            &[EraseRegion::new(8, 2), EraseRegion::new(16, 3)],
        )];
        let region = Region::new("partial", 4, 11);
        assert_eq!(
            block_on(erase_region(&mut device, &region)),
            Err(Error::InvalidAlignment)
        );
        assert_eq!(device.mutations, 0);
        assert!(matches!(
            block_on(smart_write_region(
                &mut device,
                4,
                &[0xff; 8],
                &mut NoProgress
            )),
            Err(Error::InvalidAlignment)
        ));
        assert_eq!(device.mutations, 0);
    }

    #[test]
    fn chip_erase_only_full_region_is_not_a_noop() {
        let mut device = FakeFlash::new(64);
        device.blocks = vec![EraseBlock::new(0xc7, 64)];
        block_on(erase_region_with_policy(
            &mut device,
            &Region::new("full", 0, 63),
            &mut MutationPolicy {
                allow_full_chip: true,
                ..Default::default()
            },
        ))
        .unwrap();
        assert_eq!(device.mutations, 1);
        assert!(device.bytes.iter().all(|byte| *byte == 0xff));
        assert_eq!(
            block_on(erase_region(&mut device, &Region::new("partial", 0, 7))),
            Err(Error::InvalidAlignment)
        );
    }

    #[test]
    fn readonly_layout_rejected_before_any_mutation() {
        let mut device = FakeFlash::new(64);
        let mut layout = Layout::new();
        let mut writable = Region::new("writable", 0, 15);
        writable.included = true;
        let mut protected = Region::new("protected", 16, 31);
        protected.included = true;
        protected.readonly = true;
        layout.add_region(writable);
        layout.add_region(protected);

        assert!(matches!(
            block_on(smart_write_by_layout(
                &mut device,
                &layout,
                &[0xff; 64],
                &mut NoProgress
            )),
            Err(Error::RegionProtected)
        ));
        assert_eq!(device.mutations, 0);
        assert_eq!(
            block_on(erase_by_layout(&mut device, &layout)),
            Err(Error::RegionProtected)
        );
        assert_eq!(device.mutations, 0);
        assert!(device.bytes.iter().all(|byte| *byte == 0));
    }
}

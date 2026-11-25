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

use super::policy::{
    MutationPolicy, check_erase_access, check_erase_plan_access, preflight, region_layout,
    validate_neighbors,
};
use crate::chip::WriteGranularity;
use crate::error::{EraseFailure, Error, Result};
use crate::flash::device::{EraseOutcome, FlashDevice};
use crate::flash::operations::{
    OptimalEraseOp, coalesce_write_ranges, plan_optimal_erase, plan_optimal_erase_region,
};
use crate::layout::{Layout, Region};
use crate::programmer::FlashAccess;

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
/// * `policy` - Mutation authorizations
///
/// # Returns
/// Statistics about the operations performed
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
    let snapshot = preflight(device, &layout, policy).await?;
    progress.reading(data.len());
    let plan = prepare_smart_region(device, 0, data, snapshot.as_deref(), |n| {
        progress.read_progress(n);
    })
    .await?;
    let stats = execute_prepared_region(device, &plan, data, progress).await?;
    progress.complete(&stats);
    Ok(stats)
}

/// Write a region after shared policy and erase-footprint preflight.
pub async fn smart_write_region_with_policy<D: FlashDevice + ?Sized, P: WriteProgress>(
    device: &mut D,
    addr: u32,
    data: &[u8],
    progress: &mut P,
    policy: &mut MutationPolicy<'_>,
) -> Result<WriteStats> {
    if data.is_empty() {
        let stats = WriteStats::default();
        progress.complete(&stats);
        return Ok(stats);
    }
    let len = u32::try_from(data.len()).map_err(|_| Error::AddressOutOfBounds)?;
    let snapshot = preflight(device, &region_layout(addr, len)?, policy).await?;
    progress.reading(data.len());
    let plan = prepare_smart_region(device, addr, data, snapshot.as_deref(), |n| {
        progress.read_progress(n);
    })
    .await?;
    let stats = execute_prepared_region(device, &plan, data, progress).await?;
    progress.complete(&stats);
    Ok(stats)
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
    let snapshot = preflight(device, layout, policy).await?;

    let included: Vec<_> = layout.included_regions().collect();
    if included.is_empty() {
        let stats = WriteStats::default();
        progress.complete(&stats);
        return Ok(stats);
    }

    let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();
    progress.reading(total_bytes);

    let mut plans = Vec::with_capacity(included.len());
    let mut overall_bytes_read = 0usize;
    for region in &included {
        let region_data = &image[region.start as usize..=region.end as usize];
        let read_offset = overall_bytes_read;
        let plan = prepare_smart_region(
            device,
            region.start,
            region_data,
            snapshot.as_deref(),
            |n| progress.read_progress(read_offset + n),
        )
        .await?;
        overall_bytes_read += region_data.len();
        plans.push((plan, region_data));
    }

    let mut combined_stats = WriteStats::default();
    for (plan, region_data) in &plans {
        let stats = execute_prepared_region(device, plan, region_data, progress).await?;
        combined_stats.bytes_changed += stats.bytes_changed;
        combined_stats.erases_performed += stats.erases_performed;
        combined_stats.bytes_erased += stats.bytes_erased;
        combined_stats.writes_performed += stats.writes_performed;
        combined_stats.bytes_written += stats.bytes_written;
        combined_stats.flash_modified |= stats.flash_modified;
    }

    progress.complete(&combined_stats);
    Ok(combined_stats)
}

/// Preflight all included regions before the first erase.
pub async fn erase_by_layout_with_policy<D: FlashDevice + ?Sized>(
    device: &mut D,
    layout: &Layout,
    policy: &mut MutationPolicy<'_>,
) -> Result<()> {
    let _ = preflight(device, layout, policy).await?;
    check_erase_plan_access(device, layout)?;
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
    let _ = preflight(device, &layout, policy).await?;
    check_erase_plan_access(device, &layout)?;
    erase_region_prepared(device, region).await
}

/// Write a region with safe defaults (no dangerous-region authorization).
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

/// Erase one region with safe defaults (no dangerous-region authorization).
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
pub async fn read_with_progress<D: FlashDevice + ?Sized, P: WriteProgress>(
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

/// Validated erase and write plan for a single region, computed from a single
/// read of the region (or sliced from the preflight recovery snapshot).
struct PreparedSmartRegion {
    addr: u32,
    len: u32,
    bytes_changed: usize,
    erase_ops: Vec<OptimalEraseOp>,
    write_ranges: Vec<WriteRange>,
}

/// Read (or slice from `snapshot`), plan, and validate the content-dependent
/// operations for a single region before any mutation occurs.
///
/// Earlier regions restore their neighbors, so later region contents remain
/// unchanged. Only what the plan reads and changes is checked against device
/// access restrictions, so unchanged restricted data in a region does not
/// refuse it.
async fn prepare_smart_region<D: FlashDevice + ?Sized, F: FnMut(usize)>(
    device: &mut D,
    addr: u32,
    data: &[u8],
    snapshot: Option<&[u8]>,
    mut on_read_progress: F,
) -> Result<PreparedSmartRegion> {
    if data.is_empty() {
        return Ok(PreparedSmartRegion {
            addr,
            len: 0,
            bytes_changed: 0,
            erase_ops: Vec::new(),
            write_ranges: Vec::new(),
        });
    }
    if !device.is_valid_range(addr, data.len()) {
        return Err(Error::AddressOutOfBounds);
    }
    // In range, so the length fits in u32.
    let len = data.len() as u32;
    device.check_access(FlashAccess::Read, addr, len)?;

    let start = addr as usize;
    let end = start + data.len();
    let mut current = if let Some(slice) = snapshot.and_then(|full| full.get(start..end)) {
        on_read_progress(data.len());
        slice.to_vec()
    } else {
        let mut buf = vec![0u8; data.len()];
        let mut bytes_read = 0;
        while bytes_read < data.len() {
            let chunk_size = core::cmp::min(READ_CHUNK_SIZE, data.len() - bytes_read);
            device
                .read(
                    addr + bytes_read as u32,
                    &mut buf[bytes_read..bytes_read + chunk_size],
                )
                .await?;
            bytes_read += chunk_size;
            on_read_progress(bytes_read);
        }
        buf
    };

    if !need_write(&current, data) {
        return Ok(PreparedSmartRegion {
            addr,
            len,
            bytes_changed: 0,
            erase_ops: Vec::new(),
            write_ranges: Vec::new(),
        });
    }

    let bytes_changed = get_all_write_ranges(&current, data)
        .iter()
        .map(|r| r.len as usize)
        .sum();

    let granularity = device.write_granularity();
    let erase_ops = plan_optimal_erase(
        &super::policy::usable_erasers(device),
        device.size(),
        Some(&current),
        Some(data),
        addr,
        addr + len - 1,
        granularity,
    )?;

    for op in &erase_ops {
        device.validate_erase_operation(op)?;
        validate_neighbors(device, op, addr, addr + len)?;
        check_erase_access(device, op)?;
        let rel_start = op.start.saturating_sub(addr) as usize;
        let rel_end = ((op.start + op.size).saturating_sub(addr) as usize).min(current.len());
        current[rel_start..rel_end].fill(ERASED_VALUE);
    }

    // Byte-program devices can require every programmed byte to be erased,
    // including unchanged bytes. Do not expand their writes to page boundaries.
    let write_ranges = get_all_write_ranges(&current, data);
    let write_ranges = if granularity == WriteGranularity::Byte {
        write_ranges
    } else {
        coalesce_write_ranges(&write_ranges, device.page_size(), len)
    };

    for range in &write_ranges {
        let mut offset = range.start as usize;
        let range_end = offset + range.len as usize;
        while offset < range_end {
            let chunk_len = (range_end - offset).min(WRITE_CHUNK_SIZE);
            device.validate_write(addr + offset as u32, chunk_len)?;
            device.check_access(FlashAccess::Modify, addr + offset as u32, chunk_len as u32)?;
            offset += chunk_len;
        }
    }

    Ok(PreparedSmartRegion {
        addr,
        len,
        bytes_changed,
        erase_ops,
        write_ranges,
    })
}

/// Execute a previously prepared and validated region plan.
async fn execute_prepared_region<D: FlashDevice + ?Sized, P: WriteProgress>(
    device: &mut D,
    plan: &PreparedSmartRegion,
    data: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    let mut stats = WriteStats {
        bytes_changed: plan.bytes_changed,
        ..WriteStats::default()
    };

    if !plan.erase_ops.is_empty() {
        let bytes_to_erase: usize = plan.erase_ops.iter().map(|op| op.size as usize).sum();
        progress.erasing(plan.erase_ops.len(), bytes_to_erase);

        for (i, op) in plan.erase_ops.iter().enumerate() {
            erase_preserving(device, op, plan.addr, plan.addr + plan.len).await?;
            stats.erases_performed += 1;
            stats.bytes_erased += op.size as usize;
            progress.erase_progress(i + 1, stats.bytes_erased);
        }
        stats.flash_modified = true;
    }

    if !plan.write_ranges.is_empty() {
        let bytes_to_write: usize = plan.write_ranges.iter().map(|r| r.len as usize).sum();
        progress.writing(bytes_to_write);

        let mut bytes_written = 0;

        for range in &plan.write_ranges {
            // Split large ranges into sub-chunks for progress reporting.
            let range_start = range.start as usize;
            let range_end = range_start + range.len as usize;
            let mut offset = range_start;

            while offset < range_end {
                let chunk_len = (range_end - offset).min(WRITE_CHUNK_SIZE);
                device
                    .write(plan.addr + offset as u32, &data[offset..offset + chunk_len])
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

    Ok(stats)
}

/// Read all included regions from flash into a buffer
///
/// Regions that are not included will be left unchanged in the buffer.
pub async fn read_by_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    layout: &Layout,
    buffer: &mut [u8],
) -> Result<()> {
    let flash_size = device.size();

    // Validate layout against device
    layout.validate(flash_size)?;

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

    layout.validate(flash_size)?;

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
    let erase_blocks = super::policy::usable_erasers(device);

    // Plan optimal erase operations for this region
    let erase_ops = plan_optimal_erase_region(&erase_blocks, flash_size, region.start, region.end)?;

    for op in &erase_ops {
        erase_preserving(device, op, region.start, region.end + 1).await?;
    }

    Ok(())
}

struct Neighbor {
    addr: u32,
    data: Vec<u8>,
}

// Only a verified mismatch permits recovery. A device error may leave an
// operation in flight, so issuing any further IO here would be unsafe.
async fn erase_preserving<D: FlashDevice + ?Sized>(
    device: &mut D,
    op: &super::operations::OptimalEraseOp,
    start: u32,
    end: u32,
) -> Result<()> {
    let block_end = op.start + op.size;
    let mut neighbors = Vec::new();
    for (from, to) in [
        (op.start, start.min(block_end)),
        (end.max(op.start), block_end),
    ] {
        if from < to {
            let mut data = vec![0; (to - from) as usize];
            device.read(from, &mut data).await?;
            neighbors.push(Neighbor { addr: from, data });
        }
    }

    let verified = match device.erase_operation(op).await {
        // Reading the range again would add nothing but a chance for an IO
        // error that leaves the neighbors unrestored.
        Ok(EraseOutcome::VerifiedErased) => Ok(()),
        Ok(EraseOutcome::Unverified) => verify_erased(device, op.start, op.size).await,
        // A device that reads back its own erase completed those reads, so
        // its mismatch is as recoverable as one found here.
        Err(e @ Error::EraseError(EraseFailure::VerifyFailed { .. })) => Err((e, true)),
        Err(e) => Err((e, false)),
    };
    match verified {
        Ok(()) => {}
        Err((primary, true)) => {
            if let Err(e) = restore_neighbors(device, &neighbors, true).await {
                log::error!("Neighbor recovery after incomplete erase failed: {e}");
            }
            return Err(primary);
        }
        Err((e, false)) => {
            report_neighbors(&neighbors, e);
            return Err(e);
        }
    }
    restore_neighbors(device, &neighbors, false).await
}

fn report_neighbors(neighbors: &[Neighbor], error: Error) {
    for neighbor in neighbors {
        log::error!(
            "RESTORE FAILED: {} bytes at 0x{:08X} outside the requested range may be lost or unverified ({error}); restore from backup",
            neighbor.data.len(),
            neighbor.addr
        );
    }
}

async fn restore_neighbors<D: FlashDevice + ?Sized>(
    device: &mut D,
    neighbors: &[Neighbor],
    reread: bool,
) -> Result<()> {
    // Program all neighbors before verifying any. Devices may also verify
    // inside write(): a completed read with a mismatch is safe to continue
    // after, unlike a transport failure or timeout.
    let mut first = None;
    for neighbor in neighbors {
        let mut current = vec![ERASED_VALUE; neighbor.data.len()];
        if reread && let Err(e) = device.read(neighbor.addr, &mut current).await {
            report_neighbors(neighbors, e);
            return Err(first.unwrap_or(e));
        }
        let mut offset = 0;
        for run in current.chunk_by(|a, b| (*a == ERASED_VALUE) == (*b == ERASED_VALUE)) {
            let saved = &neighbor.data[offset..offset + run.len()];
            if run[0] == ERASED_VALUE
                && saved.iter().any(|b| *b != ERASED_VALUE)
                && let Err(e) = device.write(neighbor.addr + offset as u32, saved).await
            {
                if matches!(e, Error::VerifyError { .. }) {
                    first.get_or_insert(e);
                } else {
                    report_neighbors(neighbors, e);
                    return Err(first.unwrap_or(e));
                }
            }
            offset += run.len();
        }
    }
    for (i, neighbor) in neighbors.iter().enumerate() {
        match verify_neighbor(device, neighbor).await {
            Ok(()) => {}
            Err((e, true)) => {
                report_neighbors(core::slice::from_ref(neighbor), e);
                first.get_or_insert(e);
            }
            Err((e, false)) => {
                report_neighbors(&neighbors[i..], e);
                return Err(first.unwrap_or(e));
            }
        }
    }
    first.map_or(Ok(()), Err)
}

// The boolean distinguishes a successful read with mismatched data from an
// IO error; the latter means the device's readiness is unknown.
async fn verify_neighbor<D: FlashDevice + ?Sized>(
    device: &mut D,
    neighbor: &Neighbor,
) -> core::result::Result<(), (Error, bool)> {
    let mut buf = vec![0; READ_CHUNK_SIZE.min(neighbor.data.len())];
    for (i, expected) in neighbor.data.chunks(READ_CHUNK_SIZE).enumerate() {
        let addr = neighbor.addr + (i * READ_CHUNK_SIZE) as u32;
        let chunk = &mut buf[..expected.len()];
        device.read(addr, chunk).await.map_err(|e| (e, false))?;
        if let Some(pos) = chunk.iter().zip(expected).position(|(a, b)| a != b) {
            return Err((
                Error::VerifyError {
                    addr: addr + pos as u32,
                    expected: expected[pos],
                    found: chunk[pos],
                },
                true,
            ));
        }
    }
    Ok(())
}

/// The post-erase check for every device type whose `erase_operation` did
/// not report the range as verified erased.
async fn verify_erased<D: FlashDevice + ?Sized>(
    device: &mut D,
    addr: u32,
    len: u32,
) -> core::result::Result<(), (Error, bool)> {
    let mut buf = vec![0; READ_CHUNK_SIZE.min(len as usize)];
    let mut offset = 0;
    while offset < len {
        let count = (len - offset).min(buf.len() as u32) as usize;
        device
            .read(addr + offset, &mut buf[..count])
            .await
            .map_err(|e| (e, false))?;
        if let Some((index, &found)) = buf[..count]
            .iter()
            .enumerate()
            .find(|&(_, b)| *b != ERASED_VALUE)
        {
            return Err((
                Error::EraseError(EraseFailure::VerifyFailed {
                    addr: addr + offset + index as u32,
                    found,
                }),
                true,
            ));
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
    verify_with_progress(device, expected, addr, |_| {}).await
}

async fn verify_with_buf<D: FlashDevice + ?Sized, F: FnMut(usize)>(
    device: &mut D,
    expected: &[u8],
    addr: u32,
    buf: &mut [u8],
    mut on_progress: F,
) -> Result<()> {
    if !device.is_valid_range(addr, expected.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    let mut offset = 0usize;

    while offset < expected.len() {
        let chunk_size = core::cmp::min(buf.len(), expected.len() - offset);
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
                expected: expected_chunk[rel],
                found: chunk_buf[rel],
            });
        }

        offset += chunk_size;
        on_progress(offset);
    }

    Ok(())
}

/// Verify flash contents against expected data, calling `on_progress` with the
/// cumulative number of verified bytes after each chunk.
pub async fn verify_with_progress<D: FlashDevice + ?Sized, F: FnMut(usize)>(
    device: &mut D,
    expected: &[u8],
    addr: u32,
    on_progress: F,
) -> Result<()> {
    if expected.is_empty() {
        return if device.is_valid_range(addr, 0) {
            Ok(())
        } else {
            Err(Error::AddressOutOfBounds)
        };
    }

    let mut buf = vec![0u8; READ_CHUNK_SIZE.min(expected.len())];
    verify_with_buf(device, expected, addr, &mut buf, on_progress).await
}

/// Verify all included regions match expected data
pub async fn verify_by_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    layout: &Layout,
    expected: &[u8],
) -> Result<()> {
    verify_by_layout_with_progress(device, layout, expected, |_| {}).await
}

/// Verify all included regions match expected data, calling `on_progress` with
/// the cumulative number of verified bytes across all included regions.
pub async fn verify_by_layout_with_progress<D: FlashDevice + ?Sized, F: FnMut(usize)>(
    device: &mut D,
    layout: &Layout,
    expected: &[u8],
    mut on_progress: F,
) -> Result<()> {
    let flash_size = device.size();

    layout.validate(flash_size)?;

    if expected.len() < flash_size as usize {
        return Err(Error::BufferTooSmall);
    }

    let max_region = layout
        .included_regions()
        .map(|r| r.size() as usize)
        .max()
        .unwrap_or(0);
    if max_region == 0 {
        return Ok(());
    }

    let mut buf = vec![0u8; READ_CHUNK_SIZE.min(max_region)];
    let mut verified_before = 0usize;
    for region in layout.included_regions() {
        let expected_region = &expected[region.start as usize..=region.end as usize];
        verify_with_buf(device, expected_region, region.start, &mut buf, |n| {
            on_progress(verified_before + n);
        })
        .await?;
        verified_before += expected_region.len();
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
        require_erased_writes: bool,
        reject_writes_from: Option<u32>,
        reject_erase_opcode: Option<u8>,
        unerased: Vec<(usize, u8)>,
        miswritten: Vec<(usize, u8)>,
        fail_read_after_erase: Option<usize>,
        reads_after_erase: usize,
        timeout_write_at: Option<u32>,
        erase_error: Option<Error>,
        /// `erase_operation` checks its own erase and reports the outcome.
        verifies_erase: bool,
        verifies_write: bool,
        failed: bool,
        /// Inclusive byte ranges `check_access` refuses for an access.
        blocked: Vec<(FlashAccess, u32, u32)>,
        /// Recorded `(addr, len)` of every `read()` call.
        read_ranges: Vec<(u32, usize)>,
    }

    impl FakeFlash {
        fn new(size: u32) -> Self {
            Self {
                bytes: vec![0; size as usize],
                blocks: vec![EraseBlock::with_count(0x20, 16, size / 16)],
                mutations: 0,
                require_erased_writes: false,
                reject_writes_from: None,
                reject_erase_opcode: None,
                unerased: Vec::new(),
                miswritten: Vec::new(),
                fail_read_after_erase: None,
                reads_after_erase: 0,
                timeout_write_at: None,
                erase_error: None,
                verifies_erase: false,
                verifies_write: false,
                failed: false,
                blocked: Vec::new(),
                read_ranges: Vec::new(),
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
        fn check_access(&self, access: FlashAccess, addr: u32, len: u32) -> Result<()> {
            let last = addr + len - 1;
            match self
                .blocked
                .iter()
                .any(|&(a, first, end)| a == access && first <= last && addr <= end)
            {
                true => Err(Error::RegionProtected),
                false => Ok(()),
            }
        }
        async fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
            assert!(!self.failed, "IO after device failure");
            self.read_ranges.push((addr, buf.len()));
            if self.mutations > 0 {
                self.reads_after_erase += 1;
                if self.fail_read_after_erase == Some(self.reads_after_erase) {
                    self.failed = true;
                    return Err(Error::ReadError { addr });
                }
            }
            buf.copy_from_slice(&self.bytes[addr as usize..addr as usize + buf.len()]);
            Ok(())
        }
        fn validate_write(&self, addr: u32, len: usize) -> Result<()> {
            if self
                .reject_writes_from
                .is_some_and(|from| addr as u64 + len as u64 > from as u64)
            {
                return Err(Error::OpcodeNotSupported);
            }
            if self.is_valid_range(addr, len) {
                Ok(())
            } else {
                Err(Error::AddressOutOfBounds)
            }
        }
        async fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
            assert!(!self.failed, "IO after device failure");
            if self.timeout_write_at == Some(addr) {
                self.failed = true;
                return Err(Error::Timeout);
            }
            if self.require_erased_writes {
                assert!(
                    self.bytes[addr as usize..addr as usize + data.len()]
                        .iter()
                        .all(|byte| *byte == 0xff)
                );
            }
            self.mutations += 1;
            let range = addr as usize..addr as usize + data.len();
            self.bytes[range.clone()].copy_from_slice(data);
            for &(at, value) in self.miswritten.iter().filter(|(at, _)| range.contains(at)) {
                self.bytes[at] = value;
            }
            if self.verifies_write
                && let Some(i) = self.bytes[range.clone()]
                    .iter()
                    .zip(data)
                    .position(|(a, b)| a != b)
            {
                return Err(Error::VerifyError {
                    addr: addr + i as u32,
                    expected: data[i],
                    found: self.bytes[range.start + i],
                });
            }
            Ok(())
        }
        fn validate_erase_operation(
            &self,
            op: &crate::flash::operations::OptimalEraseOp,
        ) -> Result<()> {
            if self.reject_erase_opcode == Some(op.erase_block.opcode) {
                return Err(Error::OpcodeNotSupported);
            }
            Ok(())
        }
        async fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
            assert!(!self.failed, "IO after device failure");
            self.mutations += 1;
            let range = addr as usize..(addr + len) as usize;
            self.bytes[range.clone()].fill(0xff);
            for &(at, value) in self.unerased.iter().filter(|(at, _)| range.contains(at)) {
                self.bytes[at] = value;
            }
            if let Some(e) = self.erase_error {
                self.failed = true;
                return Err(e);
            }
            Ok(())
        }
        async fn erase_operation(
            &mut self,
            op: &crate::flash::operations::OptimalEraseOp,
        ) -> Result<EraseOutcome> {
            self.erase(op.start, op.size).await?;
            if !self.verifies_erase {
                return Ok(EraseOutcome::Unverified);
            }
            // Like a device's internal check, this does not go through `read`.
            let range = op.start as usize..(op.start + op.size) as usize;
            match self.bytes[range].iter().position(|b| *b != ERASED_VALUE) {
                Some(i) => Err(Error::EraseError(EraseFailure::VerifyFailed {
                    addr: op.start + i as u32,
                    found: self.bytes[op.start as usize + i],
                })),
                None => Ok(EraseOutcome::VerifiedErased),
            }
        }
    }

    fn boundary_mutation(flash: &mut FakeFlash, smart: bool) -> Result<()> {
        flash.bytes = (0..64).collect();
        flash.require_erased_writes = true;
        if smart {
            block_on(smart_write_region(flash, 4, &[0x80; 8], &mut NoProgress)).map(|_| ())
        } else {
            block_on(erase_region(flash, &Region::new("partial", 4, 11)))
        }
    }

    #[test]
    fn boundary_neighbors_restored_and_incomplete_erase_recovers_only_erased_bytes() {
        for smart in [false, true] {
            // Recovery is the same when the device reads back its own erase.
            for (partial, verifies_erase) in [false, true].map(|v| [(false, v), (true, v)]).concat()
            {
                let mut flash = FakeFlash::new(64);
                if partial {
                    flash.unerased = vec![(6, 0), (13, 0x7f)];
                }
                flash.verifies_erase = verifies_erase;
                let result = boundary_mutation(&mut flash, smart);
                if partial {
                    assert_eq!(
                        result,
                        Err(Error::EraseError(EraseFailure::VerifyFailed {
                            addr: 6,
                            found: 0
                        }))
                    );
                    assert_eq!(&flash.bytes[4..12], &[255, 255, 0, 255, 255, 255, 255, 255]);
                    assert_eq!(&flash.bytes[12..16], &[12, 0x7f, 14, 15]);
                } else {
                    assert_eq!(result, Ok(()));
                    assert_eq!(&flash.bytes[12..16], &[12, 13, 14, 15]);
                }
                assert_eq!(&flash.bytes[..4], &[0, 1, 2, 3]);
            }
        }
    }

    #[test]
    fn device_verified_erase_is_not_read_back_again() {
        for smart in [false, true] {
            // The first read after the device's own successful check is the
            // neighbor check, after both neighbors are restored, not a second
            // erase check whose failure would leave them lost.
            for fail_read in [None, Some(1)] {
                let mut flash = FakeFlash::new(64);
                flash.verifies_erase = true;
                flash.fail_read_after_erase = fail_read;
                let result = boundary_mutation(&mut flash, smart);
                assert_eq!(&flash.bytes[..4], &[0, 1, 2, 3]);
                assert_eq!(&flash.bytes[12..16], &[12, 13, 14, 15]);
                if fail_read.is_some() {
                    assert_eq!(result, Err(Error::ReadError { addr: 0 }));
                    assert_eq!(&flash.bytes[4..12], &[0xff; 8]);
                    continue;
                }
                assert_eq!(result, Ok(()));
                if !smart {
                    assert_eq!(&flash.bytes[4..12], &[0xff; 8]);
                    // One read per neighbor.
                    assert_eq!(flash.reads_after_erase, 2);
                }
            }

            // A device error during its own erase still stops all IO.
            let mut flash = FakeFlash::new(64);
            flash.verifies_erase = true;
            flash.erase_error = Some(Error::Timeout);
            assert_eq!(boundary_mutation(&mut flash, smart), Err(Error::Timeout));
            assert_eq!(&flash.bytes[12..16], &[255; 4]);
        }
    }

    #[test]
    fn unknown_device_state_stops_recovery_and_keeps_primary_error() {
        for smart in [false, true] {
            for scenario in 0..5 {
                let mut flash = FakeFlash::new(64);
                let primary = Error::EraseError(EraseFailure::VerifyFailed { addr: 6, found: 0 });
                let expected = match scenario {
                    0 => {
                        flash.erase_error = Some(Error::Timeout);
                        Error::Timeout
                    }
                    1 => {
                        flash.fail_read_after_erase = Some(1);
                        Error::ReadError { addr: 0 }
                    }
                    2 => {
                        flash.timeout_write_at = Some(0);
                        Error::Timeout
                    }
                    3 => {
                        flash.unerased = vec![(6, 0)];
                        flash.timeout_write_at = Some(0);
                        primary
                    }
                    _ => {
                        flash.unerased = vec![(6, 0)];
                        flash.fail_read_after_erase = Some(2);
                        primary
                    }
                };
                assert_eq!(boundary_mutation(&mut flash, smart), Err(expected));
                assert_eq!(&flash.bytes[12..16], &[255; 4]);
            }
        }
    }

    #[test]
    fn both_neighbors_programmed_before_first_verification() {
        for smart in [false, true] {
            let mut flash = FakeFlash::new(64);
            flash.miswritten = vec![(2, 0)];
            assert_eq!(
                boundary_mutation(&mut flash, smart),
                Err(Error::VerifyError {
                    addr: 2,
                    expected: 2,
                    found: 0,
                })
            );
            assert_eq!(&flash.bytes[12..16], &[12, 13, 14, 15]);

            let mut flash = FakeFlash::new(64);
            flash.fail_read_after_erase = Some(2);
            assert_eq!(
                boundary_mutation(&mut flash, smart),
                Err(Error::ReadError { addr: 0 })
            );
            assert_eq!(&flash.bytes[12..16], &[12, 13, 14, 15]);
        }
    }

    #[test]
    fn device_program_mismatch_does_not_abandon_other_erase_neighbors() {
        for smart in [false, true] {
            for partial in [false, true] {
                let mut flash = FakeFlash::new(64);
                flash.verifies_write = true;
                flash.miswritten = vec![(2, 0)];
                if partial {
                    flash.unerased = vec![(6, 0)];
                }
                let expected = if partial {
                    Error::EraseError(EraseFailure::VerifyFailed { addr: 6, found: 0 })
                } else {
                    Error::VerifyError {
                        addr: 2,
                        expected: 2,
                        found: 0,
                    }
                };
                assert_eq!(boundary_mutation(&mut flash, smart), Err(expected));
                assert_eq!(&flash.bytes[12..16], &[12, 13, 14, 15]);
            }
        }
    }

    #[test]
    fn missing_program_rejects_target_and_erase_neighbors_before_mutation() {
        for erase_only in [false, true] {
            let mut flash = FakeFlash::new(256);
            flash.reject_writes_from = Some(0);
            let region = Region::new("part", 2, 7);
            let result = if erase_only {
                block_on(erase_region(&mut flash, &region))
            } else {
                block_on(smart_write_region(
                    &mut flash,
                    2,
                    &[0xff; 6],
                    &mut NoProgress,
                ))
                .map(|_| ())
            };
            assert!(matches!(result, Err(Error::OpcodeNotSupported)));
            assert_eq!(flash.mutations, 0);
        }
        let mut flash = FakeFlash::new(256);
        flash.reject_writes_from = Some(0);
        assert!(matches!(
            block_on(smart_write_region(
                &mut flash,
                0,
                &[0x7f; 16],
                &mut NoProgress
            )),
            Err(Error::OpcodeNotSupported)
        ));
        assert_eq!(flash.mutations, 0);
    }

    #[test]
    fn later_region_program_is_checked_before_first_region_mutates() {
        let mut flash = FakeFlash::new(256);
        flash.reject_writes_from = Some(128);
        let mut layout = Layout::new();
        for (name, start, end) in [("first", 0, 15), ("second", 128, 143)] {
            let mut region = Region::new(name, start, end);
            region.included = true;
            layout.add_region(region);
        }
        let mut image = flash.bytes.clone();
        image[..16].fill(0x7f);
        image[128..144].fill(0x7f);
        assert!(matches!(
            block_on(smart_write_by_layout(
                &mut flash,
                &layout,
                &image,
                &mut NoProgress
            )),
            Err(Error::OpcodeNotSupported)
        ));
        assert_eq!(flash.mutations, 0);
    }

    /// Device access restrictions are checked for the actual footprint of
    /// every selected region (erase blocks and the neighbors they read,
    /// erase and restore) before the first region is changed.
    #[test]
    fn access_restrictions_are_checked_for_every_planned_footprint_first() {
        use FlashAccess::{Modify, Read};
        // The second region's 16-byte erase block spills into 136..143.
        let spilling: &[(u32, u32)] = &[(0, 15), (128, 135)];
        // The second region's block 144..159 holds unchanged data.
        let unchanged: &[(u32, u32)] = &[(0, 15), (128, 159)];
        // (regions, blocked, erase refused, smart write refused)
        for (regions, blocked, erase_refused, write_refused) in [
            (spilling, (Modify, 140, 140), true, true),
            (spilling, (Read, 143, 150), true, true),
            (spilling, (Modify, 144, 255), false, false),
            (spilling, (Read, 16, 127), false, false),
            // A smart write leaves unchanged data alone; an erase does not.
            (unchanged, (Modify, 150, 150), true, false),
            // A smart write reads its whole region to compare it.
            (unchanged, (Read, 150, 150), true, true),
        ] {
            let mut layout = Layout::new();
            for (i, &(start, end)) in regions.iter().enumerate() {
                let mut region = Region::new(alloc::format!("r{i}"), start, end);
                region.included = true;
                layout.add_region(region);
            }
            for erase in [false, true] {
                let mut flash = FakeFlash::new(256);
                flash.blocked.push(blocked);
                let mut image = flash.bytes.clone();
                image[..16].fill(0x7f);
                image[128..136].fill(0x7f);
                let result = if erase {
                    block_on(erase_by_layout(&mut flash, &layout))
                } else {
                    block_on(smart_write_by_layout(
                        &mut flash,
                        &layout,
                        &image,
                        &mut NoProgress,
                    ))
                    .map(|_| ())
                };
                if if erase { erase_refused } else { write_refused } {
                    assert_eq!(result, Err(Error::RegionProtected), "{blocked:?}");
                    assert_eq!(flash.mutations, 0);
                    continue;
                }
                result.unwrap();
                let want = if erase { 0xff } else { 0x7f };
                assert!(flash.bytes[..16].iter().all(|b| *b == want));
                assert!(flash.bytes[128..136].iter().all(|b| *b == want));
                let rest = regions[1].1 as usize + 1;
                let unchanged = if erase { 0xff } else { 0 };
                assert!(flash.bytes[136..rest].iter().all(|b| *b == unchanged));
                // Restored neighbors and untouched data.
                assert!(flash.bytes[rest..].iter().all(|b| *b == 0));
                assert!(flash.bytes[16..128].iter().all(|b| *b == 0));
            }
        }
    }

    #[test]
    fn later_content_dependent_eraser_is_checked_before_first_region() {
        let mut flash = FakeFlash::new(256);
        flash.blocks.push(EraseBlock::with_count(0xd8, 64, 4));
        flash.reject_erase_opcode = Some(0x20);
        let mut layout = Layout::new();
        for (name, start) in [("first", 0), ("second", 128)] {
            let mut region = Region::new(name, start, start + 63);
            region.included = true;
            layout.add_region(region);
        }
        let mut image = flash.bytes.clone();
        image[..64].fill(0x7f);
        image[128] = 0x7f;
        let generic = plan_optimal_erase_region(&flash.blocks, 256, 128, 191).unwrap();
        assert!(generic.iter().all(|op| op.erase_block.opcode == 0xd8));
        let actual = plan_optimal_erase(
            &flash.blocks,
            256,
            Some(&flash.bytes[128..192]),
            Some(&image[128..192]),
            128,
            191,
            WriteGranularity::Byte,
        )
        .unwrap();
        assert!(actual.iter().any(|op| op.erase_block.opcode == 0x20));
        assert!(matches!(
            block_on(smart_write_by_layout(
                &mut flash,
                &layout,
                &image,
                &mut NoProgress
            )),
            Err(Error::OpcodeNotSupported)
        ));
        assert_eq!(flash.mutations, 0);
    }

    #[test]
    fn byte_program_does_not_reprogram_unchanged_neighbors() {
        let mut flash = FakeFlash::new(256);
        flash.bytes[7] = 0xff;
        flash.require_erased_writes = true;
        let mut desired = flash.bytes.clone();
        desired[7] = 0x7f;

        block_on(smart_write_region(&mut flash, 0, &desired, &mut NoProgress)).unwrap();
        assert_eq!(flash.bytes, desired);
        assert_eq!(flash.mutations, 1);
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
        let mut whole = MutationPolicy::default();
        assert!(matches!(
            block_on(smart_write_with_policy(
                &mut flash,
                &vec![0xff; 16384],
                &mut NoProgress,
                &mut whole
            )),
            Err(Error::MutationRefused(crate::Refusal::DangerousRegion))
        ));
        assert_eq!(
            block_on(erase_region(
                &mut flash,
                &Region::new("innocent", 4096, 8191)
            )),
            Err(Error::MutationRefused(crate::Refusal::DangerousRegion))
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
        let mut policy = MutationPolicy::default();
        assert_eq!(
            block_on(smart_write_with_policy(
                &mut flash,
                &image,
                &mut NoProgress,
                &mut policy
            ))
            .unwrap_err(),
            Error::MutationRefused(crate::Refusal::UnusableDescriptor)
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
    fn failed_recovery_is_refused_before_mutation() {
        let mut flash = FakeFlash::new(64);
        let region = Region::new("partial", 4, 11);
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
    fn cross_boundary_erase_restores_neighbors_without_a_recovery_image() {
        let mut flash = FakeFlash::new(64);
        block_on(erase_region(&mut flash, &Region::new("partial", 4, 11))).unwrap();
        assert!(flash.mutations > 0);
        assert_eq!(&flash.bytes[..4], &[0; 4]);
        assert_eq!(&flash.bytes[4..12], &[0xff; 8]);
        assert_eq!(&flash.bytes[12..16], &[0; 4]);
    }

    #[test]
    fn overlapping_unselected_parent_protects_aligned_child() {
        for readonly in [true, false] {
            for allow in [false, true] {
                let mut flash = FakeFlash::new(64);
                let mut layout = Layout::new();
                let mut parent = Region::new("parent", 0, 63);
                parent.readonly = readonly;
                parent.dangerous = !readonly;
                layout.add_region(parent);
                let mut child = Region::new("child", 16, 31);
                child.included = true;
                layout.add_region(child);
                let mut policy = MutationPolicy {
                    allow_dangerous: allow,
                    ..Default::default()
                };
                let expected = if readonly {
                    crate::Refusal::ReadOnlyRegion
                } else {
                    crate::Refusal::DangerousRegion
                };
                let erase = block_on(erase_by_layout_with_policy(
                    &mut flash,
                    &layout,
                    &mut policy,
                ));
                if !readonly && allow {
                    assert_eq!(erase, Ok(()));
                    flash.bytes.fill(0);
                    block_on(smart_write_by_layout_with_policy(
                        &mut flash,
                        &layout,
                        &[0xff; 64],
                        &mut NoProgress,
                        &mut policy,
                    ))
                    .unwrap();
                    assert!(flash.mutations > 0);
                } else {
                    assert_eq!(erase, Err(Error::MutationRefused(expected)));
                    assert_eq!(
                        block_on(smart_write_by_layout_with_policy(
                            &mut flash,
                            &layout,
                            &[0xff; 64],
                            &mut NoProgress,
                            &mut policy
                        ))
                        .unwrap_err(),
                        Error::MutationRefused(expected)
                    );
                    assert_eq!(flash.mutations, 0);
                }
            }
        }
    }

    #[test]
    fn whole_chip_erase_only_needs_authorization_for_dangerous_regions() {
        let mut flash = FakeFlash::new(64);
        let mut region = Region::new("whole", 0, 63);
        region.dangerous = true;
        assert_eq!(
            block_on(erase_region_with_policy(
                &mut flash,
                &region,
                &mut MutationPolicy::default()
            )),
            Err(Error::MutationRefused(crate::Refusal::DangerousRegion))
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
            Ok(())
        );
        assert!(flash.mutations > 0);
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
            &mut MutationPolicy::default(),
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
            &mut MutationPolicy::default(),
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
            &mut MutationPolicy::default(),
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
            Err(Error::MutationRefused(crate::Refusal::ReadOnlyRegion))
        ));
        assert_eq!(device.mutations, 0);
        assert_eq!(
            block_on(erase_by_layout(&mut device, &layout)),
            Err(Error::MutationRefused(crate::Refusal::ReadOnlyRegion))
        );
        assert_eq!(device.mutations, 0);
        assert!(device.bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn verify_by_layout_with_progress_tracks_chunks_and_reports_mismatch() {
        let total = (READ_CHUNK_SIZE * 2 + 128) as u32;
        let mut device = FakeFlash::new(total);
        for (i, b) in device.bytes.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let expected = device.bytes.clone();

        let mut layout = Layout::new();
        let mut r1 = Region::new("first", 0, (READ_CHUNK_SIZE + 31) as u32);
        r1.included = true;
        let mut r2 = Region::new("second", (READ_CHUNK_SIZE + 64) as u32, total - 1);
        r2.included = true;
        layout.add_region(r1);
        layout.add_region(r2);

        let expected_total = layout
            .included_regions()
            .map(|r| r.size() as usize)
            .sum::<usize>();
        let mut progress = Vec::new();
        block_on(verify_by_layout_with_progress(
            &mut device,
            &layout,
            &expected,
            |n| progress.push(n),
        ))
        .unwrap();
        assert!(progress.len() >= 3);
        assert_eq!(progress.last().copied(), Some(expected_total));

        let mismatch_addr = READ_CHUNK_SIZE + 70;
        device.bytes[mismatch_addr] ^= 0x55;
        let mut fail_progress = Vec::new();
        let err = block_on(verify_by_layout_with_progress(
            &mut device,
            &layout,
            &expected,
            |n| fail_progress.push(n),
        ))
        .unwrap_err();
        assert_eq!(
            err,
            Error::VerifyError {
                addr: mismatch_addr as u32,
                expected: expected[mismatch_addr],
                found: device.bytes[mismatch_addr],
            }
        );
        assert_eq!(fail_progress.last().copied(), Some(READ_CHUNK_SIZE + 32));
    }

    #[test]
    fn smart_write_reads_each_region_once_and_reuses_recovery_snapshot() {
        // Full-chip smart write without recovery: 4 KiB IFD header + 1 full read.
        let mut device = FakeFlash::new(16384);
        device.bytes.fill(0xff);
        let mut data = device.bytes.clone();
        data[0x2500] = 0x12;
        block_on(smart_write_with_policy(
            &mut device,
            &data,
            &mut NoProgress,
            &mut MutationPolicy::default(),
        ))
        .unwrap();
        assert_eq!(device.read_ranges, vec![(0, 4096), (0, 16384)]);
        assert_eq!(device.bytes, data);

        // Full-chip smart write with recovery: recovery snapshot is reused, so
        // no second full-chip read occurs.
        let mut device = FakeFlash::new(16384);
        device.bytes.fill(0xff);
        let mut backup = TestBackup::default();
        let mut progress = TraceProgress::default();
        block_on(smart_write_with_policy(
            &mut device,
            &data,
            &mut progress,
            &mut MutationPolicy {
                recovery: Some(&mut backup),
                ..Default::default()
            },
        ))
        .unwrap();
        assert_eq!(device.read_ranges, vec![(0, 4096), (0, 16384)]);
        assert_eq!(backup.image.len(), 16384);
        assert_eq!(progress.reading, vec![16384]);
        assert_eq!(progress.read_progress, vec![16384]);
        assert_eq!(device.bytes, data);

        // Layout smart write without recovery: header + 1 read per included region.
        let mut layout = Layout::new();
        for (name, start, end) in [("r1", 0x2000, 0x2fff), ("r2", 0x3000, 0x3fff)] {
            let mut region = Region::new(name, start, end);
            region.included = true;
            layout.add_region(region);
        }
        let mut device = FakeFlash::new(16384);
        device.bytes.fill(0xff);
        let mut image = device.bytes.clone();
        image[0x2010] = 0x34;
        image[0x3020] = 0x56;
        block_on(smart_write_by_layout_with_policy(
            &mut device,
            &layout,
            &image,
            &mut NoProgress,
            &mut MutationPolicy::default(),
        ))
        .unwrap();
        assert_eq!(
            device.read_ranges,
            vec![(0, 4096), (0x2000, 4096), (0x3000, 4096)]
        );
        assert_eq!(device.bytes, image);

        // Layout smart write with recovery: snapshot reused for every region.
        let mut device = FakeFlash::new(16384);
        device.bytes.fill(0xff);
        let mut backup = TestBackup::default();
        let mut progress = TraceProgress::default();
        block_on(smart_write_by_layout_with_policy(
            &mut device,
            &layout,
            &image,
            &mut progress,
            &mut MutationPolicy {
                recovery: Some(&mut backup),
                ..Default::default()
            },
        ))
        .unwrap();
        assert_eq!(device.read_ranges, vec![(0, 4096), (0, 16384)]);
        assert_eq!(progress.reading, vec![8192]);
        assert_eq!(progress.read_progress, vec![4096, 8192]);
        assert_eq!(device.bytes, image);
    }

    #[test]
    fn smart_write_by_layout_disjoint_regions_sharing_erase_block() {
        let mut flash = FakeFlash::new(64);
        flash.bytes = (0..64).collect();
        flash.require_erased_writes = true;

        let mut layout = Layout::new();
        for (name, start, end) in [("first", 2, 5), ("second", 10, 13)] {
            let mut region = Region::new(name, start, end);
            region.included = true;
            layout.add_region(region);
        }

        let mut image = flash.bytes.clone();
        image[2..6].fill(0x10);
        image[10..14].fill(0x20);

        block_on(smart_write_by_layout(
            &mut flash,
            &layout,
            &image,
            &mut NoProgress,
        ))
        .unwrap();

        assert_eq!(&flash.bytes[0..2], &[0, 1]);
        assert_eq!(&flash.bytes[2..6], &[0x10; 4]);
        assert_eq!(&flash.bytes[6..10], &[6, 7, 8, 9]);
        assert_eq!(&flash.bytes[10..14], &[0x20; 4]);
        assert_eq!(&flash.bytes[14..16], &[14, 15]);
        assert_eq!(&flash.bytes[16..64], &(16..64).collect::<Vec<u8>>()[..]);
    }

    #[test]
    fn smart_write_failed_recovery_does_not_mutate_flash() {
        let mut flash = FakeFlash::new(64);
        let mut sink = TestBackup {
            fail: true,
            ..Default::default()
        };
        assert_eq!(
            block_on(smart_write_with_policy(
                &mut flash,
                &[0xff; 64],
                &mut NoProgress,
                &mut MutationPolicy {
                    recovery: Some(&mut sink),
                    ..Default::default()
                },
            ))
            .unwrap_err(),
            Error::IoError
        );
        assert_eq!(flash.mutations, 0);
    }
}

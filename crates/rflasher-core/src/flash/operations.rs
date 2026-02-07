//! High-level flash operations
//!
//! Uses `maybe_async` to support both sync and async modes.

#[cfg(feature = "alloc")]
use alloc::vec;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "std")]
use crate::chip::ChipDatabase;
use crate::chip::EraseBlock;
#[cfg(feature = "alloc")]
use crate::chip::WriteGranularity;
use crate::error::{Error, Result};
use crate::programmer::SpiMaster;
use crate::protocol;
use maybe_async::maybe_async;

use super::context::{AddressMode, FlashContext};

// =============================================================================
// Smart erase/write support
// =============================================================================

/// The erased value for flash memory (all bits set)
const ERASED_VALUE: u8 = 0xFF;

/// Determine if an erase is required to transition from `have` to `want`
///
/// Flash memory can only change bits from 1 to 0 during writes. To change
/// bits from 0 to 1, an erase is required (which sets all bits to 1).
///
/// This function checks if the transition is possible without erasing.
///
/// # Arguments
/// * `have` - Current contents of flash
/// * `want` - Desired contents
/// * `granularity` - Write granularity of the chip
///
/// # Returns
/// `true` if erasing is required, `false` if the write can proceed without erase
#[cfg(feature = "alloc")]
pub fn need_erase(have: &[u8], want: &[u8], granularity: WriteGranularity) -> bool {
    assert_eq!(have.len(), want.len());

    match granularity {
        WriteGranularity::Bit => {
            // For bit-granularity, we can only clear bits (1->0).
            // We need erase if any bit needs to go from 0->1
            // (have & want) != want means some bit in want is 1 but in have is 0
            have.iter().zip(want.iter()).any(|(h, w)| (h & w) != *w)
        }
        WriteGranularity::Byte => {
            // For byte-granularity, if bytes differ, the old byte must be
            // in erased state (0xFF) to allow writing the new value
            have.iter().zip(want.iter()).any(|(h, w)| {
                if h == w {
                    false // No change needed
                } else {
                    *h != ERASED_VALUE // Need erase if not already erased
                }
            })
        }
        WriteGranularity::Page => {
            // For page granularity, we operate on pages (256 bytes typically)
            // but the logic is the same as byte - if any byte differs,
            // the source must be erased
            have.iter().zip(want.iter()).any(
                |(h, w)| {
                    if h == w {
                        false
                    } else {
                        *h != ERASED_VALUE
                    }
                },
            )
        }
    }
}

/// Check if a range of data needs to be written (differs from current contents)
///
/// Returns `true` if any byte in `have` differs from `want`.
#[cfg(feature = "alloc")]
#[inline]
pub fn need_write(have: &[u8], want: &[u8]) -> bool {
    have != want
}

/// A contiguous range of bytes that needs to be written
#[cfg(feature = "alloc")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteRange {
    /// Start offset within the compared buffers
    pub start: u32,
    /// Length in bytes
    pub len: u32,
}

/// Find the next contiguous range of changed bytes
///
/// Starting from `offset`, finds the first byte where `have != want`,
/// then continues until finding a byte where they match again (or end of data).
///
/// This is used to skip unchanged regions and only write what's necessary.
///
/// # Returns
/// `Some(WriteRange)` if there are changes, `None` if no more changes from `offset`
#[cfg(feature = "alloc")]
pub fn get_next_write_range(have: &[u8], want: &[u8], offset: u32) -> Option<WriteRange> {
    assert_eq!(have.len(), want.len());

    let start_offset = offset as usize;
    if start_offset >= have.len() {
        return None;
    }

    // Find start of changed region
    let have_slice = &have[start_offset..];
    let want_slice = &want[start_offset..];

    let rel_start = have_slice
        .iter()
        .zip(want_slice.iter())
        .position(|(h, w)| h != w)?;

    // Find end of changed region
    let after_start = rel_start + 1;
    let rel_end = have_slice[after_start..]
        .iter()
        .zip(want_slice[after_start..].iter())
        .position(|(h, w)| h == w)
        .map(|pos| after_start + pos)
        .unwrap_or(have_slice.len());

    Some(WriteRange {
        start: (start_offset + rel_start) as u32,
        len: (rel_end - rel_start) as u32,
    })
}

/// Get all write ranges (contiguous regions of changed bytes)
#[cfg(feature = "alloc")]
pub fn get_all_write_ranges(have: &[u8], want: &[u8]) -> Vec<WriteRange> {
    let mut ranges = Vec::new();
    let mut offset = 0u32;

    while let Some(range) = get_next_write_range(have, want, offset) {
        ranges.push(range);
        offset = range.start + range.len;
    }

    ranges
}

// =============================================================================
// Optimal erase algorithm
// =============================================================================
//
// This implements a hierarchical erase block selection algorithm similar to
// flashprog. The key insight is that flash chips typically support multiple
// erase block sizes (e.g., 4KB, 32KB, 64KB), and using larger blocks is more
// efficient (fewer operations, faster overall).
//
// The algorithm:
// 1. Build a hierarchical layout where each eraser level knows which sub-blocks
//    (from the next smaller eraser) it contains.
// 2. Recursively select blocks that need erasing, starting from the smallest
//    blocks (which gives maximum granularity).
// 3. When more than 50% of a larger block's sub-blocks need erasing, "promote"
//    to using the larger block instead (fewer operations).
//
// This trades slightly more erased data for significantly fewer erase operations,
// which is typically faster due to erase command overhead.

/// Per-erase-block metadata for optimal erase planning
#[cfg(feature = "alloc")]
#[derive(Debug, Clone)]
struct EraseBlockData {
    /// Start address of this erase block
    start_addr: u32,
    /// End address of this erase block (inclusive)
    end_addr: u32,
    /// Whether this block is selected for erasure
    selected: bool,
    /// Index of the first sub-block (in the next-smaller eraser layout)
    first_sub_block_idx: usize,
    /// Index of the last sub-block (in the next-smaller eraser layout)
    last_sub_block_idx: usize,
}

/// Layout for one eraser type (all blocks of one size)
#[cfg(feature = "alloc")]
#[derive(Debug)]
struct EraserLayout {
    /// All blocks for this eraser
    blocks: Vec<EraseBlockData>,
    /// The erase block definition (opcode and size)
    erase_block: EraseBlock,
}

/// Selected erase operation in the optimal erase plan
#[cfg(feature = "alloc")]
#[derive(Debug, Clone)]
pub struct OptimalEraseOp {
    /// Start address of the erase block
    pub start: u32,
    /// Size of the erase block
    pub size: u32,
    /// The erase block definition (opcode and size)
    #[allow(dead_code)]
    pub erase_block: EraseBlock,
}

/// Create the hierarchical erase layout for a flash chip
///
/// This creates a hierarchy of eraser layouts, sorted from smallest to largest
/// erase block size. Each block in a larger eraser knows which sub-blocks from
/// the next-smaller eraser it contains.
#[cfg(feature = "alloc")]
fn create_erase_layout(erase_blocks: &[EraseBlock], flash_size: u32) -> Vec<EraserLayout> {
    // Filter out chip erase (size >= flash_size) and non-uniform blocks, sort by size (smallest first)
    let mut sorted_erasers: Vec<EraseBlock> = erase_blocks
        .iter()
        .filter(|eb| eb.is_uniform() && eb.min_block_size() > 0 && eb.min_block_size() < flash_size)
        .cloned()
        .collect();
    sorted_erasers.sort_by_key(|eb| eb.min_block_size());

    // Remove duplicates (same size, different opcode)
    sorted_erasers.dedup_by_key(|eb| eb.min_block_size());

    if sorted_erasers.is_empty() {
        return Vec::new();
    }

    let mut layouts = Vec::with_capacity(sorted_erasers.len());

    for (layout_idx, erase_block) in sorted_erasers.into_iter().enumerate() {
        let block_size = erase_block
            .uniform_size()
            .unwrap_or(erase_block.min_block_size());
        let block_count = (flash_size / block_size) as usize;
        let mut blocks = Vec::with_capacity(block_count);

        let mut sub_block_index = 0usize;

        for block_num in 0..block_count {
            let start_addr = block_num as u32 * block_size;
            let end_addr = start_addr + block_size - 1;

            let (first_sub_block_idx, last_sub_block_idx) = if layout_idx == 0 {
                // Base case: smallest blocks have no sub-blocks
                (0, 0)
            } else {
                // Calculate which sub-blocks from the previous (smaller) layout this block contains
                let sub_layout: &EraserLayout = &layouts[layout_idx - 1];
                let first = sub_block_index;

                // Find sub-blocks until we pass our end address
                while sub_block_index < sub_layout.blocks.len()
                    && sub_layout.blocks[sub_block_index].end_addr <= end_addr
                {
                    sub_block_index += 1;
                }
                let last = if sub_block_index > first {
                    sub_block_index - 1
                } else {
                    first
                };

                (first, last)
            };

            blocks.push(EraseBlockData {
                start_addr,
                end_addr,
                selected: false,
                first_sub_block_idx,
                last_sub_block_idx,
            });
        }

        layouts.push(EraserLayout {
            blocks,
            erase_block,
        });
    }

    layouts
}

/// Recursively deselect all sub-blocks when promoting to a larger block
#[cfg(feature = "alloc")]
fn deselect_erase_block_rec(layouts: &mut [EraserLayout], layout_idx: usize, block_num: usize) {
    let block = &layouts[layout_idx].blocks[block_num];

    if block.selected {
        layouts[layout_idx].blocks[block_num].selected = false;
    } else if layout_idx > 0 {
        let first = block.first_sub_block_idx;
        let last = block.last_sub_block_idx;
        for i in first..=last {
            deselect_erase_block_rec(layouts, layout_idx - 1, i);
        }
    }
}

/// Information needed for erase selection
#[cfg(feature = "alloc")]
struct SelectionInfo<'a> {
    /// Current flash contents (optional - for smart erase)
    have: Option<&'a [u8]>,
    /// Desired flash contents (optional - for smart erase)
    want: Option<&'a [u8]>,
    /// Start address of the region being erased
    region_start: u32,
    /// End address of the region being erased (inclusive)
    region_end: u32,
    /// Write granularity for need_erase checks
    granularity: WriteGranularity,
    /// Base address of the have/want buffers (usually 0 or region_start)
    buffer_offset: u32,
}

/// Recursively select erase functions with the >50% promotion heuristic
///
/// Returns the number of bytes selected for erasure.
#[cfg(feature = "alloc")]
fn select_erase_functions_rec(
    layouts: &mut [EraserLayout],
    layout_idx: usize,
    block_num: usize,
    info: &SelectionInfo<'_>,
) -> u32 {
    let block = &layouts[layout_idx].blocks[block_num];
    let block_start = block.start_addr;
    let block_end = block.end_addr;
    let block_size = block_end - block_start + 1;

    // Check if this block overlaps with our region
    if block_start > info.region_end || block_end < info.region_start {
        return 0;
    }

    if layout_idx == 0 {
        // Base case: smallest blocks - determine if erase is needed
        let needs_erase = match (info.have, info.want) {
            (Some(have), Some(want)) => {
                // Smart erase: check if we actually need to erase
                let overlap_start = block_start.max(info.region_start);
                let overlap_end = block_end.min(info.region_end);

                // Convert to buffer indices
                let buf_start = (overlap_start - info.buffer_offset) as usize;
                let buf_end = (overlap_end - info.buffer_offset + 1) as usize;

                if buf_end <= have.len() && buf_start < buf_end {
                    let have_slice = &have[buf_start..buf_end];
                    let want_slice = &want[buf_start..buf_end];

                    need_write(have_slice, want_slice)
                        && need_erase(have_slice, want_slice, info.granularity)
                } else {
                    false
                }
            }
            _ => {
                // Explicit erase: erase everything in the region
                true
            }
        };

        if needs_erase {
            layouts[layout_idx].blocks[block_num].selected = true;
            return block_size;
        }
        return 0;
    }

    // Recursive case: larger blocks
    let first_sub = layouts[layout_idx].blocks[block_num].first_sub_block_idx;
    let last_sub = layouts[layout_idx].blocks[block_num].last_sub_block_idx;

    let mut bytes = 0u32;
    for i in first_sub..=last_sub {
        bytes += select_erase_functions_rec(layouts, layout_idx - 1, i, info);
    }

    // The >50% heuristic: if more than half of this block needs erasing,
    // promote to using this larger block instead
    if bytes > block_size / 2 {
        // Only promote if the entire block is within the region
        // (we don't want to erase outside the region)
        if block_start >= info.region_start && block_end <= info.region_end {
            // Deselect all sub-blocks and select this block instead
            deselect_erase_block_rec(layouts, layout_idx, block_num);
            layouts[layout_idx].blocks[block_num].selected = true;
            return block_size;
        }
    }

    bytes
}

/// Plan optimal erase operations for a region
///
/// This analyzes the region and returns an optimal sequence of erase operations
/// that minimizes the number of erase commands while covering all necessary areas.
///
/// If the region covers the entire chip and more than 50% of the chip needs erasing,
/// a single chip erase operation will be used instead of multiple block erases.
///
/// # Arguments
/// * `erase_blocks` - Available erase block definitions for the chip
/// * `flash_size` - Total flash size in bytes
/// * `have` - Optional current flash contents (for smart erase)
/// * `want` - Optional desired flash contents (for smart erase)
/// * `region_start` - Start address of the region to erase
/// * `region_end` - End address of the region to erase (inclusive)
/// * `granularity` - Write granularity for need_erase decisions
///
/// # Returns
/// A vector of `OptimalEraseOp` describing the erase operations to perform,
/// sorted by address.
///
/// # Example
/// ```ignore
/// // For a 40KB erase starting at offset 0 on a chip with 4KB/32KB/64KB erasers:
/// // Result might be: [32KB @ 0x0000, 4KB @ 0x8000, 4KB @ 0x9000]
/// // (3 operations instead of 10 x 4KB)
///
/// // For erasing 60% of an 8MB chip:
/// // Result might be: [chip_erase @ 0x0000] (1 operation)
/// ```
#[cfg(feature = "alloc")]
pub fn plan_optimal_erase(
    erase_blocks: &[EraseBlock],
    flash_size: u32,
    have: Option<&[u8]>,
    want: Option<&[u8]>,
    region_start: u32,
    region_end: u32,
    granularity: WriteGranularity,
) -> Vec<OptimalEraseOp> {
    let mut layouts = create_erase_layout(erase_blocks, flash_size);

    if layouts.is_empty() {
        return Vec::new();
    }

    // Determine buffer offset (if buffers are region-sized vs full-chip).
    // If the buffer is smaller than the region end address + 1 (i.e. it doesn't
    // cover the full address space up to region_end), it must be a region-sized
    // buffer starting at region_start.
    let buffer_offset = match have {
        Some(h) if (h.len() as u64) < (region_end as u64 + 1) => region_start,
        _ => 0,
    };

    let info = SelectionInfo {
        have,
        want,
        region_start,
        region_end,
        granularity,
        buffer_offset,
    };

    // Start selection from the largest eraser (top-down)
    let top_layout_idx = layouts.len() - 1;
    let mut total_bytes_to_erase = 0u32;
    for block_num in 0..layouts[top_layout_idx].blocks.len() {
        total_bytes_to_erase +=
            select_erase_functions_rec(&mut layouts, top_layout_idx, block_num, &info);
    }

    // Check if chip erase would be more efficient
    // Conditions: region covers entire chip AND >50% needs erasing
    let covers_full_chip = region_start == 0 && region_end >= flash_size - 1;
    let more_than_half = total_bytes_to_erase > flash_size / 2;

    if covers_full_chip && more_than_half {
        // Find chip erase block if available
        if let Some(chip_erase_block) = erase_blocks.iter().find(|eb| eb.is_chip_erase()) {
            // Use chip erase instead of individual blocks
            return vec![OptimalEraseOp {
                start: 0,
                size: flash_size,
                erase_block: chip_erase_block.clone(),
            }];
        }
    }

    // Collect all selected blocks across all layouts
    let mut result = Vec::new();
    for layout in &layouts {
        let block_size = layout
            .erase_block
            .uniform_size()
            .unwrap_or(layout.erase_block.min_block_size());
        for block in &layout.blocks {
            if block.selected && block.start_addr <= region_end && block.end_addr >= region_start {
                result.push(OptimalEraseOp {
                    start: block.start_addr,
                    size: block_size,
                    erase_block: layout.erase_block.clone(),
                });
            }
        }
    }

    // Sort by address
    result.sort_by_key(|op| op.start);

    result
}

/// Plan optimal erase operations for a region with explicit erase (no content comparison)
///
/// This is a convenience wrapper for `plan_optimal_erase` when you want to erase
/// a region without comparing contents.
#[cfg(feature = "alloc")]
pub fn plan_optimal_erase_region(
    erase_blocks: &[EraseBlock],
    flash_size: u32,
    region_start: u32,
    region_end: u32,
) -> Vec<OptimalEraseOp> {
    plan_optimal_erase(
        erase_blocks,
        flash_size,
        None,
        None,
        region_start,
        region_end,
        WriteGranularity::Byte,
    )
}

/// Statistics from a smart write operation
#[cfg(feature = "alloc")]
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

/// Result of a comprehensive chip probe
///
/// This structure contains all information gathered during probing,
/// including SFDP data and any mismatches with the database.
#[cfg(feature = "std")]
#[derive(Debug)]
pub struct ProbeResult {
    /// JEDEC manufacturer ID
    pub jedec_manufacturer: u8,
    /// JEDEC device ID
    pub jedec_device: u16,
    /// The chip to use for operations
    pub chip: crate::chip::FlashChip,
    /// Whether the chip was found in the database
    pub from_database: bool,
    /// SFDP information (if available)
    pub sfdp: Option<crate::sfdp::SfdpInfo>,
    /// Mismatches between SFDP and database (if both available)
    pub mismatches: Vec<crate::sfdp::SfdpMismatch>,
}

#[cfg(feature = "std")]
impl ProbeResult {
    /// Check if there are any mismatches between SFDP and database
    pub fn has_mismatches(&self) -> bool {
        !self.mismatches.is_empty()
    }

    /// Check if there are critical mismatches (size/page size)
    pub fn has_critical_mismatches(&self) -> bool {
        self.mismatches.iter().any(|m| {
            matches!(
                m,
                crate::sfdp::SfdpMismatch::TotalSize { .. }
                    | crate::sfdp::SfdpMismatch::PageSize { .. }
            )
        })
    }

    /// Create a FlashContext from this probe result
    pub fn into_context(self) -> FlashContext {
        FlashContext::new(self.chip)
    }
}

/// Probe for a flash chip with detailed results
///
/// This function performs comprehensive probing:
/// 1. Reads JEDEC ID
/// 2. Probes SFDP (if supported)
/// 3. Looks up in database
/// 4. Compares SFDP with database (if both available)
///
/// Returns detailed information about what was found, allowing the caller
/// to decide how to handle mismatches or unknown chips.
#[cfg(feature = "std")]
#[maybe_async]
pub async fn probe_detailed<M: SpiMaster + ?Sized>(
    master: &mut M,
    db: &ChipDatabase,
) -> Result<ProbeResult> {
    let (jedec_manufacturer, jedec_device) = protocol::read_jedec_id(master).await?;

    log::info!(
        "JEDEC ID: manufacturer=0x{:02X}, device=0x{:04X}",
        jedec_manufacturer,
        jedec_device
    );

    // Try SFDP probing
    log::debug!("Attempting SFDP probe...");

    let sfdp = match crate::sfdp::probe(master).await {
        Ok(info) => {
            log::info!(
                "SFDP probe successful: {} bytes, page size {} bytes",
                info.total_size(),
                info.page_size()
            );
            Some(info)
        }
        Err(e) => {
            log::debug!("SFDP probe failed: {:?}", e);
            None
        }
    };

    // Look up in database
    let db_chip = db.find_by_jedec_id(jedec_manufacturer, jedec_device);
    if db_chip.is_some() {
        log::debug!("Chip found in database");
    } else {
        log::info!(
            "Chip not in database (JEDEC {:02X}:{:04X})",
            jedec_manufacturer,
            jedec_device
        );
    }

    // Determine the chip to use and collect mismatches
    let (chip, from_database, mismatches) = match (&db_chip, &sfdp) {
        (Some(db), Some(sfdp_info)) => {
            let mismatches = crate::sfdp::compare_with_chip(sfdp_info, db);
            ((*db).clone(), true, mismatches)
        }
        (Some(db), None) => ((*db).clone(), true, Vec::new()),
        (None, Some(sfdp_info)) => {
            let chip = crate::sfdp::to_flash_chip(sfdp_info, jedec_manufacturer, jedec_device);
            (chip, false, Vec::new())
        }
        (None, None) => return Err(Error::ChipNotFound),
    };

    Ok(ProbeResult {
        jedec_manufacturer,
        jedec_device,
        chip,
        from_database,
        sfdp,
        mismatches,
    })
}

/// Read flash contents
///
/// Automatically selects the best I/O mode based on programmer and chip capabilities.
/// Uses dual or quad I/O when both the programmer and chip support it.
#[maybe_async]
pub async fn read<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    use crate::chip::Features;
    use crate::spi::IoMode;

    if !ctx.is_valid_range(addr, buf.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    let chip_has_dual = ctx.chip.features.contains(Features::DUAL_IO);
    let chip_has_quad = ctx.chip.features.contains(Features::QUAD_IO);
    let use_4byte = ctx.address_mode == AddressMode::FourByte && ctx.use_native_4byte;
    let master_features = master.features();

    // Select the best read mode based on chip and programmer capabilities
    let (io_mode, _opcode) =
        protocol::select_read_mode(master_features, chip_has_dual, chip_has_quad, use_4byte);

    // Enter 4-byte mode if needed and not using native commands
    let enter_exit_4byte = ctx.address_mode == AddressMode::FourByte && !ctx.use_native_4byte;
    if enter_exit_4byte {
        protocol::enter_4byte_mode(master).await?;
    }

    let result = match io_mode {
        IoMode::Single => {
            if use_4byte {
                protocol::read_4b(master, addr, buf).await
            } else {
                protocol::read_3b(master, addr, buf).await
            }
        }
        IoMode::DualOut => {
            if use_4byte {
                protocol::read_dual_out_4b(master, addr, buf).await
            } else {
                protocol::read_dual_out_3b(master, addr, buf).await
            }
        }
        IoMode::DualIo => {
            if use_4byte {
                protocol::read_dual_io_4b(master, addr, buf).await
            } else {
                protocol::read_dual_io_3b(master, addr, buf).await
            }
        }
        IoMode::QuadOut => {
            if use_4byte {
                protocol::read_quad_out_4b(master, addr, buf).await
            } else {
                protocol::read_quad_out_3b(master, addr, buf).await
            }
        }
        IoMode::QuadIo => {
            if use_4byte {
                protocol::read_quad_io_4b(master, addr, buf).await
            } else {
                protocol::read_quad_io_3b(master, addr, buf).await
            }
        }
        IoMode::Qpi => {
            // QPI mode requires special handling - fall back to single for now
            // TODO: Implement QPI read when needed
            if use_4byte {
                protocol::read_4b(master, addr, buf).await
            } else {
                protocol::read_3b(master, addr, buf).await
            }
        }
    };

    // Exit 4-byte mode if we entered it
    if enter_exit_4byte {
        let _ = protocol::exit_4byte_mode(master).await;
    }

    result
}

/// Write data to flash
///
/// This function handles page alignment and splitting large writes
/// into page-sized chunks. The target region must be erased first.
#[maybe_async]
pub async fn write<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    data: &[u8],
) -> Result<()> {
    if !ctx.is_valid_range(addr, data.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    let page_size = ctx.page_size();
    let use_4byte = ctx.address_mode == AddressMode::FourByte;
    let use_native = ctx.use_native_4byte;

    // Enter 4-byte mode if needed and not using native commands
    if use_4byte && !use_native {
        protocol::enter_4byte_mode(master).await?;
    }

    let mut offset = 0usize;
    let mut current_addr = addr;

    while offset < data.len() {
        // Calculate how many bytes until the next page boundary
        let page_offset = (current_addr as usize) % page_size;
        let bytes_to_page_end = page_size - page_offset;
        let remaining = data.len() - offset;
        let chunk_size = core::cmp::min(bytes_to_page_end, remaining);

        let chunk = &data[offset..offset + chunk_size];

        let result = if use_4byte && use_native {
            protocol::program_page_4b(master, current_addr, chunk).await
        } else {
            protocol::program_page_3b(master, current_addr, chunk).await
        };

        if result.is_err() {
            // Try to exit 4-byte mode before returning error
            if use_4byte && !use_native {
                let _ = protocol::exit_4byte_mode(master).await;
            }
            return result;
        }

        offset += chunk_size;
        current_addr += chunk_size as u32;
    }

    // Exit 4-byte mode if we entered it
    if use_4byte && !use_native {
        protocol::exit_4byte_mode(master).await?;
    }

    Ok(())
}

/// Select the best erase block size for the given operation
///
/// Finds the largest erase block that:
/// 1. Is not a chip erase (for partial operations)
/// 2. Has a minimum block size <= the requested length
/// 3. The address is aligned to the minimum block size
/// 4. The length is a multiple of the minimum block size
pub fn select_erase_block(erase_blocks: &[EraseBlock], addr: u32, len: u32) -> Option<EraseBlock> {
    // Find the largest block size that:
    // 1. Evenly divides the length
    // 2. The address is aligned to

    erase_blocks
        .iter()
        .filter(|eb| {
            // Skip chip erase for partial operations
            // Use min_block_size() to get the individual block size (not total coverage)
            !eb.is_chip_erase() && eb.min_block_size() <= len
        })
        .filter(|eb| {
            // For uniform blocks, check alignment
            // For non-uniform blocks, we need the min block size for alignment
            let min_size = eb.min_block_size();
            addr.is_multiple_of(min_size) && len.is_multiple_of(min_size)
        })
        .max_by_key(|eb| eb.max_block_size())
        .cloned()
}

/// Map a 3-byte erase opcode to its 4-byte equivalent
///
/// Converts standard 3-byte address erase opcodes to their 4-byte variants:
/// - SE (0x20) -> SE_4B (0x21)
/// - BE_32K (0x52) -> BE_32K_4B (0x5C)  
/// - BE_64K (0xD8) -> BE_64K_4B (0xDC)
/// - Other opcodes (like chip erase) are returned unchanged
pub fn map_to_4byte_erase_opcode(opcode: u8) -> u8 {
    use crate::spi::opcodes;
    match opcode {
        opcodes::SE_20 => opcodes::SE_21,
        opcodes::BE_52 => opcodes::BE_5C,
        opcodes::BE_D8 => opcodes::BE_DC,
        _ => opcode, // Chip erase doesn't need address
    }
}

// =============================================================================
// Layout-aware operations
// =============================================================================
//
// These operations handle the case where region boundaries don't align with
// erase block boundaries. Following the same logic as flashprog:
//
// When erasing/writing a region that crosses erase boundaries:
// 1. Read the data outside the region but inside the erase block (to preserve it)
// 2. Erase the full block
// 3. Write back the preserved data
//
// This is known as a "read-modify-write" pattern for unaligned regions.

// =============================================================================
// Smart write operations - minimize erase/write based on content comparison
// =============================================================================

/// Callback for progress reporting during smart write operations
#[cfg(feature = "alloc")]
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
#[cfg(feature = "alloc")]
pub struct NoProgress;

#[cfg(feature = "alloc")]
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
// Tests
// =============================================================================

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::chip::{EraseBlock, WriteGranularity};
    use crate::spi::opcodes;

    /// Create test erase blocks for a chip of given size
    /// These have proper block counts so they aren't detected as chip erase
    fn test_erase_blocks_4k_64k(flash_size: u32) -> Vec<EraseBlock> {
        vec![
            EraseBlock::with_count(opcodes::SE_20, 4096, flash_size / 4096), // 4KB sectors
            EraseBlock::with_count(opcodes::BE_D8, 65536, flash_size / 65536), // 64KB blocks
        ]
    }

    /// Create test erase blocks with 4KB, 32KB, and 64KB options
    fn test_erase_blocks_4k_32k_64k(flash_size: u32) -> Vec<EraseBlock> {
        vec![
            EraseBlock::with_count(opcodes::SE_20, 4096, flash_size / 4096), // 4KB sectors
            EraseBlock::with_count(opcodes::BE_52, 32768, flash_size / 32768), // 32KB blocks
            EraseBlock::with_count(opcodes::BE_D8, 65536, flash_size / 65536), // 64KB blocks
        ]
    }

    // =========================================================================
    // Tests for smart write functions
    // =========================================================================

    #[test]
    fn test_need_erase_no_change() {
        // No change means no erase needed
        let have = [0xAA, 0xBB, 0xCC, 0xDD];
        let want = [0xAA, 0xBB, 0xCC, 0xDD];
        assert!(!need_erase(&have, &want, WriteGranularity::Byte));
    }

    #[test]
    fn test_need_erase_already_erased() {
        // Changing erased bytes doesn't need erase
        let have = [0xFF, 0xFF, 0xFF, 0xFF];
        let want = [0xAA, 0xBB, 0xCC, 0xDD];
        assert!(!need_erase(&have, &want, WriteGranularity::Byte));
    }

    #[test]
    fn test_need_erase_bit_clear_only() {
        // For bit granularity: 0xF0 -> 0xE0 is just clearing bit 4, OK
        let have = [0xF0];
        let want = [0xE0];
        assert!(!need_erase(&have, &want, WriteGranularity::Bit));
    }

    #[test]
    fn test_need_erase_bit_set_needed() {
        // For bit granularity: 0xE0 -> 0xF0 needs setting bit 4, requires erase
        let have = [0xE0];
        let want = [0xF0];
        assert!(need_erase(&have, &want, WriteGranularity::Bit));
    }

    #[test]
    fn test_need_erase_byte_granularity() {
        // For byte granularity: changing non-erased byte requires erase
        let have = [0xAA];
        let want = [0xBB];
        assert!(need_erase(&have, &want, WriteGranularity::Byte));
    }

    #[test]
    fn test_need_write_no_change() {
        let have = [0xAA, 0xBB, 0xCC];
        let want = [0xAA, 0xBB, 0xCC];
        assert!(!need_write(&have, &want));
    }

    #[test]
    fn test_need_write_with_change() {
        let have = [0xAA, 0xBB, 0xCC];
        let want = [0xAA, 0x00, 0xCC];
        assert!(need_write(&have, &want));
    }

    #[test]
    fn test_get_next_write_range_no_changes() {
        let have = [0xAA, 0xBB, 0xCC, 0xDD];
        let want = [0xAA, 0xBB, 0xCC, 0xDD];
        assert!(get_next_write_range(&have, &want, 0).is_none());
    }

    #[test]
    fn test_get_next_write_range_single_byte() {
        let have = [0xAA, 0xBB, 0xCC, 0xDD];
        let want = [0xAA, 0x00, 0xCC, 0xDD];

        let range = get_next_write_range(&have, &want, 0).unwrap();
        assert_eq!(range.start, 1);
        assert_eq!(range.len, 1);
    }

    #[test]
    fn test_get_next_write_range_contiguous() {
        let have = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        let want = [0xAA, 0x00, 0x00, 0x00, 0xEE];

        let range = get_next_write_range(&have, &want, 0).unwrap();
        assert_eq!(range.start, 1);
        assert_eq!(range.len, 3);
    }

    #[test]
    fn test_get_next_write_range_multiple_ranges() {
        let have = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let want = [0x00, 0xBB, 0xCC, 0x00, 0xEE, 0xFF];

        // First range at 0
        let range1 = get_next_write_range(&have, &want, 0).unwrap();
        assert_eq!(range1.start, 0);
        assert_eq!(range1.len, 1);

        // Second range at 3
        let range2 = get_next_write_range(&have, &want, range1.start + range1.len).unwrap();
        assert_eq!(range2.start, 3);
        assert_eq!(range2.len, 1);

        // No more ranges
        assert!(get_next_write_range(&have, &want, range2.start + range2.len).is_none());
    }

    #[test]
    fn test_get_all_write_ranges() {
        let have = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11];
        let want = [0x00, 0xBB, 0xCC, 0x00, 0x00, 0xFF, 0x00, 0x22];

        let ranges = get_all_write_ranges(&have, &want);
        assert_eq!(ranges.len(), 3);
        assert_eq!(ranges[0], WriteRange { start: 0, len: 1 });
        assert_eq!(ranges[1], WriteRange { start: 3, len: 2 });
        assert_eq!(ranges[2], WriteRange { start: 7, len: 1 });
    }

    // =========================================================================
    // Tests for optimal erase algorithm
    // =========================================================================

    #[test]
    fn test_optimal_erase_single_small_block() {
        // Erasing exactly one 4KB block should use the 4KB eraser
        let erase_blocks = test_erase_blocks_4k_64k(1024 * 1024); // 1MB flash

        let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 4095);

        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].start, 0);
        assert_eq!(ops[0].size, 4096);
        assert_eq!(ops[0].erase_block.opcode, opcodes::SE_20);
    }

    #[test]
    fn test_optimal_erase_promotes_to_larger_block() {
        // Erasing 40KB starting at 0 should use: 1x32KB + 2x4KB (if 32KB available)
        // or with just 4KB/64KB: all 4KB blocks (no promotion since <50% of 64KB)
        let erase_blocks = test_erase_blocks_4k_64k(1024 * 1024); // 1MB flash

        // 40KB = 10 x 4KB blocks, but that's <50% of 64KB, so no promotion
        let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 40959);

        // Should be 10 x 4KB blocks
        assert_eq!(ops.len(), 10);
        assert!(ops.iter().all(|op| op.size == 4096));
    }

    #[test]
    fn test_optimal_erase_full_64kb_block() {
        // Erasing exactly 64KB should use a single 64KB eraser
        let erase_blocks = test_erase_blocks_4k_64k(1024 * 1024); // 1MB flash

        let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 65535);

        // >50% of 64KB needs erasing (100%), so should promote to 64KB
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].start, 0);
        assert_eq!(ops[0].size, 65536);
        assert_eq!(ops[0].erase_block.opcode, opcodes::BE_D8);
    }

    #[test]
    fn test_optimal_erase_more_than_half_promotes() {
        // Erasing 36KB (>50% of 64KB = 32KB) should promote to 64KB
        let erase_blocks = test_erase_blocks_4k_64k(1024 * 1024); // 1MB flash

        // 36KB = 9 x 4KB, which is >50% of 64KB (32KB = 8 blocks)
        // But it starts at 0 and ends at 36KB-1, which is <64KB
        // The 64KB block at 0 covers 0-64KB, but we only need 0-36KB
        // Since 36KB < 64KB (end of region), the 64KB block extends past our region
        // The algorithm should NOT promote because the block would erase outside region
        let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 36863);

        // Should be 9 x 4KB blocks (no promotion because 64KB extends past 36KB)
        assert_eq!(ops.len(), 9);
        assert!(ops.iter().all(|op| op.size == 4096));
    }

    #[test]
    fn test_optimal_erase_aligned_larger_block() {
        // Erasing 64KB exactly should promote
        let erase_blocks = test_erase_blocks_4k_32k_64k(1024 * 1024); // 1MB flash

        let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 65535);

        // Should use a single 64KB erase
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].size, 65536);
    }

    #[test]
    fn test_optimal_erase_with_32kb_blocks() {
        // Erasing 48KB should use: 1x32KB + 4x4KB
        let erase_blocks = test_erase_blocks_4k_32k_64k(1024 * 1024); // 1MB flash

        // 48KB at offset 0: should promote first 32KB (8x4KB) to 32KB block
        // remaining 16KB (4x4KB) can be promoted to 32KB? No, 16KB < 16KB (50% of 32KB)
        // Wait, 16KB = 50%, so it's exactly at the boundary. Let's test >50%:
        let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 49151); // 48KB

        // 48KB < 50% of 64KB? 48KB > 32KB (50% of 64KB), but ends before 64KB
        // First 32KB: 8x4KB, but >50% of 32KB (16KB), and block is within region
        // So first 32KB should be promoted
        // Second 16KB: 4x4KB, exactly 50% of 32KB... depends on strict >
        // With >, 16KB is not >16KB, so no promotion, should be 4x4KB

        // Expected: 1x32KB + 4x4KB = 5 ops
        let total_erased: u32 = ops.iter().map(|op| op.size).sum();
        assert_eq!(total_erased, 49152, "Should erase exactly 48KB");

        // Should have a 32KB block at the start
        assert!(
            ops.iter().any(|op| op.size == 32768),
            "Should have a 32KB erase"
        );
    }

    #[test]
    fn test_optimal_erase_smart_no_erase_needed() {
        // When flash already matches, no erase should be planned
        let have = vec![0xAA; 65536];
        let want = vec![0xAA; 65536];

        let erase_blocks = test_erase_blocks_4k_64k(65536); // 64KB flash

        let ops = plan_optimal_erase(
            &erase_blocks,
            65536,
            Some(&have),
            Some(&want),
            0,
            65535,
            WriteGranularity::Byte,
        );

        assert!(ops.is_empty(), "No erase needed when contents match");
    }

    #[test]
    fn test_optimal_erase_smart_erased_flash() {
        // Writing to erased flash shouldn't need erase
        let have = vec![0xFF; 65536];
        let want = vec![0xAA; 65536];

        let erase_blocks = test_erase_blocks_4k_64k(65536); // 64KB flash

        let ops = plan_optimal_erase(
            &erase_blocks,
            65536,
            Some(&have),
            Some(&want),
            0,
            65535,
            WriteGranularity::Byte,
        );

        assert!(
            ops.is_empty(),
            "No erase needed when flash is already erased"
        );
    }

    #[test]
    fn test_optimal_erase_smart_partial_erase() {
        // Only erase the blocks that need it
        let mut have = vec![0xFF; 65536];
        have[0..4096].fill(0xAA); // First 4KB has data

        let mut want = vec![0xFF; 65536];
        want[0..4096].fill(0xBB); // Want to change first 4KB

        let erase_blocks = test_erase_blocks_4k_64k(65536); // 64KB flash

        let ops = plan_optimal_erase(
            &erase_blocks,
            65536,
            Some(&have),
            Some(&want),
            0,
            65535,
            WriteGranularity::Byte,
        );

        // Only the first 4KB block should need erasing
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].start, 0);
        assert_eq!(ops[0].size, 4096);
    }

    #[test]
    fn test_optimal_erase_offset_region() {
        // Test erasing a region that doesn't start at 0
        let erase_blocks = test_erase_blocks_4k_64k(1024 * 1024); // 1MB flash

        // Erase 8KB starting at 64KB offset
        let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 65536, 73727);

        // Should be 2 x 4KB blocks
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].start, 65536);
        assert_eq!(ops[0].size, 4096);
        assert_eq!(ops[1].start, 69632);
        assert_eq!(ops[1].size, 4096);
    }

    #[test]
    fn test_optimal_erase_empty_blocks() {
        // Empty erase blocks should return empty result
        let ops = plan_optimal_erase_region(&[], 65536, 0, 4095);
        assert!(ops.is_empty());
    }

    #[test]
    fn test_optimal_erase_chip_erase_when_beneficial() {
        // When erasing >50% of the chip and covering the full chip, use chip erase
        let flash_size = 1024 * 1024; // 1MB
        let mut erase_blocks = test_erase_blocks_4k_64k(flash_size);
        // Add chip erase
        erase_blocks.push(EraseBlock::new(0xC7, flash_size)); // Chip erase

        // Erase the entire chip
        let ops = plan_optimal_erase_region(&erase_blocks, flash_size, 0, flash_size - 1);

        // Should use a single chip erase
        assert_eq!(ops.len(), 1, "Should use chip erase for full chip");
        assert_eq!(ops[0].start, 0);
        assert_eq!(ops[0].size, flash_size);
        assert_eq!(ops[0].erase_block.opcode, 0xC7);
    }

    #[test]
    fn test_optimal_erase_chip_erase_smart_when_most_needs_erasing() {
        // When >50% needs erasing with smart erase, use chip erase
        // Use 256KB (4 x 64KB blocks) to ensure 64KB erase isn't mistaken for chip erase
        let flash_size = 256 * 1024; // 256KB
        let mut erase_blocks = test_erase_blocks_4k_64k(flash_size);
        erase_blocks.push(EraseBlock::new(0xC7, flash_size)); // Chip erase

        // Create have/want where 75% needs erasing (non-erased data changing)
        // First 75% has 0xAA that needs to change to 0xBB
        // Last 25% is unchanged (both have and want are 0xFF)
        let erase_portion = (flash_size as usize * 3) / 4; // 75%
        let mut have = vec![0xFF; flash_size as usize];
        let mut want = vec![0xFF; flash_size as usize];
        have[..erase_portion].fill(0xAA);
        want[..erase_portion].fill(0xBB);

        let ops = plan_optimal_erase(
            &erase_blocks,
            flash_size,
            Some(&have),
            Some(&want),
            0,
            flash_size - 1,
            WriteGranularity::Byte,
        );

        // Should use chip erase since >50% needs erasing
        assert_eq!(
            ops.len(),
            1,
            "Should use chip erase when >50% needs erasing, got {} ops",
            ops.len()
        );
        assert_eq!(ops[0].erase_block.opcode, 0xC7);
    }

    #[test]
    fn test_optimal_erase_no_chip_erase_when_less_than_half() {
        // When <50% needs erasing, don't use chip erase
        // Use 256KB to ensure 64KB erase isn't mistaken for chip erase
        let flash_size = 256 * 1024; // 256KB
        let mut erase_blocks = test_erase_blocks_4k_64k(flash_size);
        erase_blocks.push(EraseBlock::new(0xC7, flash_size)); // Chip erase

        // Create have/want where only 40% needs erasing
        let mut have = vec![0xFF; flash_size as usize];
        let mut want = vec![0xFF; flash_size as usize];
        // Only the first 40% needs erasing (non-erased data changing)
        let erase_portion = (flash_size as usize * 2) / 5; // 40%
        have[..erase_portion].fill(0xAA);
        want[..erase_portion].fill(0xBB);

        let ops = plan_optimal_erase(
            &erase_blocks,
            flash_size,
            Some(&have),
            Some(&want),
            0,
            flash_size - 1,
            WriteGranularity::Byte,
        );

        // Should NOT use chip erase since <50% needs erasing
        assert!(
            !ops.iter().any(|op| op.erase_block.opcode == 0xC7),
            "Should not use chip erase when <50% needs erasing"
        );
        // Should use smaller block erases
        assert!(
            ops.len() > 1,
            "Should use multiple block erases, got {}",
            ops.len()
        );
    }

    #[test]
    fn test_optimal_erase_no_chip_erase_for_partial_region() {
        // When erasing a partial region (not full chip), don't use chip erase
        let flash_size = 1024 * 1024; // 1MB
        let mut erase_blocks = test_erase_blocks_4k_64k(flash_size);
        erase_blocks.push(EraseBlock::new(0xC7, flash_size)); // Chip erase

        // Erase only 60% of the chip (which is >50%, but not full chip)
        let region_end = (flash_size as f32 * 0.6) as u32 - 1;
        let ops = plan_optimal_erase_region(&erase_blocks, flash_size, 0, region_end);

        // Should NOT use chip erase since region doesn't cover full chip
        assert!(
            !ops.iter().any(|op| op.erase_block.opcode == 0xC7),
            "Should not use chip erase for partial region"
        );
    }
}

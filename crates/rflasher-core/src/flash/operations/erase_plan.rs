//! Optimal erase-block planner.

#[cfg(feature = "alloc")]
use alloc::vec;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::diff::{need_erase, need_write};
use crate::chip::EraseBlock;
#[cfg(feature = "alloc")]
use crate::chip::WriteGranularity;
#[cfg(feature = "alloc")]
use crate::error::{Error, Result};

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
#[derive(Debug, Clone)]
pub struct OptimalEraseOp {
    /// Start address of the erase block
    pub start: u32,
    /// Size of the erase block
    pub size: u32,
    /// The erase block definition (opcode and size)
    pub erase_block: EraseBlock,
}

/// Create the hierarchical erase layout for a flash chip
///
/// This creates a hierarchy of eraser layouts, sorted from smallest to largest
/// erase block size. Each block in a larger eraser knows which sub-blocks from
/// the next-smaller eraser it contains.
#[cfg(feature = "alloc")]
fn create_erase_layout(erase_blocks: &[EraseBlock], flash_size: u32) -> Vec<EraserLayout> {
    // Exclude addressless chip erase, but retain a single addressed block
    // even when that block covers the entire chip.
    let mut sorted_erasers: Vec<EraseBlock> = erase_blocks
        .iter()
        .filter(|eb| {
            eb.is_uniform()
                && !eb.is_chip_erase()
                && eb.min_block_size() > 0
                && eb.min_block_size() <= flash_size
                && eb.total_size() == flash_size
                && flash_size.is_multiple_of(eb.min_block_size())
        })
        .cloned()
        .collect();
    sorted_erasers.sort_by_key(|eb| eb.min_block_size());

    // Remove duplicates (same size, different opcode)
    sorted_erasers.dedup_by_key(|eb| eb.min_block_size());
    // A hierarchy needs exact subdivision; otherwise upper blocks can hide
    // trailing smaller sectors. Keep a compatible subset, never guess.
    let mut previous = 0;
    sorted_erasers.retain(|eb| {
        let size = eb.min_block_size();
        if previous == 0 || size.is_multiple_of(previous) {
            previous = size;
            true
        } else {
            false
        }
    });

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
) -> Result<Vec<OptimalEraseOp>> {
    if flash_size == 0 || region_start > region_end || region_end >= flash_size {
        return Err(Error::AddressOutOfBounds);
    }
    if let (Some(have), Some(want)) = (have, want) {
        let region_len = (region_end - region_start + 1) as usize;
        if have.len() != want.len()
            || (have.len() != region_len && have.len() != flash_size as usize)
        {
            return Err(Error::BufferTooSmall);
        }
    } else if have.is_some() || want.is_some() {
        return Err(Error::BufferTooSmall);
    }
    let mut layouts = create_erase_layout(erase_blocks, flash_size);

    if layouts.is_empty() {
        // There are no uniform sector erasers. A chip-erase-only device can
        // still erase the entire chip, but never report a partial erase as done.
        if region_start == 0
            && region_end == flash_size.saturating_sub(1)
            && let Some(block) = erase_blocks
                .iter()
                .find(|block| block.is_chip_erase() && block.total_size() == flash_size)
        {
            let needs_erase = match (have, want) {
                (Some(have), Some(want)) => need_erase(have, want, granularity),
                _ => true,
            };
            return Ok(if needs_erase {
                vec![OptimalEraseOp {
                    start: 0,
                    size: flash_size,
                    erase_block: block.clone(),
                }]
            } else {
                Vec::new()
            });
        }
        return Err(Error::InvalidAlignment);
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
        if let Some(chip_erase_block) = erase_blocks
            .iter()
            .find(|eb| eb.is_chip_erase() && eb.total_size() == flash_size)
        {
            // Use chip erase instead of individual blocks
            return Ok(vec![OptimalEraseOp {
                start: 0,
                size: flash_size,
                erase_block: chip_erase_block.clone(),
            }]);
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

    Ok(result)
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
) -> Result<Vec<OptimalEraseOp>> {
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

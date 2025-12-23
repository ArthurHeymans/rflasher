//! High-level flash operations

#[cfg(feature = "alloc")]
use alloc::vec;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use crate::chip::ChipDatabase;
use crate::chip::{EraseBlock, WriteGranularity};
use crate::error::{Error, Result};
#[cfg(feature = "alloc")]
use crate::layout::{Layout, LayoutError, Region};
use crate::programmer::SpiMaster;
use crate::protocol;

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

/// Find the next contiguous range of changed bytes
///
/// Starting from `offset`, finds the first byte where `have != want`,
/// then continues until finding a byte where they match again (or end of data).
///
/// This is used to skip unchanged regions and only write what's necessary.
///
/// # Returns
/// `Some(WriteRange)` if there are changes, `None` if no more changes from `offset`
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

/// Information about an erase block and whether it needs erasing
#[cfg(feature = "alloc")]
#[derive(Debug, Clone)]
pub struct EraseBlockPlan {
    /// Start address of the erase block
    pub start: u32,
    /// Size of the erase block
    pub size: u32,
    /// The erase block definition (opcode and size)
    pub erase_block: EraseBlock,
    /// Whether this block needs to be erased
    pub needs_erase: bool,
    /// Whether this erase block extends beyond the region boundaries
    pub region_unaligned: bool,
}

/// Plan which erase blocks need to be erased for a write operation
///
/// This analyzes `have` (current contents) and `want` (desired contents) to
/// determine which erase blocks actually need erasing. Blocks where no changes
/// require erasing (i.e., all changes are 1->0 bit transitions on erased bytes)
/// are marked as not needing erase.
///
/// # Arguments
/// * `erase_blocks` - Available erase block definitions for the chip
/// * `have` - Current flash contents (can be full chip or region-sized buffer)
/// * `want` - Desired flash contents (same size as `have`)
/// * `region_start` - Start address of the region being written (absolute chip address)
/// * `region_end` - End address of the region being written (inclusive, absolute chip address)
/// * `granularity` - Write granularity of the chip
///
/// Note: If `have` and `want` are region-sized buffers (not full chip), their
/// index 0 corresponds to `region_start` on the chip.
#[cfg(feature = "alloc")]
pub fn plan_smart_erase(
    erase_blocks: &[EraseBlock],
    have: &[u8],
    want: &[u8],
    region_start: u32,
    region_end: u32,
    granularity: WriteGranularity,
) -> Result<Vec<EraseBlockPlan>> {
    assert_eq!(have.len(), want.len());

    let mut result = Vec::new();

    // Find the smallest erase block size
    let min_erase_size = erase_blocks
        .iter()
        .filter(|eb| eb.size < u32::MAX) // Exclude chip erase
        .map(|eb| eb.size)
        .min()
        .ok_or(Error::InvalidAlignment)?;

    // Determine if buffers are region-sized or full-chip sized
    // If buffers are smaller than region_end, they're region-sized (offset by region_start)
    let buffer_offset = if (have.len() as u32) < region_end {
        // Buffers are region-sized, index 0 = region_start
        region_start
    } else {
        // Buffers are full-chip sized, index 0 = address 0
        0
    };

    // Start from the first erase block boundary at or before region_start
    let first_block_start = (region_start / min_erase_size) * min_erase_size;
    let mut current_addr = first_block_start;

    while current_addr <= region_end {
        // Find the best erase block for this position
        let remaining_to_region_end = if region_end >= current_addr {
            region_end - current_addr + 1
        } else {
            0
        };

        let erase_block = erase_blocks
            .iter()
            .filter(|eb| eb.size < u32::MAX)
            .filter(|eb| current_addr.is_multiple_of(eb.size))
            .filter(|eb| eb.size <= remaining_to_region_end || eb.size == min_erase_size)
            .max_by_key(|eb| eb.size)
            .copied()
            .unwrap_or_else(|| EraseBlock::new(0x20, min_erase_size));

        let erase_start = current_addr;
        let erase_end = erase_start + erase_block.size - 1;

        // Determine if this block is unaligned (extends beyond region)
        let region_unaligned = erase_start < region_start || erase_end > region_end;

        // Calculate the overlap between this erase block and the region
        let overlap_start = erase_start.max(region_start);
        let overlap_end = erase_end.min(region_end);

        // Convert absolute addresses to buffer indices
        let buf_start = (overlap_start - buffer_offset) as usize;
        let buf_end = (overlap_end - buffer_offset + 1) as usize;

        // Check if we need to erase this block
        let needs_erase = if buf_end <= have.len() && buf_start < buf_end {
            let have_slice = &have[buf_start..buf_end];
            let want_slice = &want[buf_start..buf_end];

            // Check if we need erase for the region portion
            if !need_write(have_slice, want_slice) {
                // No changes in this portion
                false
            } else {
                need_erase(have_slice, want_slice, granularity)
            }
        } else {
            false
        };

        result.push(EraseBlockPlan {
            start: erase_start,
            size: erase_block.size,
            erase_block,
            needs_erase,
            region_unaligned,
        });

        current_addr = erase_end + 1;
    }

    Ok(result)
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

/// Probe for a flash chip using a chip database and return a context if found
#[cfg(feature = "alloc")]
pub fn probe<M: SpiMaster + ?Sized>(master: &mut M, db: &ChipDatabase) -> Result<FlashContext> {
    let (manufacturer, device) = protocol::read_jedec_id(master)?;

    let chip = db
        .find_by_jedec_id(manufacturer, device)
        .ok_or(Error::ChipNotFound)?
        .clone();

    Ok(FlashContext::new(chip))
}

/// Read the JEDEC ID from the flash chip
///
/// Returns (manufacturer_id, device_id) tuple.
pub fn read_jedec_id<M: SpiMaster + ?Sized>(master: &mut M) -> Result<(u8, u16)> {
    protocol::read_jedec_id(master)
}

/// Read flash contents
pub fn read<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    if !ctx.is_valid_range(addr, buf.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    match ctx.address_mode {
        AddressMode::ThreeByte => protocol::read_3b(master, addr, buf),
        AddressMode::FourByte => {
            if ctx.use_native_4byte {
                protocol::read_4b(master, addr, buf)
            } else {
                // Enter 4-byte mode, read, exit
                protocol::enter_4byte_mode(master)?;
                let result = protocol::read_3b(master, addr, buf);
                let _ = protocol::exit_4byte_mode(master);
                result
            }
        }
    }
}

/// Write data to flash
///
/// This function handles page alignment and splitting large writes
/// into page-sized chunks. The target region must be erased first.
pub fn write<M: SpiMaster + ?Sized>(
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
        protocol::enter_4byte_mode(master)?;
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

        // Program timeout: typical page program time is 0.7-3ms
        let timeout_us = 10_000; // 10ms

        let result = if use_4byte && use_native {
            protocol::program_page_4b(master, current_addr, chunk, timeout_us)
        } else {
            protocol::program_page_3b(master, current_addr, chunk, timeout_us)
        };

        if result.is_err() {
            // Try to exit 4-byte mode before returning error
            if use_4byte && !use_native {
                let _ = protocol::exit_4byte_mode(master);
            }
            return result;
        }

        offset += chunk_size;
        current_addr += chunk_size as u32;
    }

    // Exit 4-byte mode if we entered it
    if use_4byte && !use_native {
        protocol::exit_4byte_mode(master)?;
    }

    Ok(())
}

/// Erase a region of flash
///
/// The region must be aligned to erase block boundaries.
/// After each block erase, the erased region is verified to contain 0xFF.
pub fn erase<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    len: u32,
) -> Result<()> {
    if !ctx.is_valid_range(addr, len as usize) {
        return Err(Error::AddressOutOfBounds);
    }

    // Find the best erase block size for this operation
    let erase_block =
        select_erase_block(ctx.chip.erase_blocks(), addr, len).ok_or(Error::InvalidAlignment)?;

    let use_4byte = ctx.address_mode == AddressMode::FourByte;
    let use_native = ctx.use_native_4byte;

    // Map 3-byte opcode to 4-byte opcode if needed
    let opcode = if use_4byte && use_native {
        map_to_4byte_erase_opcode(erase_block.opcode)
    } else {
        erase_block.opcode
    };

    // Enter 4-byte mode if needed
    if use_4byte && !use_native {
        protocol::enter_4byte_mode(master)?;
    }

    let mut current_addr = addr;
    let end_addr = addr + len;

    // Erase timeout depends on block size (larger blocks take longer)
    let timeout_us = match erase_block.size {
        s if s <= 4096 => 500_000,    // 4KB: 500ms
        s if s <= 32768 => 1_000_000, // 32KB: 1s
        s if s <= 65536 => 2_000_000, // 64KB: 2s
        _ => 60_000_000,              // Chip erase: 60s
    };

    while current_addr < end_addr {
        let result = protocol::erase_block(
            master,
            opcode,
            current_addr,
            use_4byte && use_native,
            timeout_us,
        );

        if result.is_err() {
            if use_4byte && !use_native {
                let _ = protocol::exit_4byte_mode(master);
            }
            return result;
        }

        // Verify the block was erased (same as flashprog's check_erased_range)
        if let Err(e) = check_erased_range(master, ctx, current_addr, erase_block.size) {
            if use_4byte && !use_native {
                let _ = protocol::exit_4byte_mode(master);
            }
            return Err(e);
        }

        current_addr += erase_block.size;
    }

    // Exit 4-byte mode
    if use_4byte && !use_native {
        protocol::exit_4byte_mode(master)?;
    }

    Ok(())
}

/// Erase the entire chip
///
/// This function erases the chip and then verifies the erase by reading back
/// the contents and checking they are all 0xFF.
pub fn chip_erase<M: SpiMaster + ?Sized>(master: &mut M, ctx: &FlashContext) -> Result<()> {
    // Chip erase timeout: up to 2 minutes for large chips
    let timeout_us = 120_000_000;
    protocol::chip_erase(master, timeout_us)?;

    // Verify the erase succeeded by checking the chip contents
    check_erased_range(master, ctx, 0, ctx.total_size() as u32)
}

/// Check that a range of flash has been erased (all bytes are 0xFF)
///
/// This function reads the specified range and verifies that all bytes
/// contain the erased value (0xFF). This is used to verify erase operations.
fn check_erased_range<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    len: u32,
) -> Result<()> {
    // Read in chunks to avoid allocating the entire chip size at once
    const CHUNK_SIZE: usize = 4096;
    let mut buf = [0u8; CHUNK_SIZE];

    let mut offset = 0u32;
    while offset < len {
        let chunk_len = core::cmp::min(CHUNK_SIZE as u32, len - offset) as usize;
        let chunk_buf = &mut buf[..chunk_len];

        read(master, ctx, addr + offset, chunk_buf)?;

        // Check all bytes are erased
        for &byte in chunk_buf.iter() {
            if byte != ERASED_VALUE {
                return Err(Error::EraseError);
            }
        }

        offset += chunk_len as u32;
    }

    Ok(())
}

/// Verify flash contents match the provided data
pub fn verify<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    expected: &[u8],
    buf: &mut [u8],
) -> Result<()> {
    if !ctx.is_valid_range(addr, expected.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    if buf.len() < expected.len() {
        return Err(Error::BufferTooSmall);
    }

    let verify_buf = &mut buf[..expected.len()];
    read(master, ctx, addr, verify_buf)?;

    if verify_buf != expected {
        return Err(Error::VerifyError);
    }

    Ok(())
}

/// Select the best erase block size for the given operation
fn select_erase_block(erase_blocks: &[EraseBlock], addr: u32, len: u32) -> Option<EraseBlock> {
    // Find the largest block size that:
    // 1. Evenly divides the length
    // 2. The address is aligned to

    erase_blocks
        .iter()
        .filter(|eb| {
            // Skip chip erase for partial operations
            eb.size <= len
        })
        .filter(|eb| addr.is_multiple_of(eb.size) && len.is_multiple_of(eb.size))
        .max_by_key(|eb| eb.size)
        .copied()
}

/// Map a 3-byte erase opcode to its 4-byte equivalent
fn map_to_4byte_erase_opcode(opcode: u8) -> u8 {
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

/// Information about an erase block and its relationship to a region
#[cfg(feature = "alloc")]
#[derive(Debug, Clone)]
struct EraseBlockInfo {
    /// Start address of the erase block
    erase_start: u32,
    /// End address of the erase block (inclusive)
    erase_end: u32,
    /// The erase block definition (opcode and size)
    erase_block: EraseBlock,
    /// Whether this block is unaligned (extends beyond region boundaries)
    region_unaligned: bool,
}

/// Plan erase operations for a region, returning the list of erase blocks needed
///
/// This handles regions that don't align to erase block boundaries by planning
/// to erase full blocks and tracking which blocks are "unaligned" (extend beyond
/// the region).
#[cfg(feature = "alloc")]
fn plan_erase_for_region(
    erase_blocks: &[EraseBlock],
    region_start: u32,
    region_end: u32,
) -> Result<Vec<EraseBlockInfo>> {
    let mut result = Vec::new();

    // Find the smallest erase block size
    let min_erase_size = erase_blocks
        .iter()
        .filter(|eb| eb.size < u32::MAX) // Exclude chip erase
        .map(|eb| eb.size)
        .min()
        .ok_or(Error::InvalidAlignment)?;

    // Start from the first erase block boundary at or before region_start
    let first_block_start = (region_start / min_erase_size) * min_erase_size;
    let mut current_addr = first_block_start;

    while current_addr <= region_end {
        // Find the best erase block for this position
        // At block-aligned addresses, we can use larger blocks
        // We want the largest block that:
        // 1. The current address is aligned to
        // 2. Fits within a reasonable range (or is the minimum size)
        let remaining_to_region_end = if region_end >= current_addr {
            region_end - current_addr + 1
        } else {
            0
        };

        let erase_block = erase_blocks
            .iter()
            .filter(|eb| eb.size < u32::MAX) // Exclude chip erase
            .filter(|eb| current_addr.is_multiple_of(eb.size))
            .filter(|eb| {
                // Prefer blocks that fit in the remaining region, but allow
                // the minimum size even if it extends past
                eb.size <= remaining_to_region_end || eb.size == min_erase_size
            })
            .max_by_key(|eb| eb.size)
            .copied()
            .unwrap_or_else(|| {
                // Fallback to smallest block at its aligned boundary
                EraseBlock::new(
                    erase_blocks
                        .iter()
                        .find(|eb| eb.size == min_erase_size)
                        .map(|eb| eb.opcode)
                        .unwrap_or(0x20),
                    min_erase_size,
                )
            });

        let erase_start = current_addr;
        let erase_end = erase_start + erase_block.size - 1;

        // Check if this erase block extends beyond the region boundaries
        let region_unaligned = erase_start < region_start || erase_end > region_end;

        result.push(EraseBlockInfo {
            erase_start,
            erase_end,
            erase_block,
            region_unaligned,
        });

        current_addr = erase_end + 1;
    }

    Ok(result)
}

/// Erase a single block, handling unaligned regions by preserving data outside the region
///
/// This implements the same read-modify-write logic as flashprog's `erase_block` function.
/// When a region doesn't align with erase block boundaries:
/// 1. Read data before the region (from erase_start to region_start-1)
/// 2. Read data after the region (from region_end+1 to erase_end)
/// 3. Erase the full block
/// 4. Write back the preserved data
#[cfg(feature = "alloc")]
fn erase_block_with_preserve<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    info: &EraseBlockInfo,
    region_start: u32,
    region_end: u32,
) -> Result<()> {
    let erase_len = info.erase_end - info.erase_start + 1;

    if info.region_unaligned {
        // Allocate backup buffer filled with erased value (0xFF)
        let mut backup_contents = vec![0xFFu8; erase_len as usize];

        // Read data preceding the region (to preserve)
        if region_start > info.erase_start {
            let start = info.erase_start;
            let len = region_start - info.erase_start;
            read(master, ctx, start, &mut backup_contents[..len as usize])?;
        }

        // Read data following the region (to preserve)
        if info.erase_end > region_end {
            let start = region_end + 1;
            let rel_start = (start - info.erase_start) as usize;
            let len = info.erase_end - region_end;
            read(
                master,
                ctx,
                start,
                &mut backup_contents[rel_start..rel_start + len as usize],
            )?;
        }

        // Erase the full block
        erase_single_block(master, ctx, info.erase_block, info.erase_start)?;

        // Write back the preserved data (only the parts we read)
        // We need to write back data before the region
        if region_start > info.erase_start {
            let len = (region_start - info.erase_start) as usize;
            write(master, ctx, info.erase_start, &backup_contents[..len])?;
        }

        // Write back data after the region
        if info.erase_end > region_end {
            let start = region_end + 1;
            let rel_start = (start - info.erase_start) as usize;
            let len = (info.erase_end - region_end) as usize;
            write(
                master,
                ctx,
                start,
                &backup_contents[rel_start..rel_start + len],
            )?;
        }
    } else {
        // Block is aligned with region, just erase it
        erase_single_block(master, ctx, info.erase_block, info.erase_start)?;
    }

    Ok(())
}

/// Erase a single block using the specified erase block definition
///
/// After erasing, the block is verified to contain 0xFF.
#[cfg(feature = "alloc")]
fn erase_single_block<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    erase_block: EraseBlock,
    addr: u32,
) -> Result<()> {
    let use_4byte = ctx.address_mode == AddressMode::FourByte;
    let use_native = ctx.use_native_4byte;

    let opcode = if use_4byte && use_native {
        map_to_4byte_erase_opcode(erase_block.opcode)
    } else {
        erase_block.opcode
    };

    // Enter 4-byte mode if needed
    if use_4byte && !use_native {
        protocol::enter_4byte_mode(master)?;
    }

    // Calculate timeout based on block size
    let timeout_us = match erase_block.size {
        s if s <= 4096 => 500_000,    // 4KB: 500ms
        s if s <= 32768 => 1_000_000, // 32KB: 1s
        s if s <= 65536 => 2_000_000, // 64KB: 2s
        _ => 60_000_000,              // Larger: 60s
    };

    let result = protocol::erase_block(master, opcode, addr, use_4byte && use_native, timeout_us);

    // Exit 4-byte mode
    if use_4byte && !use_native {
        let _ = protocol::exit_4byte_mode(master);
    }

    result?;

    // Verify the block was erased (same as flashprog's check_erased_range)
    check_erased_range(master, ctx, addr, erase_block.size)
}

/// Erase a region of flash, handling erase block boundary crossing
///
/// Unlike the basic `erase` function which requires alignment, this function
/// handles regions that don't align to erase block boundaries by:
/// 1. Reading data outside the region but inside affected erase blocks
/// 2. Erasing the full blocks
/// 3. Writing back the preserved data
///
/// This matches flashprog's behavior for layout-based operations.
#[cfg(feature = "alloc")]
pub fn erase_region<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    region: &Region,
) -> Result<()> {
    if !ctx.is_valid_range(region.start, region.size() as usize) {
        return Err(Error::AddressOutOfBounds);
    }

    // Plan the erase operations
    let erase_plan = plan_erase_for_region(ctx.chip.erase_blocks(), region.start, region.end)?;

    // Execute each erase block
    for info in &erase_plan {
        erase_block_with_preserve(master, ctx, info, region.start, region.end)?;
    }

    Ok(())
}

/// Erase all included regions in a layout
///
/// This function iterates through all included regions in the layout and
/// erases them, properly handling erase block boundary crossing.
#[cfg(feature = "alloc")]
pub fn erase_by_layout<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    layout: &Layout,
) -> Result<()> {
    // Validate layout against chip
    layout
        .validate(ctx.chip.total_size)
        .map_err(|_| Error::AddressOutOfBounds)?;

    // Erase each included region
    for region in layout.included_regions() {
        erase_region(master, ctx, region)?;
    }

    Ok(())
}

/// Write data to a region, handling erase block boundary crossing
///
/// This function:
/// 1. Reads the current contents of affected erase blocks
/// 2. Erases the blocks (preserving data outside the region)
/// 3. Writes the new data to the region
///
/// The `data` slice must match the region size.
#[cfg(feature = "alloc")]
pub fn write_region<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    region: &Region,
    data: &[u8],
) -> Result<()> {
    if data.len() != region.size() as usize {
        return Err(Error::BufferTooSmall);
    }

    if !ctx.is_valid_range(region.start, region.size() as usize) {
        return Err(Error::AddressOutOfBounds);
    }

    // Erase the region first (this handles boundary crossing)
    erase_region(master, ctx, region)?;

    // Write the new data
    write(master, ctx, region.start, data)?;

    Ok(())
}

/// Write data to all included regions from an image buffer
///
/// This function takes a full chip image and writes only the included regions.
/// Each region is erased (with boundary handling) before writing.
///
/// The `image` buffer must be at least as large as `chip_size`.
#[cfg(feature = "alloc")]
pub fn write_by_layout<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    layout: &Layout,
    image: &[u8],
) -> Result<()> {
    // Validate layout against chip
    layout
        .validate(ctx.chip.total_size)
        .map_err(|_| Error::AddressOutOfBounds)?;

    // Image must cover the chip
    if image.len() < ctx.chip.total_size as usize {
        return Err(Error::BufferTooSmall);
    }

    // Write each included region
    for region in layout.included_regions() {
        let region_data = &image[region.start as usize..=region.end as usize];
        write_region(master, ctx, region, region_data)?;
    }

    Ok(())
}

/// Read all included regions from flash into a buffer
///
/// This function reads only the included regions into the provided buffer.
/// Regions that are not included will be left unchanged in the buffer.
///
/// The `buffer` must be at least as large as `chip_size`.
#[cfg(feature = "alloc")]
pub fn read_by_layout<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    layout: &Layout,
    buffer: &mut [u8],
) -> Result<()> {
    // Validate layout against chip
    layout
        .validate(ctx.chip.total_size)
        .map_err(|_| Error::AddressOutOfBounds)?;

    // Buffer must cover the chip
    if buffer.len() < ctx.chip.total_size as usize {
        return Err(Error::BufferTooSmall);
    }

    // Read each included region
    for region in layout.included_regions() {
        let region_buf = &mut buffer[region.start as usize..=region.end as usize];
        read(master, ctx, region.start, region_buf)?;
    }

    Ok(())
}

/// Verify that flash contents match the expected data for all included regions
///
/// This function reads each included region and compares it against the
/// expected image data.
#[cfg(feature = "alloc")]
pub fn verify_by_layout<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    layout: &Layout,
    expected: &[u8],
) -> Result<()> {
    // Validate layout against chip
    layout
        .validate(ctx.chip.total_size)
        .map_err(|_| Error::AddressOutOfBounds)?;

    // Expected buffer must cover the chip
    if expected.len() < ctx.chip.total_size as usize {
        return Err(Error::BufferTooSmall);
    }

    // Allocate a read buffer
    let mut read_buf = vec![0u8; ctx.chip.total_size as usize];

    // Verify each included region
    for region in layout.included_regions() {
        let region_buf = &mut read_buf[region.start as usize..=region.end as usize];
        read(master, ctx, region.start, region_buf)?;

        let expected_region = &expected[region.start as usize..=region.end as usize];
        if region_buf != expected_region {
            return Err(Error::VerifyError);
        }
    }

    Ok(())
}

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

/// Perform a smart write operation that minimizes flash operations
///
/// This function compares the current flash contents with the desired contents
/// and only erases/writes the regions that actually need to change. This is
/// inspired by flashprog's optimization strategy:
///
/// 1. Read current flash contents
/// 2. Compare with desired contents to find changed regions
/// 3. For each changed region, determine if erase is needed (based on bit transitions)
/// 4. Erase only the blocks that need erasing
/// 5. Write only the bytes that are different
///
/// # Arguments
/// * `master` - SPI master for flash communication
/// * `ctx` - Flash context with chip information
/// * `data` - Desired flash contents (must match chip size)
/// * `progress` - Progress callback (use `NoProgress` if not needed)
///
/// # Returns
/// Statistics about the operations performed
#[cfg(feature = "alloc")]
pub fn smart_write<M: SpiMaster + ?Sized, P: WriteProgress>(
    master: &mut M,
    ctx: &FlashContext,
    data: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    let chip_size = ctx.total_size();

    if data.len() != chip_size {
        return Err(Error::BufferTooSmall);
    }

    let mut stats = WriteStats::default();

    // Step 1: Read current flash contents
    progress.reading(chip_size);
    let mut current = vec![0u8; chip_size];

    const READ_CHUNK_SIZE: usize = 4096;
    let mut bytes_read = 0;
    while bytes_read < chip_size {
        let chunk_size = core::cmp::min(READ_CHUNK_SIZE, chip_size - bytes_read);
        read(
            master,
            ctx,
            bytes_read as u32,
            &mut current[bytes_read..bytes_read + chunk_size],
        )?;
        bytes_read += chunk_size;
        progress.read_progress(bytes_read);
    }

    // Step 2: Find all regions that need to be written
    let write_ranges = get_all_write_ranges(&current, data);

    if write_ranges.is_empty() {
        // Nothing to do - flash already matches
        progress.complete(&stats);
        return Ok(stats);
    }

    // Calculate total changed bytes
    stats.bytes_changed = write_ranges.iter().map(|r| r.len as usize).sum();

    // Step 3: Plan erase operations for each changed region
    // We need to check which erase blocks contain changes that require erasing
    let granularity = ctx.chip.write_granularity;
    let mut erase_plan = Vec::new();

    for range in &write_ranges {
        let have_slice = &current[range.start as usize..(range.start + range.len) as usize];
        let want_slice = &data[range.start as usize..(range.start + range.len) as usize];

        if need_erase(have_slice, want_slice, granularity) {
            // Plan erase for this range
            let range_plan = plan_smart_erase(
                ctx.chip.erase_blocks(),
                &current,
                data,
                range.start,
                range.start + range.len - 1,
                granularity,
            )?;

            // Merge with existing plan (avoid duplicate erases)
            for block in range_plan {
                if block.needs_erase
                    && !erase_plan
                        .iter()
                        .any(|b: &EraseBlockPlan| b.start == block.start)
                {
                    erase_plan.push(block);
                }
            }
        }
    }

    // Step 4: Execute erase operations
    if !erase_plan.is_empty() {
        let bytes_to_erase: usize = erase_plan.iter().map(|b| b.size as usize).sum();
        progress.erasing(erase_plan.len(), bytes_to_erase);

        let mut blocks_erased = 0;
        let mut bytes_erased = 0;

        for block in &erase_plan {
            // For blocks that extend beyond our write ranges, we need to preserve
            // data outside those ranges
            erase_block_smart(master, ctx, block, &current)?;

            blocks_erased += 1;
            bytes_erased += block.size as usize;
            progress.erase_progress(blocks_erased, bytes_erased);
        }

        stats.erases_performed = blocks_erased;
        stats.bytes_erased = bytes_erased;
        stats.flash_modified = true;

        // Update our view of current contents (erased blocks are now 0xFF)
        for block in &erase_plan {
            for i in 0..block.size as usize {
                let addr = block.start as usize + i;
                if addr < current.len() {
                    current[addr] = ERASED_VALUE;
                }
            }
        }
    }

    // Step 5: Write only the changed bytes
    // Re-calculate write ranges after erasing (some bytes may now match after erase)
    let write_ranges = get_all_write_ranges(&current, data);

    if !write_ranges.is_empty() {
        let bytes_to_write: usize = write_ranges.iter().map(|r| r.len as usize).sum();
        progress.writing(bytes_to_write);

        let mut bytes_written = 0;

        for range in &write_ranges {
            let write_data = &data[range.start as usize..(range.start + range.len) as usize];
            write(master, ctx, range.start, write_data)?;

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

/// Erase a block, preserving data that shouldn't be erased
///
/// This handles the case where an erase block contains data that we don't
/// want to modify. It reads the data before erasing and writes it back after.
#[cfg(feature = "alloc")]
fn erase_block_smart<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    block: &EraseBlockPlan,
    current_contents: &[u8],
) -> Result<()> {
    // We need to preserve data at the edges of the erase block that
    // we're not actually modifying
    let block_start = block.start as usize;
    let block_end = block_start + block.size as usize;

    // Read the current block contents that might need preserving
    // (this is the data in the erase block that's not being changed)
    let backup = current_contents[block_start..block_end].to_vec();

    // Erase the block
    erase_single_block(master, ctx, block.erase_block, block.start)?;

    // The caller is responsible for writing new data
    // We don't need to restore here since we only erase blocks
    // where we're going to write new data anyway

    // But if the block is unaligned, we may need to restore edges
    // that aren't part of our write - this is handled by the fact
    // that smart_write only erases blocks that have actual changes,
    // and we write all the changed data after

    let _ = backup; // Will be used if we need edge preservation

    Ok(())
}

/// Perform a smart write operation for a specific region
///
/// Similar to `smart_write` but only operates on a specific region of flash.
/// Data outside the region is preserved.
///
/// # Arguments
/// * `master` - SPI master for flash communication
/// * `ctx` - Flash context with chip information
/// * `addr` - Start address of the region
/// * `data` - Data to write to the region
/// * `progress` - Progress callback
#[cfg(feature = "alloc")]
pub fn smart_write_region<M: SpiMaster + ?Sized, P: WriteProgress>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    data: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    if !ctx.is_valid_range(addr, data.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    let mut stats = WriteStats::default();
    let granularity = ctx.chip.write_granularity;

    // Step 1: Read current contents of the region
    progress.reading(data.len());
    let mut current = vec![0u8; data.len()];

    const READ_CHUNK_SIZE: usize = 4096;
    let mut bytes_read = 0;
    while bytes_read < data.len() {
        let chunk_size = core::cmp::min(READ_CHUNK_SIZE, data.len() - bytes_read);
        read(
            master,
            ctx,
            addr + bytes_read as u32,
            &mut current[bytes_read..bytes_read + chunk_size],
        )?;
        bytes_read += chunk_size;
        progress.read_progress(bytes_read);
    }

    // Step 2: Find changed regions
    let write_ranges = get_all_write_ranges(&current, data);

    if write_ranges.is_empty() {
        progress.complete(&stats);
        return Ok(stats);
    }

    stats.bytes_changed = write_ranges.iter().map(|r| r.len as usize).sum();

    // Step 3: Check if any changes need erasing
    let needs_any_erase = write_ranges.iter().any(|range| {
        let have = &current[range.start as usize..(range.start + range.len) as usize];
        let want = &data[range.start as usize..(range.start + range.len) as usize];
        need_erase(have, want, granularity)
    });

    // Step 4: Erase if needed
    if needs_any_erase {
        // Plan erase for the entire write region
        let region_end = addr + data.len() as u32 - 1;
        let erase_plan = plan_smart_erase(
            ctx.chip.erase_blocks(),
            &current,
            data,
            addr,
            region_end,
            granularity,
        )?;

        let blocks_to_erase: Vec<_> = erase_plan.into_iter().filter(|b| b.needs_erase).collect();

        if !blocks_to_erase.is_empty() {
            let bytes_to_erase: usize = blocks_to_erase.iter().map(|b| b.size as usize).sum();
            progress.erasing(blocks_to_erase.len(), bytes_to_erase);

            // We need to read data outside our region but inside erase blocks
            // to preserve it after erasing
            for block in &blocks_to_erase {
                // Handle data before our region
                if block.start < addr {
                    let preserve_len = (addr - block.start) as usize;
                    let mut preserve_data = vec![0u8; preserve_len];
                    read(master, ctx, block.start, &mut preserve_data)?;

                    // Erase and restore
                    erase_single_block(master, ctx, block.erase_block, block.start)?;
                    write(master, ctx, block.start, &preserve_data)?;

                    stats.erases_performed += 1;
                    stats.bytes_erased += block.size as usize;
                } else if block.start + block.size > addr + data.len() as u32 {
                    // Handle data after our region
                    let region_end = addr + data.len() as u32;
                    let preserve_start = region_end;
                    let preserve_len = (block.start + block.size - region_end) as usize;

                    let mut preserve_data = vec![0u8; preserve_len];
                    read(master, ctx, preserve_start, &mut preserve_data)?;

                    erase_single_block(master, ctx, block.erase_block, block.start)?;
                    write(master, ctx, preserve_start, &preserve_data)?;

                    stats.erases_performed += 1;
                    stats.bytes_erased += block.size as usize;
                } else {
                    // Block is entirely within our region
                    erase_single_block(master, ctx, block.erase_block, block.start)?;
                    stats.erases_performed += 1;
                    stats.bytes_erased += block.size as usize;
                }

                progress.erase_progress(stats.erases_performed, stats.bytes_erased);
            }

            stats.flash_modified = true;

            // Update current to reflect erased state
            for block in &blocks_to_erase {
                let rel_start = block.start.saturating_sub(addr) as usize;
                let rel_end =
                    ((block.start + block.size).saturating_sub(addr) as usize).min(current.len());
                for byte in &mut current[rel_start..rel_end] {
                    *byte = ERASED_VALUE;
                }
            }
        }
    }

    // Step 5: Write only changed bytes (recalculate after erase)
    let write_ranges = get_all_write_ranges(&current, data);

    if !write_ranges.is_empty() {
        let bytes_to_write: usize = write_ranges.iter().map(|r| r.len as usize).sum();
        progress.writing(bytes_to_write);

        let mut bytes_written = 0;

        for range in &write_ranges {
            let write_data = &data[range.start as usize..(range.start + range.len) as usize];
            write(master, ctx, addr + range.start, write_data)?;

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
/// This function writes data to all included regions in the layout,
/// using smart write semantics (only erasing/writing changed bytes).
///
/// # Arguments
/// * `master` - SPI master for flash communication
/// * `ctx` - Flash context with chip information
/// * `layout` - Layout with regions marked as included
/// * `image` - Full flash image (must be at least chip size)
/// * `progress` - Progress callback
///
/// # Returns
/// Combined statistics about all operations performed
#[cfg(feature = "alloc")]
pub fn smart_write_by_layout<M: SpiMaster + ?Sized, P: WriteProgress>(
    master: &mut M,
    ctx: &FlashContext,
    layout: &Layout,
    image: &[u8],
    progress: &mut P,
) -> Result<WriteStats> {
    // Validate layout against chip
    layout.validate(ctx.chip.total_size).map_err(|e| match e {
        LayoutError::RegionOutOfBounds => Error::AddressOutOfBounds,
        LayoutError::ChipSizeMismatch { .. } => Error::AddressOutOfBounds,
        _ => Error::LayoutError,
    })?;

    // Image must cover the chip (or at least the included regions)
    if image.len() < ctx.chip.total_size as usize {
        return Err(Error::BufferTooSmall);
    }

    // Collect included regions
    let included: Vec<_> = layout.included_regions().collect();
    if included.is_empty() {
        // Nothing to do
        let stats = WriteStats::default();
        progress.complete(&stats);
        return Ok(stats);
    }

    // Calculate total bytes across all included regions
    let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();

    let mut combined_stats = WriteStats::default();

    // For progress reporting, we track overall progress across all regions
    let mut overall_bytes_read = 0usize;
    let mut overall_bytes_written = 0usize;

    // Read phase: read all included regions
    progress.reading(total_bytes);

    // We'll process each region and accumulate stats
    for region in &included {
        let region_data = &image[region.start as usize..=region.end as usize];

        // Create a sub-progress reporter that offsets the overall progress
        #[allow(dead_code)]
        struct OffsetProgress<'a, P: WriteProgress> {
            inner: &'a mut P,
            read_offset: usize,
            write_offset: usize,
            total_read: usize,
            total_write: usize,
        }

        impl<P: WriteProgress> WriteProgress for OffsetProgress<'_, P> {
            fn reading(&mut self, _total_bytes: usize) {
                // Already called at the outer level
            }
            fn read_progress(&mut self, bytes_read: usize) {
                self.inner.read_progress(self.read_offset + bytes_read);
            }
            fn erasing(&mut self, blocks_to_erase: usize, bytes_to_erase: usize) {
                self.inner.erasing(blocks_to_erase, bytes_to_erase);
            }
            fn erase_progress(&mut self, blocks_erased: usize, bytes_erased: usize) {
                self.inner.erase_progress(blocks_erased, bytes_erased);
            }
            fn writing(&mut self, _bytes_to_write: usize) {
                // Use total write across all regions
                self.inner.writing(self.total_write);
            }
            fn write_progress(&mut self, bytes_written: usize) {
                self.inner.write_progress(self.write_offset + bytes_written);
            }
            fn complete(&mut self, _stats: &WriteStats) {
                // Don't call complete for individual regions
            }
        }

        let mut offset_progress = OffsetProgress {
            inner: progress,
            read_offset: overall_bytes_read,
            write_offset: overall_bytes_written,
            total_read: total_bytes,
            total_write: total_bytes,
        };

        let stats =
            smart_write_region(master, ctx, region.start, region_data, &mut offset_progress)?;

        // Accumulate stats
        combined_stats.bytes_changed += stats.bytes_changed;
        combined_stats.erases_performed += stats.erases_performed;
        combined_stats.bytes_erased += stats.bytes_erased;
        combined_stats.writes_performed += stats.writes_performed;
        combined_stats.bytes_written += stats.bytes_written;
        combined_stats.flash_modified |= stats.flash_modified;

        overall_bytes_read += region.size() as usize;
        overall_bytes_written += stats.bytes_written;
    }

    progress.complete(&combined_stats);
    Ok(combined_stats)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::chip::{EraseBlock, Features, FlashChip, WriteGranularity};
    use crate::layout::{Layout, LayoutSource, Region};
    use crate::programmer::SpiFeatures;
    use crate::spi::opcodes;
    use crate::spi::SpiCommand;
    use alloc::string::ToString;
    use std::cell::RefCell;

    /// A mock SPI master that simulates flash memory for testing
    ///
    /// This mock tracks all operations and simulates flash behavior:
    /// - Memory starts as all 0xFF (erased state)
    /// - Erase operations set regions to 0xFF
    /// - Write operations modify memory (only 1->0 transitions in real flash)
    struct MockFlash {
        /// Simulated flash memory contents
        memory: RefCell<Vec<u8>>,
        /// Record of all erase operations: (address, size)
        erases: RefCell<Vec<(u32, u32)>>,
        /// Record of all write operations: (address, data)
        writes: RefCell<Vec<(u32, Vec<u8>)>>,
        /// Record of all read operations: (address, length)
        reads: RefCell<Vec<(u32, usize)>>,
    }

    impl MockFlash {
        fn new(size: usize) -> Self {
            Self {
                memory: RefCell::new(vec![0xFF; size]),
                erases: RefCell::new(Vec::new()),
                writes: RefCell::new(Vec::new()),
                reads: RefCell::new(Vec::new()),
            }
        }

        /// Initialize memory with specific contents
        fn with_contents(size: usize, contents: &[(u32, &[u8])]) -> Self {
            let mock = Self::new(size);
            for (addr, data) in contents {
                let addr = *addr as usize;
                mock.memory.borrow_mut()[addr..addr + data.len()].copy_from_slice(data);
            }
            mock
        }

        /// Get the current memory contents
        fn get_memory(&self) -> Vec<u8> {
            self.memory.borrow().clone()
        }

        /// Get the list of erase operations
        fn get_erases(&self) -> Vec<(u32, u32)> {
            self.erases.borrow().clone()
        }

        /// Get the list of write operations
        fn get_writes(&self) -> Vec<(u32, Vec<u8>)> {
            self.writes.borrow().clone()
        }

        /// Get the list of read operations
        fn get_reads(&self) -> Vec<(u32, usize)> {
            self.reads.borrow().clone()
        }
    }

    impl SpiMaster for MockFlash {
        fn features(&self) -> SpiFeatures {
            SpiFeatures::empty()
        }

        fn max_read_len(&self) -> usize {
            4096
        }

        fn max_write_len(&self) -> usize {
            256
        }

        fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
            let opcode = cmd.opcode;

            match opcode {
                // Read operations (0x03 = READ)
                opcodes::READ => {
                    if let Some(addr) = cmd.address {
                        let addr = addr as usize;
                        let len = cmd.read_buf.len();
                        if len > 0 {
                            self.reads.borrow_mut().push((addr as u32, len));
                            let mem = self.memory.borrow();
                            if addr + len <= mem.len() {
                                cmd.read_buf.copy_from_slice(&mem[addr..addr + len]);
                            }
                        }
                    }
                    Ok(())
                }

                // Write enable (no-op for mock)
                opcodes::WREN => Ok(()),

                // Page program (0x02 = PP)
                opcodes::PP => {
                    if let Some(addr) = cmd.address {
                        let addr = addr as usize;
                        let write_data = cmd.write_data;
                        if !write_data.is_empty() {
                            self.writes
                                .borrow_mut()
                                .push((addr as u32, write_data.to_vec()));
                            let mut mem = self.memory.borrow_mut();
                            if addr + write_data.len() <= mem.len() {
                                // Simulate flash write behavior (only 1->0 transitions)
                                for (i, byte) in write_data.iter().enumerate() {
                                    mem[addr + i] &= byte;
                                }
                            }
                        }
                    }
                    Ok(())
                }

                // Sector erase (4KB)
                opcodes::SE_20 => {
                    if let Some(addr) = cmd.address {
                        let addr = addr as usize;
                        let size = 4096;
                        self.erases.borrow_mut().push((addr as u32, size as u32));
                        let mut mem = self.memory.borrow_mut();
                        if addr + size <= mem.len() {
                            for byte in &mut mem[addr..addr + size] {
                                *byte = 0xFF;
                            }
                        }
                    }
                    Ok(())
                }

                // Block erase (32KB)
                opcodes::BE_52 => {
                    if let Some(addr) = cmd.address {
                        let addr = addr as usize;
                        let size = 32768;
                        self.erases.borrow_mut().push((addr as u32, size as u32));
                        let mut mem = self.memory.borrow_mut();
                        if addr + size <= mem.len() {
                            for byte in &mut mem[addr..addr + size] {
                                *byte = 0xFF;
                            }
                        }
                    }
                    Ok(())
                }

                // Block erase (64KB)
                opcodes::BE_D8 => {
                    if let Some(addr) = cmd.address {
                        let addr = addr as usize;
                        let size = 65536;
                        self.erases.borrow_mut().push((addr as u32, size as u32));
                        let mut mem = self.memory.borrow_mut();
                        if addr + size <= mem.len() {
                            for byte in &mut mem[addr..addr + size] {
                                *byte = 0xFF;
                            }
                        }
                    }
                    Ok(())
                }

                // Read status register (return ready)
                opcodes::RDSR => {
                    if !cmd.read_buf.is_empty() {
                        cmd.read_buf[0] = 0x00; // Not busy
                    }
                    Ok(())
                }

                _ => Ok(()),
            }
        }

        fn delay_us(&mut self, _us: u32) {}
    }

    /// Create a test chip definition with standard erase blocks
    fn test_chip(size: u32) -> FlashChip {
        FlashChip {
            vendor: "Test".to_string(),
            name: "TestFlash".to_string(),
            jedec_manufacturer: 0xEF,
            jedec_device: 0x4018,
            total_size: size,
            page_size: 256,
            features: Features::empty(),
            voltage_min_mv: 2700,
            voltage_max_mv: 3600,
            write_granularity: WriteGranularity::Page,
            erase_blocks: vec![
                EraseBlock::new(opcodes::SE_20, 4096),  // 4KB sector
                EraseBlock::new(opcodes::BE_52, 32768), // 32KB block
                EraseBlock::new(opcodes::BE_D8, 65536), // 64KB block
            ],
            tested: Default::default(),
        }
    }

    // =========================================================================
    // Tests for plan_erase_for_region
    // =========================================================================

    #[test]
    fn test_plan_erase_aligned_4k() {
        // Region perfectly aligned to 4KB boundary
        let erase_blocks = vec![
            EraseBlock::new(opcodes::SE_20, 4096),
            EraseBlock::new(opcodes::BE_D8, 65536),
        ];

        let plan = plan_erase_for_region(&erase_blocks, 0x1000, 0x1FFF).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].erase_start, 0x1000);
        assert_eq!(plan[0].erase_end, 0x1FFF);
        assert_eq!(plan[0].erase_block.size, 4096);
        assert!(!plan[0].region_unaligned);
    }

    #[test]
    fn test_plan_erase_aligned_64k() {
        // Region perfectly aligned to 64KB boundary
        let erase_blocks = vec![
            EraseBlock::new(opcodes::SE_20, 4096),
            EraseBlock::new(opcodes::BE_D8, 65536),
        ];

        let plan = plan_erase_for_region(&erase_blocks, 0x10000, 0x1FFFF).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].erase_start, 0x10000);
        assert_eq!(plan[0].erase_end, 0x1FFFF);
        assert_eq!(plan[0].erase_block.size, 65536);
        assert!(!plan[0].region_unaligned);
    }

    #[test]
    fn test_plan_erase_unaligned_start() {
        // Region starts in the middle of a 4KB block
        let erase_blocks = vec![
            EraseBlock::new(opcodes::SE_20, 4096),
            EraseBlock::new(opcodes::BE_D8, 65536),
        ];

        // Region 0x1500 - 0x1FFF (starts at offset 0x500 into a 4KB block)
        let plan = plan_erase_for_region(&erase_blocks, 0x1500, 0x1FFF).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].erase_start, 0x1000); // Must erase from block start
        assert_eq!(plan[0].erase_end, 0x1FFF);
        assert!(plan[0].region_unaligned); // Start is unaligned
    }

    #[test]
    fn test_plan_erase_unaligned_end() {
        // Region ends in the middle of a 4KB block
        let erase_blocks = vec![
            EraseBlock::new(opcodes::SE_20, 4096),
            EraseBlock::new(opcodes::BE_D8, 65536),
        ];

        // Region 0x1000 - 0x1500 (ends before block end)
        let plan = plan_erase_for_region(&erase_blocks, 0x1000, 0x1500).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].erase_start, 0x1000);
        assert_eq!(plan[0].erase_end, 0x1FFF); // Must erase to block end
        assert!(plan[0].region_unaligned); // End is unaligned
    }

    #[test]
    fn test_plan_erase_unaligned_both() {
        // Region starts and ends in the middle of blocks
        let erase_blocks = vec![
            EraseBlock::new(opcodes::SE_20, 4096),
            EraseBlock::new(opcodes::BE_D8, 65536),
        ];

        // Region 0x1500 - 0x2500 (crosses block boundary, both ends unaligned)
        let plan = plan_erase_for_region(&erase_blocks, 0x1500, 0x2500).unwrap();

        assert_eq!(plan.len(), 2);
        // First block
        assert_eq!(plan[0].erase_start, 0x1000);
        assert_eq!(plan[0].erase_end, 0x1FFF);
        assert!(plan[0].region_unaligned);
        // Second block
        assert_eq!(plan[1].erase_start, 0x2000);
        assert_eq!(plan[1].erase_end, 0x2FFF);
        assert!(plan[1].region_unaligned);
    }

    #[test]
    fn test_plan_erase_multiple_blocks() {
        // Region spanning multiple 4KB blocks
        let erase_blocks = vec![
            EraseBlock::new(opcodes::SE_20, 4096),
            EraseBlock::new(opcodes::BE_D8, 65536),
        ];

        // Region 0x1000 - 0x3FFF (3 x 4KB blocks)
        let plan = plan_erase_for_region(&erase_blocks, 0x1000, 0x3FFF).unwrap();

        assert_eq!(plan.len(), 3);
        assert!(!plan[0].region_unaligned);
        assert!(!plan[1].region_unaligned);
        assert!(!plan[2].region_unaligned);
    }

    // =========================================================================
    // Tests for erase_block_with_preserve (integration with mock flash)
    // =========================================================================

    #[test]
    fn test_erase_preserves_data_before_region() {
        // Test that data before the region is preserved when erasing
        // an unaligned region

        // Create a 64KB flash with some data
        let mut mock = MockFlash::with_contents(
            65536,
            &[
                (0x1000, &[0xAA; 0x500]), // Data before region (0x1000-0x14FF)
                (0x1500, &[0xBB; 0xB00]), // Data in region (0x1500-0x1FFF)
            ],
        );

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);
        let region = Region::new("test", 0x1500, 0x1FFF);

        erase_region(&mut mock, &ctx, &region).unwrap();

        let memory = mock.get_memory();

        // Data before region should be preserved
        assert!(
            memory[0x1000..0x1500].iter().all(|&b| b == 0xAA),
            "Data before region should be preserved"
        );

        // Region should be erased
        assert!(
            memory[0x1500..0x2000].iter().all(|&b| b == 0xFF),
            "Region should be erased"
        );

        // Check that we read the data before the region
        let reads = mock.get_reads();
        assert!(
            reads.iter().any(|(addr, _)| *addr == 0x1000),
            "Should read data before region to preserve it"
        );

        // Check that we wrote back the preserved data (may be split into page-sized chunks)
        let writes = mock.get_writes();
        // Verify writes occurred in the range 0x1000-0x14FF (the data before region)
        let writes_in_preserve_range: usize = writes
            .iter()
            .filter(|(addr, _)| *addr >= 0x1000 && *addr < 0x1500)
            .map(|(_, data)| data.len())
            .sum();
        assert!(
            writes_in_preserve_range >= 0x500,
            "Should write back at least 0x500 bytes of preserved data (got {})",
            writes_in_preserve_range
        );
    }

    #[test]
    fn test_erase_preserves_data_after_region() {
        // Test that data after the region is preserved when erasing
        // an unaligned region

        let mut mock = MockFlash::with_contents(
            65536,
            &[
                (0x1000, &[0xBB; 0x500]), // Data in region (0x1000-0x14FF)
                (0x1500, &[0xCC; 0xB00]), // Data after region (0x1500-0x1FFF)
            ],
        );

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);
        let region = Region::new("test", 0x1000, 0x14FF);

        erase_region(&mut mock, &ctx, &region).unwrap();

        let memory = mock.get_memory();

        // Region should be erased
        assert!(
            memory[0x1000..0x1500].iter().all(|&b| b == 0xFF),
            "Region should be erased"
        );

        // Data after region should be preserved
        assert!(
            memory[0x1500..0x2000].iter().all(|&b| b == 0xCC),
            "Data after region should be preserved"
        );
    }

    #[test]
    fn test_erase_preserves_data_both_sides() {
        // Test that data on both sides is preserved

        let mut mock = MockFlash::with_contents(
            65536,
            &[
                (0x1000, &[0xAA; 0x200]), // Before (0x1000-0x11FF)
                (0x1200, &[0xBB; 0x600]), // In region (0x1200-0x17FF)
                (0x1800, &[0xCC; 0x800]), // After (0x1800-0x1FFF)
            ],
        );

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);
        let region = Region::new("test", 0x1200, 0x17FF);

        erase_region(&mut mock, &ctx, &region).unwrap();

        let memory = mock.get_memory();

        // Data before should be preserved
        assert!(
            memory[0x1000..0x1200].iter().all(|&b| b == 0xAA),
            "Data before region should be preserved"
        );

        // Region should be erased
        assert!(
            memory[0x1200..0x1800].iter().all(|&b| b == 0xFF),
            "Region should be erased"
        );

        // Data after should be preserved
        assert!(
            memory[0x1800..0x2000].iter().all(|&b| b == 0xCC),
            "Data after region should be preserved"
        );
    }

    #[test]
    fn test_erase_aligned_no_preserve() {
        // Test that aligned erases don't do unnecessary writes (no data to preserve)
        // Note: reads still happen for erase verification (verifying the block contains 0xFF)

        let mut mock = MockFlash::with_contents(65536, &[(0x1000, &[0xAA; 0x1000])]);

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);
        let region = Region::new("test", 0x1000, 0x1FFF); // Perfectly aligned 4KB

        erase_region(&mut mock, &ctx, &region).unwrap();

        let memory = mock.get_memory();

        // Region should be erased
        assert!(
            memory[0x1000..0x2000].iter().all(|&b| b == 0xFF),
            "Region should be erased"
        );

        // Should have exactly one erase
        let erases = mock.get_erases();
        assert_eq!(erases.len(), 1);
        assert_eq!(erases[0], (0x1000, 4096));

        // Should have reads for erase verification (reading back to check 0xFF)
        let reads = mock.get_reads();
        assert!(
            !reads.is_empty(),
            "Erase should verify by reading back the erased block"
        );

        // Should have no writes (no data to restore for aligned erase)
        let writes = mock.get_writes();
        assert!(writes.is_empty(), "Aligned erase should not require writes");
    }

    #[test]
    fn test_erase_crossing_multiple_blocks() {
        // Test erasing a region that crosses multiple erase blocks

        let mut mock = MockFlash::with_contents(
            65536,
            &[
                (0x0F00, &[0xAA; 0x100]),  // Before first block (0x0F00-0x0FFF)
                (0x1000, &[0xBB; 0x3000]), // In region (0x1000-0x3FFF)
                (0x4000, &[0xCC; 0x100]),  // After last block (0x4000-0x40FF)
            ],
        );

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);
        // Region spans from middle of first block to middle of last block
        let region = Region::new("test", 0x0F00, 0x40FF);

        erase_region(&mut mock, &ctx, &region).unwrap();

        let memory = mock.get_memory();

        // Check erases happened (should be 5 x 4KB blocks: 0x0000, 0x1000, 0x2000, 0x3000, 0x4000)
        let erases = mock.get_erases();
        assert_eq!(erases.len(), 5);

        // Region should be erased
        assert!(
            memory[0x0F00..0x4100].iter().all(|&b| b == 0xFF),
            "Region should be erased"
        );

        // Data before first block boundary should be preserved
        assert!(
            memory[0x0000..0x0F00].iter().all(|&b| b == 0xFF),
            "Before region in first block was originally 0xFF"
        );

        // Data after last block boundary should be preserved
        assert!(
            memory[0x4100..0x5000].iter().all(|&b| b == 0xFF),
            "After region in last block was originally 0xFF"
        );
    }

    // =========================================================================
    // Tests for layout-based operations
    // =========================================================================

    #[test]
    fn test_erase_by_layout_single_region() {
        let mut mock = MockFlash::with_contents(65536, &[(0x1000, &[0xAA; 0x1000])]);

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);

        let mut layout = Layout::with_source(LayoutSource::Manual);
        layout.chip_size = Some(65536);
        let mut region = Region::new("test", 0x1000, 0x1FFF);
        region.included = true;
        layout.add_region(region);

        erase_by_layout(&mut mock, &ctx, &layout).unwrap();

        let memory = mock.get_memory();
        assert!(
            memory[0x1000..0x2000].iter().all(|&b| b == 0xFF),
            "Region should be erased"
        );
    }

    #[test]
    fn test_erase_by_layout_multiple_regions() {
        let mut mock = MockFlash::with_contents(
            65536,
            &[
                (0x1000, &[0xAA; 0x1000]), // Region 1
                (0x3000, &[0xBB; 0x1000]), // Region 2
            ],
        );

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);

        let mut layout = Layout::with_source(LayoutSource::Manual);
        layout.chip_size = Some(65536);

        let mut region1 = Region::new("region1", 0x1000, 0x1FFF);
        region1.included = true;
        layout.add_region(region1);

        let mut region2 = Region::new("region2", 0x3000, 0x3FFF);
        region2.included = true;
        layout.add_region(region2);

        // Add a region that is NOT included
        let region3 = Region::new("region3", 0x5000, 0x5FFF);
        layout.add_region(region3);

        erase_by_layout(&mut mock, &ctx, &layout).unwrap();

        let memory = mock.get_memory();

        // Included regions should be erased
        assert!(
            memory[0x1000..0x2000].iter().all(|&b| b == 0xFF),
            "Region 1 should be erased"
        );
        assert!(
            memory[0x3000..0x4000].iter().all(|&b| b == 0xFF),
            "Region 2 should be erased"
        );

        // Non-included region should be untouched (was already 0xFF)
        assert!(
            memory[0x5000..0x6000].iter().all(|&b| b == 0xFF),
            "Region 3 should be unchanged"
        );
    }

    #[test]
    fn test_write_region_with_unaligned_erase() {
        // Test that write_region properly erases and writes to unaligned regions

        let mut mock = MockFlash::with_contents(
            65536,
            &[
                (0x1000, &[0xAA; 0x500]), // Before region
                (0x1500, &[0xBB; 0xB00]), // In region
            ],
        );

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);
        let region = Region::new("test", 0x1500, 0x1FFF);

        // Write new data to the region
        let new_data = vec![0xDD; 0xB00];
        write_region(&mut mock, &ctx, &region, &new_data).unwrap();

        let memory = mock.get_memory();

        // Data before region should be preserved
        assert!(
            memory[0x1000..0x1500].iter().all(|&b| b == 0xAA),
            "Data before region should be preserved"
        );

        // New data should be written
        assert!(
            memory[0x1500..0x2000].iter().all(|&b| b == 0xDD),
            "New data should be written"
        );
    }

    #[test]
    fn test_write_by_layout() {
        let mut mock = MockFlash::new(65536);

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);

        let mut layout = Layout::with_source(LayoutSource::Manual);
        layout.chip_size = Some(65536);

        let mut region = Region::new("test", 0x1000, 0x1FFF);
        region.included = true;
        layout.add_region(region);

        // Create a full chip image
        let mut image = vec![0x00; 65536];
        // Put specific data in the region
        for i in 0x1000..0x2000 {
            image[i] = 0xAB;
        }

        write_by_layout(&mut mock, &ctx, &layout, &image).unwrap();

        let memory = mock.get_memory();

        // Only the included region should have new data
        assert!(
            memory[0x1000..0x2000].iter().all(|&b| b == 0xAB),
            "Region should have new data"
        );

        // Other areas should still be erased (0xFF)
        assert!(
            memory[0x0000..0x1000].iter().all(|&b| b == 0xFF),
            "Before region should be unchanged"
        );
        assert!(
            memory[0x2000..0x3000].iter().all(|&b| b == 0xFF),
            "After region should be unchanged"
        );
    }

    #[test]
    fn test_read_by_layout() {
        let mut mock = MockFlash::with_contents(
            65536,
            &[
                (0x1000, &[0xAA; 0x1000]), // Region 1
                (0x3000, &[0xBB; 0x1000]), // Region 2
            ],
        );

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);

        let mut layout = Layout::with_source(LayoutSource::Manual);
        layout.chip_size = Some(65536);

        let mut region1 = Region::new("region1", 0x1000, 0x1FFF);
        region1.included = true;
        layout.add_region(region1);

        let mut region2 = Region::new("region2", 0x3000, 0x3FFF);
        region2.included = true;
        layout.add_region(region2);

        let mut buffer = vec![0x00; 65536];
        read_by_layout(&mut mock, &ctx, &layout, &mut buffer).unwrap();

        // Only included regions should be read
        assert!(
            buffer[0x1000..0x2000].iter().all(|&b| b == 0xAA),
            "Region 1 should be read"
        );
        assert!(
            buffer[0x3000..0x4000].iter().all(|&b| b == 0xBB),
            "Region 2 should be read"
        );

        // Non-included areas should be unchanged (still 0x00)
        assert!(
            buffer[0x0000..0x1000].iter().all(|&b| b == 0x00),
            "Before regions should be unchanged"
        );
        assert!(
            buffer[0x2000..0x3000].iter().all(|&b| b == 0x00),
            "Between regions should be unchanged"
        );
    }

    #[test]
    fn test_verify_by_layout_success() {
        let mut mock = MockFlash::with_contents(65536, &[(0x1000, &[0xAA; 0x1000])]);

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);

        let mut layout = Layout::with_source(LayoutSource::Manual);
        layout.chip_size = Some(65536);

        let mut region = Region::new("test", 0x1000, 0x1FFF);
        region.included = true;
        layout.add_region(region);

        // Expected data matches
        let mut expected = vec![0xFF; 65536];
        for i in 0x1000..0x2000 {
            expected[i] = 0xAA;
        }

        let result = verify_by_layout(&mut mock, &ctx, &layout, &expected);
        assert!(result.is_ok());
    }

    #[test]
    fn test_verify_by_layout_failure() {
        let mut mock = MockFlash::with_contents(65536, &[(0x1000, &[0xAA; 0x1000])]);

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);

        let mut layout = Layout::with_source(LayoutSource::Manual);
        layout.chip_size = Some(65536);

        let mut region = Region::new("test", 0x1000, 0x1FFF);
        region.included = true;
        layout.add_region(region);

        // Expected data does NOT match
        let mut expected = vec![0xFF; 65536];
        for i in 0x1000..0x2000 {
            expected[i] = 0xBB; // Different from 0xAA in flash
        }

        let result = verify_by_layout(&mut mock, &ctx, &layout, &expected);
        assert!(matches!(result, Err(Error::VerifyError)));
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

    #[test]
    fn test_smart_write_no_changes() {
        // Flash already contains desired data - no operations should occur
        let mut mock = MockFlash::with_contents(65536, &[(0x0, &[0xAA; 65536])]);

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);

        // Want the same data
        let want = vec![0xAA; 65536];

        let stats = smart_write(&mut mock, &ctx, &want, &mut NoProgress).unwrap();

        assert!(!stats.flash_modified);
        assert_eq!(stats.bytes_changed, 0);
        assert_eq!(stats.erases_performed, 0);
        assert_eq!(stats.bytes_written, 0);

        // Should have reads (to compare) but no erases or writes
        let erases = mock.get_erases();
        assert!(erases.is_empty(), "Should not erase when no changes");

        let writes = mock.get_writes();
        assert!(writes.is_empty(), "Should not write when no changes");
    }

    #[test]
    fn test_smart_write_single_byte_change_erased() {
        // Change one byte in an already erased region - no erase needed
        let mut mock = MockFlash::new(65536); // All 0xFF

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);

        // Change one byte at offset 0x1000
        let mut want = vec![0xFF; 65536];
        want[0x1000] = 0xAA;

        let stats = smart_write(&mut mock, &ctx, &want, &mut NoProgress).unwrap();

        assert!(stats.flash_modified);
        assert_eq!(stats.bytes_changed, 1);
        assert_eq!(
            stats.erases_performed, 0,
            "Should not need erase for already erased flash"
        );
        assert_eq!(stats.bytes_written, 1);

        // Verify the byte was written
        let memory = mock.get_memory();
        assert_eq!(memory[0x1000], 0xAA);
    }

    #[test]
    fn test_smart_write_single_byte_change_needs_erase() {
        // Change one byte in a non-erased region - erase needed
        let mut mock = MockFlash::with_contents(65536, &[(0x1000, &[0xBB; 0x1000])]);

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);

        // Change one byte at offset 0x1000
        let mut want = vec![0xFF; 65536];
        // Keep existing data but change one byte
        for i in 0x1000..0x2000 {
            want[i] = 0xBB;
        }
        want[0x1500] = 0xAA;

        let stats = smart_write(&mut mock, &ctx, &want, &mut NoProgress).unwrap();

        assert!(stats.flash_modified);
        // Only one erase block (4KB) should be erased
        assert!(
            stats.erases_performed <= 2,
            "Should only erase affected blocks, got {}",
            stats.erases_performed
        );

        // Verify the change was made
        let memory = mock.get_memory();
        assert_eq!(memory[0x1500], 0xAA);
    }

    #[test]
    fn test_smart_write_preserves_unchanged_regions() {
        // Write to one region, verify another region is unchanged
        let mut mock = MockFlash::with_contents(
            65536,
            &[
                (0x0000, &[0xAA; 0x1000]), // Region we don't want to change
                (0x2000, &[0xBB; 0x1000]), // Region we'll modify
            ],
        );

        let chip = test_chip(65536);
        let ctx = FlashContext::new(chip);

        // Create want buffer - keep first region, change second
        let mut want = vec![0xFF; 65536];
        for i in 0x0000..0x1000 {
            want[i] = 0xAA; // Keep this the same
        }
        for i in 0x2000..0x3000 {
            want[i] = 0xCC; // Change this
        }

        let _stats = smart_write(&mut mock, &ctx, &want, &mut NoProgress).unwrap();

        let memory = mock.get_memory();

        // First region should be unchanged
        assert!(
            memory[0x0000..0x1000].iter().all(|&b| b == 0xAA),
            "Unchanged region should be preserved"
        );

        // Second region should have new data
        assert!(
            memory[0x2000..0x3000].iter().all(|&b| b == 0xCC),
            "Modified region should have new data"
        );
    }

    #[test]
    fn test_plan_smart_erase_no_erase_needed() {
        // When writing to erased flash, no erase should be planned
        let have = vec![0xFF; 4096];
        let want = vec![0xAA; 4096];

        let erase_blocks = vec![
            EraseBlock::new(opcodes::SE_20, 4096),
            EraseBlock::new(opcodes::BE_D8, 65536),
        ];

        let plan =
            plan_smart_erase(&erase_blocks, &have, &want, 0, 4095, WriteGranularity::Byte).unwrap();

        // All blocks should be marked as not needing erase
        assert!(
            plan.iter().all(|b| !b.needs_erase),
            "Should not need erase when writing to erased flash"
        );
    }

    #[test]
    fn test_plan_smart_erase_erase_needed() {
        // When overwriting non-erased data, erase should be planned
        let have = vec![0xAA; 4096];
        let want = vec![0xBB; 4096];

        let erase_blocks = vec![
            EraseBlock::new(opcodes::SE_20, 4096),
            EraseBlock::new(opcodes::BE_D8, 65536),
        ];

        let plan =
            plan_smart_erase(&erase_blocks, &have, &want, 0, 4095, WriteGranularity::Byte).unwrap();

        // At least one block should need erasing
        assert!(
            plan.iter().any(|b| b.needs_erase),
            "Should need erase when overwriting non-erased data"
        );
    }
}

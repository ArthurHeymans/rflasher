//! Content diffing and write-range coalescing for smart writes.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use crate::chip::WriteGranularity;

// =============================================================================
// Smart erase/write support
// =============================================================================

/// The erased value for flash memory (all bits set)
#[cfg(feature = "alloc")]
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
    assert_eq!(
        have.len(),
        want.len(),
        "need_erase: have and want must be the same length"
    );

    match granularity {
        WriteGranularity::Bit => {
            // For bit-granularity, we can only clear bits (1->0).
            // We need erase if any bit needs to go from 0->1
            // (have & want) != want means some bit in want is 1 but in have is 0
            have.iter().zip(want.iter()).any(|(h, w)| (h & w) != *w)
        }
        WriteGranularity::Byte | WriteGranularity::Page => {
            // Byte- and page-granularity chips behave the same at the byte
            // level: if a byte differs, the source must be erased (0xFF) to
            // allow writing the new value. Page granularity only restricts
            // how many bytes may be programmed per command, not which
            // transitions require an erase.
            have.iter()
                .zip(want.iter())
                .any(|(h, w)| *h != *w && *h != ERASED_VALUE)
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
    assert_eq!(
        have.len(),
        want.len(),
        "get_next_write_range: have and want must be the same length"
    );

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

/// Coalesce write ranges to page boundaries for optimal write performance.
///
/// Hardware-accelerated programmers (like Dediprog) use fast bulk USB transfers
/// for page-aligned writes, but fall back to slow byte-at-a-time SPI command
/// sequencing (WREN + PP + RDSR polling per chunk) for sub-page or misaligned
/// writes. This function aligns write ranges to `page_size` boundaries and
/// merges overlapping/adjacent ranges so that all writes can use the fast path.
///
/// Writing page-aligned 0xFF bytes to already-erased flash is harmless (no
/// state change in the flash cells) but allows the programmer's firmware to
/// handle the entire write via its optimized bulk transfer path.
///
/// If `page_size` is 0 or 1, the input ranges are returned unchanged (no
/// alignment benefit).
///
/// # Arguments
/// * `ranges` - Write ranges from `get_all_write_ranges` (must be sorted by start)
/// * `page_size` - Page size in bytes (typically 256 for SPI NOR flash)
/// * `total_size` - Total flash/buffer size in bytes (for clamping)
#[cfg(feature = "alloc")]
pub fn coalesce_write_ranges(
    ranges: &[WriteRange],
    page_size: u32,
    total_size: u32,
) -> Vec<WriteRange> {
    if ranges.is_empty() || page_size <= 1 {
        return ranges.to_vec();
    }

    let mut result: Vec<WriteRange> = Vec::with_capacity(ranges.len());

    for range in ranges {
        // Align start down to page boundary
        let aligned_start = (range.start / page_size) * page_size;
        // Align end up to page boundary, clamped to total_size
        let end = range.start + range.len;
        let aligned_end = (end.div_ceil(page_size) * page_size).min(total_size);

        // Merge with previous range if overlapping or adjacent
        if let Some(last) = result.last_mut() {
            let last_end = last.start + last.len;
            if aligned_start <= last_end {
                // Ranges overlap or are adjacent after alignment; merge
                let merged_end = aligned_end.max(last_end);
                last.len = merged_end - last.start;
                continue;
            }
        }

        result.push(WriteRange {
            start: aligned_start,
            len: aligned_end - aligned_start,
        });
    }

    result
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

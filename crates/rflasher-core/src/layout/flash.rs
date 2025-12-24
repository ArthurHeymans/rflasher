//! Functions to read layouts from flash chips
//!
//! These functions read IFD (Intel Flash Descriptor) and FMAP (Flash Map)
//! structures directly from a flash chip, rather than from a file.

use std::vec;

use crate::flash::{self, FlashContext};
use crate::programmer::SpiMaster;

use super::{fmap, has_fmap, has_ifd, ifd, is_valid_fmap_header, Layout, LayoutError};

/// Size of the IFD region (first 4KB of flash)
const IFD_SIZE: usize = 0x1000;

/// FMAP signature for search
const FMAP_SIGNATURE: &[u8; 8] = b"__FMAP__";

/// FMAP header size
const FMAP_HEADER_SIZE: usize = 56;

/// FMAP area entry size
const FMAP_AREA_SIZE: usize = 42;

/// Read Intel Flash Descriptor from flash chip and parse into Layout
///
/// The IFD is located at the beginning of the flash (first 4KB).
/// This function reads that region and parses the descriptor.
///
/// # Arguments
/// * `master` - SPI master for flash communication
/// * `ctx` - Flash context with chip information
///
/// # Returns
/// A Layout parsed from the IFD, or an error if the IFD is not found or invalid.
///
/// # Example
/// ```ignore
/// let ctx = flash::probe(master, db)?;
/// let layout = read_ifd_from_flash(master, &ctx)?;
/// println!("Found {} regions", layout.len());
/// ```
pub fn read_ifd_from_flash<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
) -> std::result::Result<Layout, LayoutError> {
    // IFD is always at the start - read first 4KB
    let mut buf = vec![0u8; IFD_SIZE];

    flash::read(master, ctx, 0, &mut buf).map_err(|_| LayoutError::IoError)?;

    if !has_ifd(&buf) {
        return Err(LayoutError::InvalidIfdSignature);
    }

    ifd::parse_ifd(&buf)
}

/// Read FMAP from flash chip and parse into Layout
///
/// This function searches the flash for the FMAP signature using a
/// combination of binary search (for common power-of-2 locations) and
/// linear search as a fallback. This follows the same strategy as flashprog.
///
/// # Arguments
/// * `master` - SPI master for flash communication
/// * `ctx` - Flash context with chip information
///
/// # Returns
/// A Layout parsed from the FMAP, or an error if no FMAP is found.
///
/// # Example
/// ```ignore
/// let ctx = flash::probe(master, db)?;
/// let layout = read_fmap_from_flash(master, &ctx)?;
/// println!("Found {} regions", layout.len());
/// ```
pub fn read_fmap_from_flash<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
) -> std::result::Result<Layout, LayoutError> {
    let chip_size = ctx.total_size() as u32;

    // Try binary search first (common locations at power-of-2 offsets)
    if let Some(layout) = fmap_bsearch_rom(master, ctx, 0, chip_size, 256)? {
        return Ok(layout);
    }

    // Fallback to linear search - read the entire flash
    // This is expensive but ensures we find the FMAP if it exists
    fmap_lsearch_rom(master, ctx, 0, chip_size)
}

/// Binary search for FMAP at common power-of-2 offsets
///
/// This follows flashprog's approach of checking aligned locations first
/// since FMAPs are commonly placed at power-of-2 boundaries.
fn fmap_bsearch_rom<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    rom_offset: u32,
    len: u32,
    min_stride: u32,
) -> std::result::Result<Option<Layout>, LayoutError> {
    let chip_size = ctx.total_size() as u32;

    if rom_offset + len > chip_size {
        return Err(LayoutError::IoError);
    }

    if (len as usize) < FMAP_HEADER_SIZE {
        return Ok(None);
    }

    // Buffer for reading the FMAP signature (8 bytes)
    let mut sig_buf = [0u8; 8];

    // Start with the largest stride and decrease
    let mut stride = chip_size / 2;
    let mut check_offset_0 = true;

    while stride >= min_stride {
        if stride > len {
            stride /= 2;
            continue;
        }

        let mut offset = rom_offset;
        while offset <= rom_offset + len - FMAP_HEADER_SIZE as u32 {
            // Skip offsets we've already checked at larger strides
            if offset.is_multiple_of(stride * 2) && offset != 0 {
                offset += stride;
                continue;
            }

            if offset == 0 && !check_offset_0 {
                offset += stride;
                continue;
            }

            if offset == 0 {
                check_offset_0 = false;
            }

            // Read the signature
            if flash::read(master, ctx, offset, &mut sig_buf).is_err() {
                offset += stride;
                continue;
            }

            // Check for FMAP signature
            if &sig_buf != FMAP_SIGNATURE {
                offset += stride;
                continue;
            }

            // Found potential FMAP - read and validate the header
            let mut header_buf = vec![0u8; FMAP_HEADER_SIZE];
            if flash::read(master, ctx, offset, &mut header_buf).is_err() {
                offset += stride;
                continue;
            }

            // Validate the header
            if is_valid_fmap_header(&header_buf) {
                // Read the full FMAP including areas
                if let Ok(layout) = read_fmap_at_offset(master, ctx, offset) {
                    return Ok(Some(layout));
                }
            }

            offset += stride;
        }

        stride /= 2;
    }

    Ok(None)
}

/// Linear search for FMAP by reading the entire region
fn fmap_lsearch_rom<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    rom_offset: u32,
    len: u32,
) -> std::result::Result<Layout, LayoutError> {
    // Read the entire region into memory
    // This is expensive for large chips but necessary as a fallback
    let mut buf = vec![0u8; len as usize];

    flash::read(master, ctx, rom_offset, &mut buf).map_err(|_| LayoutError::IoError)?;

    if !has_fmap(&buf) {
        return Err(LayoutError::InvalidFmapSignature);
    }

    fmap::parse_fmap(&buf)
}

/// Read FMAP from a specific offset in flash
fn read_fmap_at_offset<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    offset: u32,
) -> std::result::Result<Layout, LayoutError> {
    // First read just the header to get the number of areas
    let mut header = vec![0u8; FMAP_HEADER_SIZE];
    flash::read(master, ctx, offset, &mut header).map_err(|_| LayoutError::IoError)?;

    if !is_valid_fmap_header(&header) {
        return Err(LayoutError::InvalidFmapSignature);
    }

    let nareas = u16::from_le_bytes([header[54], header[55]]) as usize;
    let total_size = FMAP_HEADER_SIZE + nareas * FMAP_AREA_SIZE;

    // Now read the full FMAP
    let mut fmap_data = vec![0u8; total_size];
    flash::read(master, ctx, offset, &mut fmap_data).map_err(|_| LayoutError::IoError)?;

    fmap::parse_fmap_at(&fmap_data, 0)
}

/// Auto-detect and read layout from flash (tries IFD first, then FMAP)
///
/// This function attempts to detect the layout format automatically by:
/// 1. Checking for Intel Flash Descriptor at the start of flash
/// 2. If not found, searching for FMAP signature
///
/// # Arguments
/// * `master` - SPI master for flash communication  
/// * `ctx` - Flash context with chip information
///
/// # Returns
/// A Layout from either IFD or FMAP, or an error if neither is found.
pub fn read_layout_from_flash<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
) -> std::result::Result<Layout, LayoutError> {
    // Try IFD first (it's at offset 0, so quick to check)
    if let Ok(layout) = read_ifd_from_flash(master, ctx) {
        return Ok(layout);
    }

    // Try FMAP
    read_fmap_from_flash(master, ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_fmap_header() {
        // Valid header
        let mut header = vec![0u8; FMAP_HEADER_SIZE];
        header[0..8].copy_from_slice(FMAP_SIGNATURE);
        header[8] = 1; // ver_major
        header[9] = 0; // ver_minor
        header[54..56].copy_from_slice(&5u16.to_le_bytes()); // 5 areas

        assert!(is_valid_fmap_header(&header));

        // Invalid signature
        let mut bad_sig = header.clone();
        bad_sig[0] = 0xFF;
        assert!(!is_valid_fmap_header(&bad_sig));

        // Invalid version
        let mut bad_ver = header.clone();
        bad_ver[8] = 5; // Major version too high
        assert!(!is_valid_fmap_header(&bad_ver));

        // Too many areas
        let mut too_many_areas = header.clone();
        too_many_areas[54..56].copy_from_slice(&2000u16.to_le_bytes());
        assert!(!is_valid_fmap_header(&too_many_areas));

        // Too short
        assert!(!is_valid_fmap_header(&[0u8; 10]));
    }
}

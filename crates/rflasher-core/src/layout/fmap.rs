//! FMAP (Flash Map) parsing
//!
//! FMAP is a format used primarily by Chromebook firmware to describe
//! flash regions. The FMAP structure can be embedded anywhere in the
//! flash image.
//!
//! Reference: flashprog/fmap.c and https://chromium.googlesource.com/chromiumos/platform/flashmap

use std::format;
use std::string::{String, ToString};
use std::vec;

use zerocopy::byteorder::little_endian::{U16 as U16LE, U32 as U32LE, U64 as U64LE};
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::{Layout, LayoutError, LayoutSource, Region};

/// FMAP signature: "__FMAP__"
const FMAP_SIGNATURE: &[u8; 8] = b"__FMAP__";

/// Maximum supported FMAP major version
const FMAP_VER_MAJOR: u8 = 1;

/// Size of FMAP header
const FMAP_HEADER_SIZE: usize = 56;

/// Size of FMAP area
#[allow(dead_code)]
const FMAP_AREA_SIZE: usize = 42;

/// Minimum stride for binary search
///
/// This is the smallest alignment boundary we check during binary search.
/// Smaller values = more thorough search but more flash reads.
const MIN_STRIDE: u32 = 256;

/// FMAP area flags
pub mod flags {
    /// Area is static (read-only)
    pub const STATIC: u16 = 1 << 0;
    /// Area is compressed
    #[allow(dead_code)]
    pub const COMPRESSED: u16 = 1 << 1;
    /// Area is read-only
    pub const RO: u16 = 1 << 2;
}

/// FMAP header structure (56 bytes)
///
/// All multi-byte fields are little-endian.
#[repr(C)]
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
struct FmapHeader {
    signature: [u8; 8],
    ver_major: u8,
    ver_minor: u8,
    base: U64LE,
    size: U32LE,
    name: [u8; 32],
    nareas: U16LE,
}

/// FMAP area structure (42 bytes)
///
/// All multi-byte fields are little-endian.
#[repr(C)]
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
struct FmapArea {
    offset: U32LE,
    size: U32LE,
    name: [u8; 32],
    flags: U16LE,
}

/// Search for FMAP signature in data
fn find_fmap(data: &[u8]) -> Option<usize> {
    if data.len() < FMAP_HEADER_SIZE {
        return None;
    }

    // Search for signature
    for offset in 0..=(data.len() - FMAP_HEADER_SIZE) {
        if &data[offset..offset + 8] == FMAP_SIGNATURE {
            // Found potential FMAP, validate it
            if validate_fmap(&data[offset..]).is_ok() {
                return Some(offset);
            }
        }
    }

    None
}

/// Validate an FMAP structure
///
/// Checks signature, version, and structure validity.
/// This is the canonical validation function used by all FMAP implementations.
pub fn validate_fmap(data: &[u8]) -> Result<(), LayoutError> {
    // Parse header using zerocopy - this also validates minimum size
    let (header, remaining) =
        FmapHeader::ref_from_prefix(data).map_err(|_| LayoutError::InvalidFmapSignature)?;

    // Check signature
    if &header.signature != FMAP_SIGNATURE {
        return Err(LayoutError::InvalidFmapSignature);
    }

    // Check version
    if header.ver_major > FMAP_VER_MAJOR {
        return Err(LayoutError::UnsupportedFmapVersion);
    }

    // Check that nareas is reasonable
    let nareas = header.nareas.get() as usize;
    if nareas > 256 {
        // Sanity check - more than 256 areas is unreasonable
        return Err(LayoutError::InvalidFmapSignature);
    }

    // Verify there's enough data for all areas using zerocopy
    <[FmapArea]>::ref_from_prefix_with_elems(remaining, nareas)
        .map_err(|_| LayoutError::InvalidFmapSignature)?;

    Ok(())
}

/// Check if a buffer contains a valid FMAP header (convenience wrapper)
pub fn is_valid_fmap_header(data: &[u8]) -> bool {
    validate_fmap(data).is_ok()
}

/// Trait for searchable storage (file buffer or flash chip)
///
/// This abstraction allows FMAP search algorithms to work on both
/// in-memory buffers and physical flash chips.
pub trait FmapSearchable {
    /// Get the total size of the searchable storage
    fn size(&self) -> u32;

    /// Read data from an offset
    fn read_at(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), LayoutError>;
}

/// Implement FmapSearchable for byte slices (file buffers)
impl FmapSearchable for &[u8] {
    fn size(&self) -> u32 {
        self.len() as u32
    }

    fn read_at(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), LayoutError> {
        let offset = offset as usize;
        let end = offset + buf.len();

        if end > self.len() {
            return Err(LayoutError::IoError);
        }

        buf.copy_from_slice(&self[offset..end]);
        Ok(())
    }
}

/// Search for FMAP using binary search followed by linear search
///
/// This follows flashprog's strategy:
/// 1. Binary search at power-of-2 aligned offsets (fast, checks common locations)
/// 2. Linear search as fallback (slow but comprehensive)
///
/// Works on both file buffers and flash chips through the FmapSearchable trait.
pub fn search_fmap<S: FmapSearchable>(storage: &mut S) -> Result<Layout, LayoutError> {
    let size = storage.size();

    // Try binary search first (check power-of-2 aligned offsets)
    if let Some(offset) = binary_search_fmap(storage, 0, size)? {
        // Read enough to parse the FMAP
        let mut header = vec![0u8; 4096]; // Generous size for header + areas
        storage.read_at(offset, &mut header)?;
        return parse_fmap_at(&header, 0);
    }

    // Fallback to linear search - read entire storage and search
    let mut buffer = vec![0u8; size as usize];
    storage.read_at(0, &mut buffer)?;

    if let Some(offset) = find_fmap(&buffer) {
        parse_fmap_at(&buffer, offset)
    } else {
        Err(LayoutError::InvalidFmapSignature)
    }
}

/// Binary search for FMAP at power-of-2 aligned offsets
///
/// Follows flashprog's algorithm: start with largest stride (size/2) and
/// halve on each iteration. Skip offsets already checked by larger strides.
fn binary_search_fmap<S: FmapSearchable>(
    storage: &mut S,
    rom_offset: u32,
    len: u32,
) -> Result<Option<u32>, LayoutError> {
    let size = storage.size();

    if rom_offset + len > size || (len as usize) < FMAP_HEADER_SIZE {
        return Ok(None);
    }

    // Buffer for reading signature and header
    let mut sig_buf = [0u8; 8];
    let mut header_buf = vec![0u8; FMAP_HEADER_SIZE];

    // Generate strides: size/2, size/4, size/8, ... down to MIN_STRIDE
    // Using successors to generate the halving sequence
    let strides = std::iter::successors(Some(size / 2), |&stride| {
        let next = stride / 2;
        (next >= MIN_STRIDE).then_some(next)
    });

    let mut offset_0_checked = false;

    for stride in strides.filter(|&s| s <= len) {
        // Generate candidate offsets at this stride level
        let offsets =
            (rom_offset..=rom_offset + len - FMAP_HEADER_SIZE as u32).step_by(stride as usize);

        for offset in offsets {
            // Skip offsets already checked by larger strides
            if offset.is_multiple_of(stride * 2) && offset != 0 {
                continue;
            }

            // Special handling for offset 0 - only check once
            if offset == 0 {
                if offset_0_checked {
                    continue;
                }
                offset_0_checked = true;
            }
            // Read signature first (8 bytes) - cheap check
            if storage.read_at(offset, &mut sig_buf).is_err() {
                continue;
            }

            // Check for FMAP signature
            if &sig_buf != FMAP_SIGNATURE {
                continue;
            }

            // Found potential FMAP - read and validate the header
            if storage.read_at(offset, &mut header_buf).is_err() {
                continue;
            }

            if is_valid_fmap_header(&header_buf) {
                return Ok(Some(offset));
            }
        }
    }

    Ok(None)
}

/// Parse FMAP from raw data
pub fn parse_fmap(data: &[u8]) -> Result<Layout, LayoutError> {
    let offset = find_fmap(data).ok_or(LayoutError::InvalidFmapSignature)?;
    parse_fmap_at(data, offset)
}

/// Parse FMAP from a specific offset
pub fn parse_fmap_at(data: &[u8], offset: usize) -> Result<Layout, LayoutError> {
    let fmap_data = &data[offset..];
    validate_fmap(fmap_data)?;

    // Parse header using zerocopy - ref_from_prefix returns (reference, remaining_bytes)
    let (header, remaining) =
        FmapHeader::ref_from_prefix(fmap_data).map_err(|_| LayoutError::InvalidFmapSignature)?;

    let ver_major = header.ver_major;
    let ver_minor = header.ver_minor;
    let nareas = header.nareas.get() as usize;

    // Parse name (null-terminated)
    let name = parse_fmap_string(&header.name);

    let mut layout = Layout::with_source(LayoutSource::Fmap);
    layout.name = Some(format!("FMAP: {} (v{}.{})", name, ver_major, ver_minor));

    // Parse all areas as a slice in one go
    let areas = <[FmapArea]>::ref_from_prefix_with_elems(remaining, nareas)
        .map_err(|_| LayoutError::InvalidFmapSignature)?
        .0;

    let mut layout =
        areas
            .iter()
            .filter(|area| area.size.get() != 0)
            .fold(layout, |mut layout, area| {
                let area_start = area.offset.get();
                let area_size = area.size.get();
                let area_flags = area.flags.get();
                let area_name = parse_fmap_string(&area.name);
                let end = area_start + area_size - 1;

                let region = Region {
                    name: area_name,
                    start: area_start,
                    end,
                    readonly: (area_flags & flags::STATIC) != 0 || (area_flags & flags::RO) != 0,
                    dangerous: false,
                    included: false,
                };

                layout.add_region(region);
                layout
            });

    layout.sort_by_address();
    Ok(layout)
}

/// Parse a null-terminated FMAP string
fn parse_fmap_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).to_string()
}

/// Check if data appears to contain an FMAP
pub fn has_fmap(data: &[u8]) -> bool {
    find_fmap(data).is_some()
}

/// Find the offset of FMAP in data
pub fn fmap_offset(data: &[u8]) -> Option<usize> {
    find_fmap(data)
}

impl Layout {
    /// Parse layout from FMAP in raw data
    pub fn from_fmap(data: &[u8]) -> Result<Self, LayoutError> {
        parse_fmap(data)
    }

    /// Parse layout from FMAP in a file
    pub fn from_fmap_file(path: impl AsRef<std::path::Path>) -> Result<Self, LayoutError> {
        let data = std::fs::read(path).map_err(|_| LayoutError::IoError)?;
        parse_fmap(&data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    fn make_test_fmap() -> Vec<u8> {
        let mut data = vec![0xFF; 0x1000];

        // Put FMAP at offset 0x100
        let offset = 0x100;

        // Signature
        data[offset..offset + 8].copy_from_slice(FMAP_SIGNATURE);

        // Version 1.0
        data[offset + 8] = 1; // ver_major
        data[offset + 9] = 0; // ver_minor

        // Base address (8 bytes, little endian)
        data[offset + 10..offset + 18].copy_from_slice(&0u64.to_le_bytes());

        // Size (4 bytes, little endian)
        data[offset + 18..offset + 22].copy_from_slice(&0x1000u32.to_le_bytes());

        // Name: "TEST_FMAP"
        let name = b"TEST_FMAP\0";
        data[offset + 22..offset + 22 + name.len()].copy_from_slice(name);

        // Number of areas: 2
        data[offset + 54..offset + 56].copy_from_slice(&2u16.to_le_bytes());

        // Area 0: RO_SECTION at 0x000-0x1FF
        let area0_offset = offset + FMAP_HEADER_SIZE;
        data[area0_offset..area0_offset + 4].copy_from_slice(&0u32.to_le_bytes()); // offset
        data[area0_offset + 4..area0_offset + 8].copy_from_slice(&0x200u32.to_le_bytes()); // size
        let area0_name = b"RO_SECTION\0";
        data[area0_offset + 8..area0_offset + 8 + area0_name.len()].copy_from_slice(area0_name);
        data[area0_offset + 40..area0_offset + 42].copy_from_slice(&flags::STATIC.to_le_bytes()); // flags

        // Area 1: RW_SECTION at 0x200-0xFFF
        let area1_offset = area0_offset + FMAP_AREA_SIZE;
        data[area1_offset..area1_offset + 4].copy_from_slice(&0x200u32.to_le_bytes()); // offset
        data[area1_offset + 4..area1_offset + 8].copy_from_slice(&0xE00u32.to_le_bytes()); // size
        let area1_name = b"RW_SECTION\0";
        data[area1_offset + 8..area1_offset + 8 + area1_name.len()].copy_from_slice(area1_name);
        data[area1_offset + 40..area1_offset + 42].copy_from_slice(&0u16.to_le_bytes()); // flags

        data
    }

    #[test]
    fn test_has_fmap() {
        let data = make_test_fmap();
        assert!(has_fmap(&data));
        assert!(!has_fmap(&[0xFF; 0x1000]));
    }

    #[test]
    fn test_fmap_offset() {
        let data = make_test_fmap();
        assert_eq!(fmap_offset(&data), Some(0x100));
    }

    #[test]
    fn test_parse_fmap() {
        let data = make_test_fmap();
        let layout = parse_fmap(&data).unwrap();

        assert!(layout.name.as_ref().unwrap().contains("TEST_FMAP"));
        assert_eq!(layout.regions.len(), 2);

        assert_eq!(layout.regions[0].name, "RO_SECTION");
        assert_eq!(layout.regions[0].start, 0x000);
        assert_eq!(layout.regions[0].end, 0x1FF);
        assert!(layout.regions[0].readonly);

        assert_eq!(layout.regions[1].name, "RW_SECTION");
        assert_eq!(layout.regions[1].start, 0x200);
        assert_eq!(layout.regions[1].end, 0xFFF);
        assert!(!layout.regions[1].readonly);
    }
}

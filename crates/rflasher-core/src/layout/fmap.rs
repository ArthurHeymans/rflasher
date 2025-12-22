//! FMAP (Flash Map) parsing
//!
//! FMAP is a format used primarily by Chromebook firmware to describe
//! flash regions. The FMAP structure can be embedded anywhere in the
//! flash image.
//!
//! Reference: flashprog/fmap.c and https://chromium.googlesource.com/chromiumos/platform/flashmap

use std::format;
use std::string::{String, ToString};

use super::{Layout, LayoutError, LayoutSource, Region};

/// FMAP signature: "__FMAP__"
const FMAP_SIGNATURE: &[u8; 8] = b"__FMAP__";

/// Maximum supported FMAP major version
const FMAP_VER_MAJOR: u8 = 1;



/// Size of FMAP header
const FMAP_HEADER_SIZE: usize = 56;

/// Size of FMAP area
const FMAP_AREA_SIZE: usize = 42;

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
fn validate_fmap(data: &[u8]) -> Result<(), LayoutError> {
    if data.len() < FMAP_HEADER_SIZE {
        return Err(LayoutError::InvalidFmapSignature);
    }

    // Check signature
    if &data[0..8] != FMAP_SIGNATURE {
        return Err(LayoutError::InvalidFmapSignature);
    }

    // Check version
    let ver_major = data[8];
    if ver_major > FMAP_VER_MAJOR {
        return Err(LayoutError::UnsupportedFmapVersion);
    }

    // Check that nareas is reasonable
    let nareas = u16::from_le_bytes([data[54], data[55]]) as usize;
    let required_size = FMAP_HEADER_SIZE + nareas * FMAP_AREA_SIZE;
    if data.len() < required_size {
        return Err(LayoutError::InvalidFmapSignature);
    }

    Ok(())
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

    // Parse header
    let ver_major = fmap_data[8];
    let ver_minor = fmap_data[9];
    let _base = u64::from_le_bytes(fmap_data[10..18].try_into().unwrap());
    let _size = u32::from_le_bytes(fmap_data[18..22].try_into().unwrap());
    let name_bytes = &fmap_data[22..54];
    let nareas = u16::from_le_bytes([fmap_data[54], fmap_data[55]]) as usize;

    // Parse name (null-terminated)
    let name = parse_fmap_string(name_bytes);

    let mut layout = Layout::with_source(LayoutSource::Fmap);
    layout.name = Some(format!("FMAP: {} (v{}.{})", name, ver_major, ver_minor));

    // Parse areas
    for i in 0..nareas {
        let area_offset = FMAP_HEADER_SIZE + i * FMAP_AREA_SIZE;
        let area_data = &fmap_data[area_offset..area_offset + FMAP_AREA_SIZE];

        let area_start = u32::from_le_bytes(area_data[0..4].try_into().unwrap());
        let area_size = u32::from_le_bytes(area_data[4..8].try_into().unwrap());
        let area_name_bytes = &area_data[8..40];
        let area_flags = u16::from_le_bytes([area_data[40], area_data[41]]);

        // Skip zero-size areas
        if area_size == 0 {
            continue;
        }

        let area_name = parse_fmap_string(area_name_bytes);
        let end = area_start + area_size - 1;

        let mut region = Region::new(area_name, area_start, end);

        // Set readonly flag based on FMAP flags
        region.readonly = (area_flags & flags::STATIC) != 0 || (area_flags & flags::RO) != 0;

        layout.add_region(region);
    }

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

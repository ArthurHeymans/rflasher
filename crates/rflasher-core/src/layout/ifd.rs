//! Intel Flash Descriptor (IFD) parsing
//!
//! The Intel Flash Descriptor is located at the beginning of flash chips
//! on Intel platforms. It contains information about flash regions.
//!
//! Reference: flashprog/ich_descriptors.c

use std::string::ToString;

use super::{Layout, LayoutError, LayoutSource, Region};

/// IFD signature at offset 0x10
const IFD_SIGNATURE: u32 = 0x0FF0_A55A;

/// Maximum number of IFD regions
const MAX_IFD_REGIONS: usize = 16;

/// IFD region names (based on Intel documentation)
const IFD_REGION_NAMES: [&str; MAX_IFD_REGIONS] = [
    "descriptor", // 0: Flash Descriptor
    "bios",       // 1: BIOS
    "me",         // 2: Intel ME
    "gbe",        // 3: Gigabit Ethernet
    "platform",   // 4: Platform Data
    "devexp",     // 5: Device Expansion
    "bios2",      // 6: Secondary BIOS
    "ec",         // 7: Embedded Controller
    "ie",         // 8: Innovation Engine
    "10gbe",      // 9: 10 Gigabit Ethernet
    "oprom",      // 10: Option ROM
    "region11",   // 11: Reserved
    "region12",   // 12: Reserved
    "region13",   // 13: Reserved
    "region14",   // 14: Reserved
    "ptt",        // 15: Platform Trust Technology
];

/// Dangerous regions that can brick the system
const DANGEROUS_REGIONS: [&str; 3] = ["me", "descriptor", "ptt"];

/// Read-only regions (descriptor should never be written)
const READONLY_REGIONS: [&str; 1] = ["descriptor"];

/// Extract base address from a Flash Region register (FLREG)
///
/// The base address is stored in bits 14:0, representing address bits 26:12.
/// This matches flashprog's ICH_FREG_BASE macro.
#[inline]
fn freg_base(flreg: u32) -> u32 {
    (flreg << 12) & 0x07FFF000
}

/// Extract limit address from a Flash Region register (FLREG)
///
/// The limit address is stored in bits 30:16, representing address bits 26:12.
/// The result is ORed with 0xFFF to get the inclusive end address.
/// This matches flashprog's ICH_FREG_LIMIT macro.
#[inline]
fn freg_limit(flreg: u32) -> u32 {
    ((flreg >> 4) & 0x07FFF000) | 0x00000FFF
}

/// Parse Intel Flash Descriptor from raw data
///
/// The IFD is located at the beginning of the flash chip (first 4KB typically).
pub fn parse_ifd(data: &[u8]) -> Result<Layout, LayoutError> {
    if data.len() < 0x1000 {
        return Err(LayoutError::InvalidIfdSignature);
    }

    // Check signature at offset 0x10
    let sig = u32::from_le_bytes([data[0x10], data[0x11], data[0x12], data[0x13]]);
    if sig != IFD_SIGNATURE {
        return Err(LayoutError::InvalidIfdSignature);
    }

    // Read FLMAP0 at offset 0x14
    let flmap0 = u32::from_le_bytes([data[0x14], data[0x15], data[0x16], data[0x17]]);

    // Calculate Flash Region Base Address (FRBA)
    // FRBA is at bits 23:16 of FLMAP0, shifted left by 4
    let frba = ((flmap0 >> 12) & 0xFF0) as usize;

    // Always scan all possible regions (up to 16).
    // The NR field in FLMAP0 is not reliable for newer chipsets (Skylake+),
    // where the number of regions is fixed per chipset generation.
    // Unused regions have limit < base, which we detect below.
    let num_regions = MAX_IFD_REGIONS;

    if frba + num_regions * 4 > data.len() {
        return Err(LayoutError::InvalidIfdSignature);
    }

    let mut layout = Layout::with_source(LayoutSource::Ifd);
    layout.name = Some("Intel Flash Descriptor".to_string());

    // Parse each region
    for (i, &name) in IFD_REGION_NAMES.iter().enumerate().take(num_regions) {
        let offset = frba + i * 4;
        let freg = u32::from_le_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]);

        // 0xFFFFFFFF indicates we've hit uninitialized flash memory beyond the
        // actual region table. Stop scanning here.
        if freg == 0xFFFFFFFF {
            break;
        }

        // Extract base and limit addresses using the same encoding as flashprog
        let base = freg_base(freg);
        let limit = freg_limit(freg);

        // Region is unused if limit < base
        if limit < base {
            continue;
        }

        let mut region = Region::new(name, base, limit);
        region.readonly = READONLY_REGIONS.contains(&name);
        region.dangerous = DANGEROUS_REGIONS.contains(&name);

        layout.add_region(region);
    }

    layout.sort_by_address();
    Ok(layout)
}

/// Check if data appears to contain an Intel Flash Descriptor
pub fn has_ifd(data: &[u8]) -> bool {
    if data.len() < 0x14 {
        return false;
    }
    let sig = u32::from_le_bytes([data[0x10], data[0x11], data[0x12], data[0x13]]);
    sig == IFD_SIGNATURE
}

impl Layout {
    /// Parse layout from Intel Flash Descriptor in raw data
    pub fn from_ifd(data: &[u8]) -> Result<Self, LayoutError> {
        parse_ifd(data)
    }

    /// Parse layout from Intel Flash Descriptor in a file
    pub fn from_ifd_file(path: impl AsRef<std::path::Path>) -> Result<Self, LayoutError> {
        let data = std::fs::read(path).map_err(|_| LayoutError::IoError)?;
        parse_ifd(&data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// Value for unused FLREG entries (limit < base means unused)
    /// This sets base=0x7FFF (max), limit=0 which makes limit < base.
    const FLREG_UNUSED: u32 = 0x00007FFF;

    fn make_test_ifd() -> Vec<u8> {
        let mut data = vec![0x00; 0x1000];

        // Signature at 0x10
        data[0x10..0x14].copy_from_slice(&IFD_SIGNATURE.to_le_bytes());

        // FLMAP0: NR=2 (3 regions), FRBA=0x40 (0x40 >> 4 = 0x04 in field)
        // bits 26:24 = NR, bits 23:16 = FRBA >> 4
        let flmap0: u32 = (2 << 24) | (0x04 << 16);
        data[0x14..0x18].copy_from_slice(&flmap0.to_le_bytes());

        // FRBA at 0x40 - initialize all 16 regions as unused first
        for i in 0..MAX_IFD_REGIONS {
            let offset = 0x40 + i * 4;
            data[offset..offset + 4].copy_from_slice(&FLREG_UNUSED.to_le_bytes());
        }

        // Region 0 (descriptor): 0x000000 - 0x000FFF
        let freg0: u32 = 0x0000_0000; // limit=0, base=0
        data[0x40..0x44].copy_from_slice(&freg0.to_le_bytes());

        // Region 1 (bios): 0x001000 - 0x7FFFFF
        let freg1: u32 = (0x07FF << 16) | 0x0001; // limit=0x7FF, base=0x001
        data[0x44..0x48].copy_from_slice(&freg1.to_le_bytes());

        // Region 2 (me): 0x800000 - 0xFFFFFF
        let freg2: u32 = (0x0FFF << 16) | 0x0800; // limit=0xFFF, base=0x800
        data[0x48..0x4C].copy_from_slice(&freg2.to_le_bytes());

        data
    }

    #[test]
    fn test_has_ifd() {
        let data = make_test_ifd();
        assert!(has_ifd(&data));
        assert!(!has_ifd(&[0xFF; 0x1000]));
    }

    #[test]
    fn test_parse_ifd() {
        let data = make_test_ifd();
        let layout = parse_ifd(&data).unwrap();

        assert_eq!(layout.regions.len(), 3);

        assert_eq!(layout.regions[0].name, "descriptor");
        assert_eq!(layout.regions[0].start, 0x000000);
        assert_eq!(layout.regions[0].end, 0x000FFF);
        assert!(layout.regions[0].readonly);

        assert_eq!(layout.regions[1].name, "bios");
        assert_eq!(layout.regions[1].start, 0x001000);
        assert_eq!(layout.regions[1].end, 0x7FFFFF);

        assert_eq!(layout.regions[2].name, "me");
        assert_eq!(layout.regions[2].start, 0x800000);
        assert_eq!(layout.regions[2].end, 0xFFFFFF);
        assert!(layout.regions[2].dangerous);
    }
}

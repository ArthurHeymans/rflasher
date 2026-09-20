//! Intel Flash Descriptor (IFD) parsing
//!
//! The Intel Flash Descriptor is located at the beginning of flash chips
//! on Intel platforms. It contains information about flash regions.
//!
//! Reference: flashprog/ich_descriptors.c

use std::string::ToString;

use zerocopy::byteorder::little_endian::U32 as U32LE;
use zerocopy::{FromBytes, Immutable, KnownLayout, Unaligned};

use super::{Layout, LayoutError, LayoutSource, Region};

/// Intel Flash Descriptor signature.
///
/// Normal descriptors store this at offset 0x00. Some PCH generations have
/// the descriptor shifted by one DWORD, in which case it is at offset 0x10;
/// flashprog accepts both forms.
const IFD_SIGNATURE: u32 = 0x0FF0_A55A;

/// Maximum number of IFD regions
const MAX_IFD_REGIONS: usize = 16;

/// Offset of the flash descriptor upper map.
const UPPER_MAP_OFFSET: usize = 0xEFC;

/// Fixed portion of an Intel Flash Descriptor.
#[repr(C)]
#[derive(FromBytes, KnownLayout, Immutable, Unaligned)]
struct IfdHeader {
    signature: U32LE,
    flmap0: U32LE,
    flmap1: U32LE,
    flmap2: U32LE,
}

/// IFD families that differ in region count or region naming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IfdGeneration {
    Legacy,
    LynxPoint,
    SunrisePoint,
    SixRegions,
    IfwiSixteenRegions,
    Server,
    LunarOrPantherLake,
    SixteenRegions,
}

/// Return a valid IFD header at an expected descriptor offset.
fn ifd_header_at(data: &[u8], offset: usize) -> Option<&IfdHeader> {
    let (header, _) = IfdHeader::ref_from_prefix(data.get(offset..)?).ok()?;
    (header.signature.get() == IFD_SIGNATURE).then_some(header)
}

/// Region names used by standard PCH100 and newer descriptors.
const IFD_REGION_NAMES: [&str; MAX_IFD_REGIONS] = [
    "descriptor", // 0: Flash Descriptor
    "bios",       // 1: BIOS
    "me",         // 2: Intel ME
    "gbe",        // 3: Gigabit Ethernet
    "platform",   // 4: Platform Data
    "region5",    // 5: Generation-specific
    "bios2",      // 6: Secondary BIOS
    "region7",    // 7: Generation-specific
    "ec",         // 8: Embedded Controller
    "region9",    // 9: Generation-specific
    "sse",        // 10: Sensor Subsystem
    "nis",        // 11: Network Interface Subsystem
    "region12",   // 12: Generation-specific
    "irc",        // 13: Integrated Runtime Configuration
    "region14",   // 14: Generation-specific
    "ptt",        // 15: Platform Trust Technology
];

/// Apollo Lake, Gemini Lake, and Elkhart Lake use an IFWI/TXE-oriented map.
const SIX_REGION_NAMES: [&str; MAX_IFD_REGIONS] = [
    "descriptor",
    "ifwi",
    "txe",
    "region3",
    "platform",
    "devexp",
    "region6",
    "region7",
    "region8",
    "region9",
    "region10",
    "region11",
    "region12",
    "region13",
    "region14",
    "region15",
];

/// Lewisburg and Emmitsburg server PCHs use IE and 10GbE regions.
const SERVER_REGION_NAMES: [&str; MAX_IFD_REGIONS] = [
    "descriptor",
    "bios",
    "me",
    "gbe",
    "platform",
    "devexp",
    "bios2",
    "region7",
    "bmc",
    "devexp2",
    "ie",
    "10gbe",
    "oprom",
    "region13",
    "region14",
    "region15",
];

/// Lunar Lake and Panther Lake assign PSE to FLREG10 and leave FLREG11 generic.
const LUNAR_PANTHER_REGION_NAMES: [&str; MAX_IFD_REGIONS] = [
    "descriptor", // 0: Flash Descriptor
    "bios",       // 1: BIOS
    "me",         // 2: Intel CSME
    "gbe",        // 3: Gigabit Ethernet
    "platform",   // 4: Platform Data
    "region5",    // 5: Generation-specific
    "region6",    // 6: Generation-specific
    "region7",    // 7: Generation-specific
    "ec",         // 8: Embedded Controller
    "region9",    // 9: Generation-specific
    "pse",        // 10: Programmable Services Engine
    "region11",   // 11: Generation-specific
    "region12",   // 12: Generation-specific
    "region13",   // 13: Generation-specific
    "region14",   // 14: Generation-specific
    "region15",   // 15: Generation-specific
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

/// Read one little-endian descriptor DWORD at an absolute byte offset.
fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes: [u8; 4] = data.get(offset..offset + 4)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

/// Infer the descriptor generation using the same fields as flashprog's
/// `guess_ich_chipset()`. The NR field became reserved with Sunrise Point, so
/// it cannot be used to determine the region count on Skylake and newer PCHs.
fn guess_ifd_generation(flmap1: u32, flmap2: u32, flumap1: u32) -> IfdGeneration {
    let nm = (flmap1 >> 8) & 0x7;
    let isl = flmap1 >> 24;
    let fmsba = flmap2 & 0xff;
    let msl = (flmap2 >> 8) & 0xff;
    let iccriba = (flmap2 >> 16) & 0xff;
    let mdtba = flumap1 >> 24;

    if iccriba == 0 {
        if isl <= 16 {
            IfdGeneration::Legacy
        } else if flmap2 == 0 {
            IfdGeneration::SixRegions
        } else if isl < 0x50 {
            IfdGeneration::Legacy
        } else if nm == 6 {
            IfdGeneration::Server
        } else {
            // Remaining descriptors in this branch are treated as Arrow Lake
            // compatible by flashprog.
            IfdGeneration::SixteenRegions
        }
    } else if mdtba == 0 {
        if iccriba < 0x31 && fmsba < 0x30 {
            if (msl == 0 && isl <= 17) || (msl <= 1 && isl <= 18) {
                IfdGeneration::Legacy
            } else {
                IfdGeneration::LynxPoint
            }
        } else if nm == 6 {
            IfdGeneration::Server
        } else {
            IfdGeneration::SunrisePoint
        }
    } else if flmap2 == u32::MAX {
        IfdGeneration::SixteenRegions
    } else {
        let cssl = (flmap2 >> 16) & 0xff;
        let csso = (flmap2 >> 2) & 0x3ff;
        if cssl == 0x03 && csso == 0x58 {
            IfdGeneration::IfwiSixteenRegions
        } else if cssl == 0x03 && (csso == 0x60 || (csso == 0x70 && matches!(isl, 0x7d | 0x7e))) {
            IfdGeneration::LunarOrPantherLake
        } else {
            // Cannon Point and all other later generations recognized by
            // flashprog use sixteen FLREGs.
            IfdGeneration::SixteenRegions
        }
    }
}

/// Determine how many FLREG entries are valid for this descriptor generation.
fn ifd_region_count(generation: IfdGeneration, header: &IfdHeader) -> Option<usize> {
    let nr = ((header.flmap0.get() >> 24) & 0x7) as usize;

    match generation {
        IfdGeneration::Legacy => (nr <= 4).then_some(nr + 1),
        IfdGeneration::LynxPoint => (nr <= 6).then_some(nr + 1),
        IfdGeneration::SunrisePoint => Some(10),
        IfdGeneration::SixRegions => Some(6),
        IfdGeneration::IfwiSixteenRegions
        | IfdGeneration::Server
        | IfdGeneration::LunarOrPantherLake
        | IfdGeneration::SixteenRegions => Some(16),
    }
}

/// Select region names for descriptor families with generation-specific slots.
fn ifd_region_names(generation: IfdGeneration) -> &'static [&'static str; MAX_IFD_REGIONS] {
    match generation {
        IfdGeneration::SixRegions | IfdGeneration::IfwiSixteenRegions => &SIX_REGION_NAMES,
        IfdGeneration::Server => &SERVER_REGION_NAMES,
        IfdGeneration::LunarOrPantherLake => &LUNAR_PANTHER_REGION_NAMES,
        _ => &IFD_REGION_NAMES,
    }
}

/// Parse Intel Flash Descriptor from raw data
///
/// The IFD is located at the beginning of the flash chip (first 4KB typically).
pub fn parse_ifd(data: &[u8]) -> Result<Layout, LayoutError> {
    if data.len() < 0x1000 {
        return Err(LayoutError::InvalidIfdSignature);
    }

    // flashprog accepts the normal descriptor location (offset 0) and the
    // PCH-bug variant where the descriptor is shifted by one DWORD (offset
    // 0x10). FLMAP0 is immediately after the signature in either form.
    let header = ifd_header_at(data, 0)
        .or_else(|| ifd_header_at(data, 0x10))
        .ok_or(LayoutError::InvalidIfdSignature)?;
    let flmap0 = header.flmap0.get();

    // Calculate Flash Region Base Address (FRBA), matching flashprog's
    // getFRBA() macro. FRBA is an absolute descriptor offset, even for the
    // PCH-bug variant.
    let frba = ((flmap0 >> 12) & 0xFF0) as usize;
    let flumap1 = read_u32(data, UPPER_MAP_OFFSET).ok_or(LayoutError::InvalidIfdSignature)?;
    let generation = guess_ifd_generation(header.flmap1.get(), header.flmap2.get(), flumap1);
    let num_regions =
        ifd_region_count(generation, header).ok_or(LayoutError::InvalidIfdSignature)?;
    let region_names = ifd_region_names(generation);

    let (region_registers, _) = <[U32LE]>::ref_from_prefix_with_elems(
        data.get(frba..).ok_or(LayoutError::InvalidIfdSignature)?,
        num_regions,
    )
    .map_err(|_| LayoutError::InvalidIfdSignature)?;

    let mut layout = Layout::with_source(LayoutSource::Ifd);
    layout.name = Some("Intel Flash Descriptor".to_string());

    // Parse each region
    for (&name, freg) in region_names.iter().zip(region_registers) {
        let freg = freg.get();

        // Extract base and limit addresses using the same encoding as flashprog
        let base = freg_base(freg);
        let limit = freg_limit(freg);

        // Region is unused if limit <= base
        if limit <= base {
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
    ifd_header_at(data, 0).is_some() || ifd_header_at(data, 0x10).is_some()
}

impl Layout {
    /// Parse layout from Intel Flash Descriptor in raw data
    pub fn from_ifd(data: &[u8]) -> Result<Self, LayoutError> {
        parse_ifd(data)
    }

    /// Parse layout from Intel Flash Descriptor in a file
    pub fn from_ifd_file(path: impl AsRef<std::path::Path>) -> Result<Self, LayoutError> {
        let data = std::fs::read(path).map_err(|e| LayoutError::IoError(e.to_string()))?;
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

    /// Build a small legacy descriptor at either accepted signature offset.
    fn make_test_ifd_at(offset: usize) -> Vec<u8> {
        let mut data = vec![0x00; 0x1000];

        // Signature at the requested descriptor offset
        data[offset..offset + 4].copy_from_slice(&IFD_SIGNATURE.to_le_bytes());

        // FLMAP0: NR=2 (3 regions), FRBA=0x40 (0x40 >> 4 = 0x04 in field)
        // bits 26:24 = NR, bits 23:16 = FRBA >> 4
        let flmap0: u32 = (2 << 24) | (0x04 << 16);
        data[offset + 4..offset + 8].copy_from_slice(&flmap0.to_le_bytes());
        data[UPPER_MAP_OFFSET..UPPER_MAP_OFFSET + 4].copy_from_slice(&0u32.to_le_bytes());

        // FRBA at absolute offset 0x40 - initialize all reported regions as unused first
        for i in 0..3 {
            let region_offset = 0x40 + i * 4;
            data[region_offset..region_offset + 4].copy_from_slice(&FLREG_UNUSED.to_le_bytes());
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

    /// Build the shifted-signature legacy descriptor used by basic tests.
    fn make_test_ifd() -> Vec<u8> {
        make_test_ifd_at(0x10)
    }

    /// Build a Sunrise Point descriptor whose reserved NR field is zero.
    fn make_sunrise_point_ifd() -> Vec<u8> {
        let mut data = vec![0x00; 0x1000];
        data[0..4].copy_from_slice(&IFD_SIGNATURE.to_le_bytes());

        // NR is reserved on Sunrise Point and later. A value of zero must not
        // restrict parsing to FLREG0. FRBA points to 0x40.
        let flmap0: u32 = 0x04 << 16;
        data[4..8].copy_from_slice(&flmap0.to_le_bytes());

        // ICCRIBA=0x31 and MDTBA=0 identify a Sunrise Point-compatible
        // descriptor, which has ten FLREG entries.
        let flmap2: u32 = 0x31 << 16;
        data[12..16].copy_from_slice(&flmap2.to_le_bytes());
        data[UPPER_MAP_OFFSET..UPPER_MAP_OFFSET + 4].copy_from_slice(&0u32.to_le_bytes());

        for i in 0..10 {
            let offset = 0x40 + i * 4;
            data[offset..offset + 4].copy_from_slice(&FLREG_UNUSED.to_le_bytes());
        }

        let regions: [(usize, u32); 4] = [
            (0, 0x0000_0000),             // descriptor: 0x000000-0x000fff
            (1, (0x0fff << 16) | 0x0500), // BIOS: 0x500000-0xffffff
            (2, (0x03ff << 16) | 0x0001), // ME: 0x001000-0x3fffff
            (8, (0x04ff << 16) | 0x0400), // EC: 0x400000-0x4fffff
        ];
        for (index, flreg) in regions {
            let offset = 0x40 + index * 4;
            data[offset..offset + 4].copy_from_slice(&flreg.to_le_bytes());
        }

        data
    }

    /// Build a Lunar or Panther Lake descriptor with populated FLREG10/11.
    fn make_lunar_panther_ifd(csso: u32, isl: u32) -> Vec<u8> {
        let mut data = vec![0x00; 0x1000];
        data[0..4].copy_from_slice(&IFD_SIGNATURE.to_le_bytes());
        data[4..8].copy_from_slice(&(0x04u32 << 16).to_le_bytes());
        data[8..12].copy_from_slice(&(isl << 24).to_le_bytes());

        // CSSL=3 selects the modern soft-strap encoding. CSSO distinguishes
        // Panther Lake (0x60) and Lunar Lake (0x70 with ISL 0x7d/0x7e).
        let flmap2 = (0x03u32 << 16) | (csso << 2);
        data[12..16].copy_from_slice(&flmap2.to_le_bytes());
        data[UPPER_MAP_OFFSET..UPPER_MAP_OFFSET + 4].copy_from_slice(&(1u32 << 24).to_le_bytes());

        for i in 0..16 {
            let offset = 0x40 + i * 4;
            data[offset..offset + 4].copy_from_slice(&FLREG_UNUSED.to_le_bytes());
        }

        let regions: [(usize, u32); 3] = [
            (0, 0x0000_0000),              // descriptor: 0x000000-0x000fff
            (10, (0x0001 << 16) | 0x0001), // PSE: 0x001000-0x001fff
            (11, (0x0002 << 16) | 0x0002), // region11: 0x002000-0x002fff
        ];
        for (index, flreg) in regions {
            let offset = 0x40 + index * 4;
            data[offset..offset + 4].copy_from_slice(&flreg.to_le_bytes());
        }

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

    #[test]
    fn test_parse_standard_ifd_location() {
        let data = make_test_ifd_at(0);
        assert!(has_ifd(&data));
        assert_eq!(parse_ifd(&data).unwrap().regions.len(), 3);
    }

    #[test]
    fn test_parse_sunrise_point_with_reserved_nr() {
        let layout = parse_ifd(&make_sunrise_point_ifd()).unwrap();

        assert_eq!(layout.regions.len(), 4);
        assert_eq!(layout.find_region("descriptor").unwrap().start, 0);
        assert_eq!(layout.find_region("me").unwrap().start, 0x001000);
        assert_eq!(layout.find_region("ec").unwrap().start, 0x400000);
        assert_eq!(layout.find_region("bios").unwrap().start, 0x500000);
    }

    #[test]
    fn test_lunar_and_panther_lake_region_names() {
        for data in [
            make_lunar_panther_ifd(0x70, 0x7d),
            make_lunar_panther_ifd(0x60, 0x9a),
        ] {
            let layout = parse_ifd(&data).unwrap();

            assert_eq!(layout.find_region("pse").unwrap().start, 0x001000);
            assert_eq!(layout.find_region("region11").unwrap().start, 0x002000);
            assert!(layout.find_region("ie").is_none());
            assert!(layout.find_region("10gbe").is_none());
        }
    }
}

//! Flash chip type definitions

#[cfg(feature = "alloc")]
use alloc::{string::String, vec::Vec};

use super::features::Features;

/// Region definition: size and count pair
///
/// Represents a contiguous region of blocks with the same size.
/// For non-uniform flash chips (like boot sector chips), multiple
/// regions can be combined to describe the full layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct EraseRegion {
    /// Size of each block in this region, in bytes
    pub size: u32,
    /// Number of blocks in this region
    pub count: u32,
}

impl EraseRegion {
    /// Create a new erase region
    pub const fn new(size: u32, count: u32) -> Self {
        Self { size, count }
    }

    /// Get the total size of this region in bytes
    pub const fn total_size(&self) -> u32 {
        self.size * self.count
    }
}

/// Erase block definition
///
/// Represents an erase operation supported by a flash chip.
/// Each erase block has an opcode and one or more regions.
/// For uniform chips, there's typically one region.
/// For non-uniform chips (like PT/PU boot sector variants),
/// there may be multiple regions with different block sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct EraseBlock {
    /// SPI opcode for this erase operation
    pub opcode: u8,
    /// Number of regions for this erase operation
    pub region_count: u8,
    /// Regions for this erase operation (up to 8 regions)
    pub regions: [EraseRegion; 8],
}

impl EraseBlock {
    /// Create a new erase block with a single uniform region
    pub const fn new(opcode: u8, size: u32) -> Self {
        Self {
            opcode,
            region_count: 1,
            regions: [
                EraseRegion::new(size, 1),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
            ],
        }
    }

    /// Create an erase block from multiple regions
    pub const fn with_regions(opcode: u8, regions: &[EraseRegion]) -> Self {
        let mut eb = Self {
            opcode,
            region_count: regions.len() as u8,
            regions: [
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
                EraseRegion::new(0, 0),
            ],
        };
        let mut i = 0;
        while i < regions.len() && i < 8 {
            eb.regions[i] = regions[i];
            i += 1;
        }
        eb
    }

    /// Get the active regions for this erase block
    pub fn regions(&self) -> &[EraseRegion] {
        &self.regions[..self.region_count as usize]
    }

    /// Get the total size covered by this erase operation
    pub fn total_size(&self) -> u32 {
        self.regions().iter().map(|r| r.total_size()).sum()
    }

    /// Check if this is a uniform erase (single region)
    pub fn is_uniform(&self) -> bool {
        self.region_count == 1
    }

    /// Get the uniform block size (only valid if is_uniform() is true)
    pub fn uniform_size(&self) -> Option<u32> {
        if self.is_uniform() {
            Some(self.regions[0].size)
        } else {
            None
        }
    }

    /// Get the minimum block size across all regions
    pub fn min_block_size(&self) -> u32 {
        self.regions().iter().map(|r| r.size).min().unwrap_or(0)
    }

    /// Get the maximum block size across all regions
    pub fn max_block_size(&self) -> u32 {
        self.regions().iter().map(|r| r.size).max().unwrap_or(0)
    }

    /// Get the block size at a given offset within this erase operation's coverage.
    ///
    /// For uniform erase blocks, this returns the same size regardless of offset.
    /// For non-uniform layouts (boot sector chips), this returns the block size
    /// for the region containing the given offset.
    pub fn block_size_at_offset(&self, offset: u32) -> Option<u32> {
        let mut current_offset = 0u32;
        for region in self.regions() {
            let region_end = current_offset + region.total_size();
            if offset < region_end {
                return Some(region.size);
            }
            current_offset = region_end;
        }
        None
    }
}

/// Write granularity
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub enum WriteGranularity {
    /// Can write individual bits (1->0 only)
    Bit,
    /// Can write individual bytes
    Byte,
    /// Must write full pages
    #[default]
    Page,
}

/// Test status for a chip operation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub enum TestStatus {
    /// Not tested
    #[default]
    Untested,
    /// Tested and working
    Ok,
    /// Tested but has issues
    Bad,
    /// Not applicable for this chip
    Na,
}

/// Test results for various chip operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct ChipTestStatus {
    /// Probe/identification
    pub probe: TestStatus,
    /// Read operation
    pub read: TestStatus,
    /// Erase operation
    pub erase: TestStatus,
    /// Write/program operation
    pub write: TestStatus,
    /// Write protection
    pub wp: TestStatus,
}

/// Flash chip definition (owned version for runtime use)
///
/// This structure contains all the information needed to identify and
/// interact with a specific flash chip model. Uses owned types (String, Vec)
/// for runtime flexibility.
#[derive(Debug, Clone)]
#[cfg(feature = "alloc")]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct FlashChip {
    /// Vendor name (e.g., "Winbond")
    pub vendor: String,
    /// Chip model name (e.g., "W25Q128FV")
    pub name: String,
    /// JEDEC manufacturer ID (first byte of RDID response)
    pub jedec_manufacturer: u8,
    /// JEDEC device ID (second and third bytes of RDID response)
    pub jedec_device: u16,
    /// Total flash size in bytes
    pub total_size: u32,
    /// Page size in bytes (for programming)
    pub page_size: u16,
    /// Feature flags
    #[cfg_attr(feature = "std", serde(default))]
    pub features: Features,
    /// Minimum operating voltage in millivolts
    #[cfg_attr(feature = "std", serde(default = "default_voltage_min"))]
    pub voltage_min_mv: u16,
    /// Maximum operating voltage in millivolts
    #[cfg_attr(feature = "std", serde(default = "default_voltage_max"))]
    pub voltage_max_mv: u16,
    /// Write granularity
    #[cfg_attr(feature = "std", serde(default))]
    pub write_granularity: WriteGranularity,
    /// Available erase block sizes (smallest to largest)
    pub erase_blocks: Vec<EraseBlock>,
    /// Test status
    #[cfg_attr(feature = "std", serde(default))]
    pub tested: ChipTestStatus,
}

#[cfg(feature = "std")]
fn default_voltage_min() -> u16 {
    2700
}

#[cfg(feature = "std")]
fn default_voltage_max() -> u16 {
    3600
}

/// Flash chip definition (static/const version for no_std)
///
/// This structure uses static references for zero-cost embedded use.
#[derive(Debug, Clone, Copy)]
#[cfg(not(feature = "alloc"))]
pub struct FlashChip {
    /// Vendor name (e.g., "Winbond")
    pub vendor: &'static str,
    /// Chip model name (e.g., "W25Q128FV")
    pub name: &'static str,
    /// JEDEC manufacturer ID (first byte of RDID response)
    pub jedec_manufacturer: u8,
    /// JEDEC device ID (second and third bytes of RDID response)
    pub jedec_device: u16,
    /// Total flash size in bytes
    pub total_size: u32,
    /// Page size in bytes (for programming)
    pub page_size: u16,
    /// Feature flags
    pub features: Features,
    /// Minimum operating voltage in millivolts
    pub voltage_min_mv: u16,
    /// Maximum operating voltage in millivolts
    pub voltage_max_mv: u16,
    /// Write granularity
    pub write_granularity: WriteGranularity,
    /// Available erase block sizes (smallest to largest)
    pub erase_blocks: &'static [EraseBlock],
    /// Test status
    pub tested: ChipTestStatus,
}

impl FlashChip {
    /// Get the JEDEC ID as a 24-bit value (manufacturer << 16 | device)
    pub fn jedec_id(&self) -> u32 {
        ((self.jedec_manufacturer as u32) << 16) | (self.jedec_device as u32)
    }

    /// Check if this chip matches the given JEDEC ID
    pub fn matches_jedec_id(&self, manufacturer: u8, device: u16) -> bool {
        self.jedec_manufacturer == manufacturer && self.jedec_device == device
    }

    /// Check if this chip requires 4-byte addressing
    pub fn requires_4byte_addr(&self) -> bool {
        self.total_size > 16 * 1024 * 1024
    }

    /// Get the smallest erase block size across all erase operations
    pub fn min_erase_size(&self) -> Option<u32> {
        self.erase_blocks()
            .iter()
            .map(|eb| eb.min_block_size())
            .filter(|&s| s > 0)
            .min()
    }

    /// Get the largest erase block size (excluding chip erase)
    pub fn max_erase_size(&self) -> Option<u32> {
        self.erase_blocks()
            .iter()
            .filter(|eb| eb.total_size() < self.total_size)
            .map(|eb| eb.max_block_size())
            .max()
    }

    /// Find an erase block that can erase a region of the given size
    pub fn erase_block_for_size(&self, size: u32) -> Option<&EraseBlock> {
        self.erase_blocks()
            .iter()
            .find(|eb| eb.regions().iter().any(|r| r.size == size))
    }

    /// Check if a given address and length are aligned to an erase block boundary
    pub fn is_erase_aligned(&self, addr: u32, len: u32) -> bool {
        if let Some(min_erase) = self.min_erase_size() {
            addr.is_multiple_of(min_erase) && len.is_multiple_of(min_erase)
        } else {
            false
        }
    }

    /// Get vendor name as a string slice
    #[cfg(feature = "alloc")]
    pub fn vendor(&self) -> &str {
        &self.vendor
    }

    /// Get chip name as a string slice
    #[cfg(feature = "alloc")]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get erase blocks as a slice
    #[cfg(feature = "alloc")]
    pub fn erase_blocks(&self) -> &[EraseBlock] {
        &self.erase_blocks
    }

    /// Get vendor name as a string slice
    #[cfg(not(feature = "alloc"))]
    pub fn vendor(&self) -> &str {
        self.vendor
    }

    /// Get chip name as a string slice
    #[cfg(not(feature = "alloc"))]
    pub fn name(&self) -> &str {
        self.name
    }

    /// Get erase blocks as a slice
    #[cfg(not(feature = "alloc"))]
    pub fn erase_blocks(&self) -> &[EraseBlock] {
        self.erase_blocks
    }
}

/// JEDEC manufacturer IDs
pub mod manufacturer {
    /// AMD/Spansion
    pub const AMD: u8 = 0x01;
    /// Atmel
    pub const ATMEL: u8 = 0x1F;
    /// EON
    pub const EON: u8 = 0x1C;
    /// Fujitsu
    pub const FUJITSU: u8 = 0x04;
    /// GigaDevice
    pub const GIGADEVICE: u8 = 0xC8;
    /// Intel
    pub const INTEL: u8 = 0x89;
    /// ISSI
    pub const ISSI: u8 = 0x9D;
    /// Macronix
    pub const MACRONIX: u8 = 0xC2;
    /// Micron
    pub const MICRON: u8 = 0x20;
    /// PMC
    pub const PMC: u8 = 0x9D;
    /// Sanyo
    pub const SANYO: u8 = 0x62;
    /// SST
    pub const SST: u8 = 0xBF;
    /// ST (now Micron)
    pub const ST: u8 = 0x20;
    /// Winbond
    pub const WINBOND: u8 = 0xEF;
    /// XMC
    pub const XMC: u8 = 0x20;
}

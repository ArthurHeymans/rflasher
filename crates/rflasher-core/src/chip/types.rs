//! Flash chip type definitions

#[cfg(feature = "alloc")]
use alloc::{string::String, vec::Vec};

use super::features::Features;

/// Maximum number of erase regions per erase block (for no_std)
pub const MAX_ERASE_REGIONS: usize = 8;

/// Type alias for the regions collection.
///
/// Uses `heapless::Vec` for no_std (stack-allocated, max 8 regions),
/// or `alloc::vec::Vec` for std/alloc (heap-allocated, unlimited).
#[cfg(not(feature = "alloc"))]
pub type RegionVec = heapless::Vec<EraseRegion, MAX_ERASE_REGIONS>;

/// Type alias for the regions collection.
///
/// Uses `heapless::Vec` for no_std (stack-allocated, max 8 regions),
/// or `alloc::vec::Vec` for std/alloc (heap-allocated, unlimited).
#[cfg(feature = "alloc")]
pub type RegionVec = Vec<EraseRegion>;

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
    #[must_use]
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
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct EraseBlock {
    /// SPI opcode for this erase operation
    pub opcode: u8,
    /// Regions for this erase operation (up to 8 regions for no_std)
    pub regions: RegionVec,
}

impl EraseBlock {
    /// Create a new erase block with a single uniform region
    ///
    /// Note: This creates a block with count=1, suitable for chip erase operations.
    /// For sector/block erase operations, use `with_count()` instead to specify
    /// the number of blocks on the chip.
    pub fn new(opcode: u8, size: u32) -> Self {
        Self::with_count(opcode, size, 1)
    }

    /// Create a new erase block with a single uniform region and specified count
    ///
    /// This is the correct way to create sector/block erase definitions where
    /// multiple blocks of the same size exist on the chip.
    pub fn with_count(opcode: u8, size: u32, count: u32) -> Self {
        Self::with_regions(opcode, &[EraseRegion::new(size, count)])
    }

    /// Create an erase block from multiple regions
    #[cfg(feature = "alloc")]
    pub fn with_regions(opcode: u8, regions: &[EraseRegion]) -> Self {
        Self {
            opcode,
            regions: regions.to_vec(),
        }
    }

    /// Create an erase block from multiple regions
    #[cfg(not(feature = "alloc"))]
    pub fn with_regions(opcode: u8, regions: &[EraseRegion]) -> Self {
        let mut vec = RegionVec::new();
        for region in regions.iter().take(MAX_ERASE_REGIONS) {
            vec.push(*region).unwrap();
        }
        Self {
            opcode,
            regions: vec,
        }
    }

    /// Get the active regions for this erase block
    pub fn regions(&self) -> &[EraseRegion] {
        &self.regions
    }

    /// Get the total size covered by this erase operation
    #[must_use]
    pub fn total_size(&self) -> u32 {
        self.regions.iter().map(|r| r.total_size()).sum()
    }

    /// Check if this is a uniform erase (single region)
    #[must_use]
    pub fn is_uniform(&self) -> bool {
        self.regions.len() == 1
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
        self.regions.iter().map(|r| r.size).min().unwrap_or(0)
    }

    /// Get the maximum block size across all regions
    pub fn max_block_size(&self) -> u32 {
        self.regions.iter().map(|r| r.size).max().unwrap_or(0)
    }

    /// Check if this is a chip erase block (single large block, typically used for full chip erase)
    ///
    /// A chip erase block is characterized by having a single region with count=1.
    /// This is used by opcodes like 0xC7 or 0x60 which erase the entire chip at once.
    #[must_use]
    pub fn is_chip_erase(&self) -> bool {
        // Chip erase blocks have exactly one region with count=1
        // This distinguishes them from sector/block erase which have count > 1
        self.regions.len() == 1 && self.regions[0].count == 1
    }

    /// Get the block size at a given offset within this erase operation's coverage.
    ///
    /// For uniform erase blocks, this returns the same size regardless of offset.
    /// For non-uniform layouts (boot sector chips), this returns the block size
    /// for the region containing the given offset.
    pub fn block_size_at_offset(&self, offset: u32) -> Option<u32> {
        let mut current_offset = 0u32;
        for region in self.regions.iter() {
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
    #[must_use]
    pub fn jedec_id(&self) -> u32 {
        ((self.jedec_manufacturer as u32) << 16) | (self.jedec_device as u32)
    }

    /// Check if this chip matches the given JEDEC ID
    #[must_use]
    pub fn matches_jedec_id(&self, manufacturer: u8, device: u16) -> bool {
        self.jedec_manufacturer == manufacturer && self.jedec_device == device
    }

    /// Check if this chip requires 4-byte addressing
    #[must_use]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_chip_erase() {
        // Chip erase: single block with count=1
        let chip_erase = EraseBlock::new(0xC7, 8 * 1024 * 1024);
        assert!(chip_erase.is_chip_erase());
        assert!(chip_erase.is_uniform());

        // 4KB sector erase: count > 1
        let sector_erase = EraseBlock::with_count(0x20, 4096, 2048);
        assert!(!sector_erase.is_chip_erase());
        assert!(sector_erase.is_uniform());

        // 64KB block erase: count > 1
        let block_erase = EraseBlock::with_count(0xD8, 65536, 128);
        assert!(!block_erase.is_chip_erase());
        assert!(block_erase.is_uniform());
    }

    #[test]
    fn test_non_uniform_is_not_chip_erase() {
        // Non-uniform boot sector chip - not a chip erase
        let boot_sector = EraseBlock::with_regions(
            0xD8,
            &[
                EraseRegion::new(4096, 2),
                EraseRegion::new(8192, 1),
                EraseRegion::new(65536, 1),
            ],
        );
        assert!(!boot_sector.is_chip_erase());
        assert!(!boot_sector.is_uniform());
    }
}

/// JEDEC manufacturer IDs
///
/// Note: Some manufacturer IDs are shared between vendors. JEDEC bank
/// extensions would normally disambiguate, but many cheap flash chips
/// only report a single-byte ID. The duplicates here reflect real-world
/// usage in flashrom/flashprog chip databases.
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
    /// ISSI (shares 0x9D with PMC — disambiguated by device ID)
    pub const ISSI: u8 = 0x9D;
    /// Macronix
    pub const MACRONIX: u8 = 0xC2;
    /// Micron (shares 0x20 with ST and XMC — disambiguated by device ID)
    pub const MICRON: u8 = 0x20;
    /// PMC (shares 0x9D with ISSI — disambiguated by device ID)
    pub const PMC: u8 = 0x9D;
    /// Sanyo
    pub const SANYO: u8 = 0x62;
    /// SST
    pub const SST: u8 = 0xBF;
    /// ST / STMicroelectronics, now Micron (shares 0x20 with Micron and XMC)
    pub const ST: u8 = 0x20;
    /// Winbond
    pub const WINBOND: u8 = 0xEF;
    /// XMC (shares 0x20 with Micron and ST — disambiguated by device ID)
    pub const XMC: u8 = 0x20;
}

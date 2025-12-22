//! Flash chip type definitions

use super::features::Features;

/// Erase block definition
///
/// Represents a single erase block size supported by a flash chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EraseBlock {
    /// SPI opcode for this erase size
    pub opcode: u8,
    /// Size in bytes
    pub size: u32,
}

impl EraseBlock {
    /// Create a new erase block definition
    pub const fn new(opcode: u8, size: u32) -> Self {
        Self { opcode, size }
    }
}

/// Write granularity
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

/// Flash chip definition
///
/// This structure contains all the information needed to identify and
/// interact with a specific flash chip model.
#[derive(Debug, Clone)]
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
    pub const fn jedec_id(&self) -> u32 {
        ((self.jedec_manufacturer as u32) << 16) | (self.jedec_device as u32)
    }

    /// Check if this chip matches the given JEDEC ID
    pub const fn matches_jedec_id(&self, manufacturer: u8, device: u16) -> bool {
        self.jedec_manufacturer == manufacturer && self.jedec_device == device
    }

    /// Check if this chip requires 4-byte addressing
    pub const fn requires_4byte_addr(&self) -> bool {
        self.total_size > 16 * 1024 * 1024
    }

    /// Get the smallest erase block size
    pub fn min_erase_size(&self) -> Option<u32> {
        self.erase_blocks.first().map(|eb| eb.size)
    }

    /// Get the largest erase block size (excluding chip erase)
    pub fn max_erase_size(&self) -> Option<u32> {
        self.erase_blocks
            .iter()
            .filter(|eb| eb.size < self.total_size)
            .map(|eb| eb.size)
            .max()
    }

    /// Find an erase block that matches the given size
    pub fn erase_block_for_size(&self, size: u32) -> Option<&EraseBlock> {
        self.erase_blocks.iter().find(|eb| eb.size == size)
    }

    /// Check if a given address and length are aligned to an erase block boundary
    pub fn is_erase_aligned(&self, addr: u32, len: u32) -> bool {
        if let Some(min_erase) = self.min_erase_size() {
            addr.is_multiple_of(min_erase) && len.is_multiple_of(min_erase)
        } else {
            false
        }
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

// Include the generated chip database
include!(concat!(env!("OUT_DIR"), "/chips_generated.rs"));

/// Find a chip by its JEDEC ID
pub fn find_by_jedec_id(manufacturer: u8, device: u16) -> Option<&'static FlashChip> {
    CHIPS
        .iter()
        .find(|c| c.matches_jedec_id(manufacturer, device))
}

/// Find chips by name (case-insensitive partial match)
#[cfg(feature = "alloc")]
pub fn find_by_name(name: &str) -> alloc::vec::Vec<&'static FlashChip> {
    let name_lower = name.to_lowercase();
    CHIPS
        .iter()
        .filter(|c| c.name.to_lowercase().contains(&name_lower))
        .collect()
}

/// Find chips by vendor (case-insensitive partial match)
#[cfg(feature = "alloc")]
pub fn find_by_vendor(vendor: &str) -> alloc::vec::Vec<&'static FlashChip> {
    let vendor_lower = vendor.to_lowercase();
    CHIPS
        .iter()
        .filter(|c| c.vendor.to_lowercase().contains(&vendor_lower))
        .collect()
}

//! Shared RON schema and validation for runtime and compiled chip databases.

use serde::Deserialize;
use std::collections::HashSet;

/// Size specification with human-readable units
#[derive(Debug, Clone, Copy, Deserialize)]
pub enum Size {
    /// Size in bytes
    B(u32),
    /// Size in kibibytes (1024 bytes)
    KiB(u32),
    /// Size in mebibytes (1024 * 1024 bytes)
    MiB(u32),
}

impl Size {
    fn checked_bytes(self) -> Option<u32> {
        match self {
            Size::B(n) => Some(n),
            Size::KiB(n) => n.checked_mul(1024),
            Size::MiB(n) => n.checked_mul(1024 * 1024),
        }
    }

    /// Convert to bytes
    pub fn to_bytes(self) -> u32 {
        match self {
            Size::B(n) => n,
            Size::KiB(n) => n * 1024,
            Size::MiB(n) => n * 1024 * 1024,
        }
    }
}

// ============================================================================
// Feature flags - structured instead of string array
// ============================================================================

/// Feature flags for flash chips (structured for better RON ergonomics)
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default)]
pub struct FeaturesDef {
    // Write enable behavior
    /// Use WREN (0x06) before WRSR
    pub wrsr_wren: bool,
    /// Use EWSR (0x50) before WRSR (legacy SST)
    pub wrsr_ewsr: bool,
    /// WRSR writes both SR1 and SR2 with one command
    pub wrsr_ext: bool,

    // Read capabilities
    /// Supports Fast Read (0x0B)
    pub fast_read: bool,
    /// Supports Dual I/O read commands
    pub dual_io: bool,
    /// Supports Quad I/O read commands
    pub quad_io: bool,

    // 4-byte addressing
    /// Supports 4-byte address mode
    pub four_byte_addr: bool,
    /// Can enter 4BA mode with EN4B (0xB7)
    pub four_byte_enter: bool,
    /// Has native 4BA commands (0x13, 0x12, etc.)
    pub four_byte_native: bool,
    /// Supports extended address register (legacy coarse flag)
    pub ext_addr_reg: bool,
    /// Enter/exit 4BA mode requires WREN before 0xB7/0xE9
    pub four_byte_enter_wren: bool,
    /// Enter/exit 4BA mode by setting bit 7 of the extended address register
    pub four_byte_enter_ear7: bool,
    /// Extended Address Register uses 0xC5/0xC8
    pub ext_addr_reg_c5c8: bool,
    /// Extended Address Register uses 0x17/0x16
    pub ext_addr_reg_1716: bool,
    /// Native 4BA read instruction 0x13
    pub four_byte_read: bool,
    /// Native 4BA fast-read instruction 0x0C
    pub four_byte_fast_read: bool,
    /// Native 4BA page-program instruction 0x12
    pub four_byte_program: bool,
    /// Native 4BA dual-output read instruction 0x3C
    pub four_byte_dual_out_read: bool,
    /// Native 4BA dual-I/O read instruction 0xBC
    pub four_byte_dual_io_read: bool,
    /// Native 4BA quad-output read instruction 0x6C
    pub four_byte_quad_out_read: bool,
    /// Native 4BA quad-I/O read instruction 0xEC
    pub four_byte_quad_io_read: bool,

    // Special features
    /// Has OTP (One-Time Programmable) area
    pub otp: bool,
    /// Supports QPI mode (4-4-4)
    pub qpi: bool,
    /// Has security registers
    pub security_reg: bool,
    /// Supports SFDP (Serial Flash Discoverable Parameters)
    pub sfdp: bool,

    // Write behavior
    /// Byte-granularity writes (can write single bytes)
    pub write_byte: bool,
    /// Supports AAI (Auto Address Increment) word program
    pub aai_word: bool,
    /// SST26-style per-block protection register (requires WREN + ULBPR to unlock)
    pub sst26_bpr: bool,

    // Status register features
    /// Has status register 2
    pub status_reg_2: bool,
    /// Has status register 3
    pub status_reg_3: bool,
    /// Quad Enable bit is in SR2
    pub qe_sr2: bool,

    // Power management
    /// Supports deep power down
    pub deep_power_down: bool,

    // Write protection
    /// Top/Bottom protect bit available
    pub wp_tb: bool,
    /// Sector/Block protect bit available
    pub wp_sec: bool,
    /// Complement (CMP) bit available
    pub wp_cmp: bool,
}

// ============================================================================
// Chip definitions
// ============================================================================

/// Region definition: size and count pair
#[derive(Debug, Clone, Deserialize)]
pub struct RegionDef {
    /// Size of each block in this region
    pub size: Size,
    /// Number of blocks of this size
    pub count: u32,
}

/// Erase block definition in RON format
///
/// Supports both uniform blocks (single size across entire chip) and
/// non-uniform layouts (multiple regions with different sizes, common
/// in boot sector chips like PT/PU variants).
#[derive(Debug, Clone, Deserialize)]
pub struct EraseBlockDef {
    /// Regular SPI opcode for this erase operation
    pub opcode: u8,
    /// Native 4-byte-address SPI opcode for this erase operation, if supported
    pub opcode_4b: Option<u8>,
    /// Regions for this erase opcode.
    /// For uniform chips: single region covering the whole chip.
    /// For non-uniform chips: multiple regions (e.g., boot sector chips).
    pub regions: Vec<RegionDef>,
}

/// Test status for chip operations
#[derive(Debug, Clone, Copy, Deserialize, Default)]
pub enum TestStatus {
    #[default]
    Untested,
    Ok,
    Bad,
    Na,
}

/// Test results for various chip operations
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct TestStatusDef {
    pub probe: TestStatus,
    pub read: TestStatus,
    pub erase: TestStatus,
    pub write: TestStatus,
    pub wp: TestStatus,
}

/// Write granularity
#[derive(Debug, Clone, Copy, Deserialize, Default)]
pub enum WriteGranularity {
    Bit,
    Byte,
    #[default]
    Page,
}

/// Voltage range in millivolts
#[derive(Debug, Clone, Deserialize)]
pub struct VoltageDef {
    pub min: u16,
    pub max: u16,
}

impl Default for VoltageDef {
    fn default() -> Self {
        Self {
            min: 2700,
            max: 3600,
        }
    }
}

/// Single chip definition in RON format
#[derive(Debug, Clone, Deserialize)]
pub struct ChipDef {
    /// Chip model name (e.g., "W25Q128FV")
    pub name: String,
    /// JEDEC device ID (2 bytes, e.g., 0x4018)
    pub device_id: u16,
    /// Total flash size
    pub total_size: Size,
    /// Page size in bytes (for programming)
    #[serde(default = "default_page_size")]
    pub page_size: u16,
    /// Feature flags
    #[serde(default)]
    pub features: FeaturesDef,
    /// Operating voltage range
    #[serde(default)]
    pub voltage: VoltageDef,
    /// Write granularity
    #[serde(default)]
    pub write_granularity: WriteGranularity,
    /// Available erase block sizes
    pub erase_blocks: Vec<EraseBlockDef>,
    /// Test status
    #[serde(default)]
    pub tested: TestStatusDef,
}

fn default_page_size() -> u16 {
    256
}

/// Vendor definition containing multiple chips
#[derive(Debug, Clone, Deserialize)]
pub struct VendorDef {
    /// Vendor name (e.g., "Winbond")
    pub vendor: String,
    /// JEDEC manufacturer ID (1 byte, e.g., 0xEF)
    pub manufacturer_id: u8,
    /// List of chips from this vendor
    pub chips: Vec<ChipDef>,
}

impl VendorDef {
    /// Validate the chip database
    pub fn validate(&self) -> Result<(), String> {
        let mut chip_names = HashSet::new();

        for chip in &self.chips {
            if !chip_names.insert(chip.name.as_str()) {
                return Err(format!(
                    "Vendor {} defines chip name {} more than once",
                    self.vendor, chip.name
                ));
            }

            // Validate erase blocks
            if chip.erase_blocks.is_empty() {
                return Err(format!("Chip {} has no erase blocks defined", chip.name));
            }

            // Validate that chip erase exists
            let total_size = chip.total_size.checked_bytes().ok_or_else(|| {
                format!("Chip {} has a size exceeding u32 address space", chip.name)
            })?;
            let mut has_chip_erase = false;
            for eb in &chip.erase_blocks {
                let mut erase_total = 0u32;
                for region in &eb.regions {
                    let bytes = region
                        .size
                        .checked_bytes()
                        .and_then(|size| size.checked_mul(region.count))
                        .and_then(|size| erase_total.checked_add(size))
                        .ok_or_else(|| {
                            format!(
                                "Chip {} has erase regions exceeding u32 address space",
                                chip.name
                            )
                        })?;
                    erase_total = bytes;
                }
                has_chip_erase |= erase_total == total_size;
            }
            if !has_chip_erase {
                return Err(format!(
                    "Chip {} has no chip-erase block (size {} not found in erase_blocks)",
                    chip.name, total_size
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_validation_covers_duplicates_and_erase_geometry() {
        let ron = r#"(
            vendor: "Test", manufacturer_id: 1,
            chips: [(
                name: "Chip", device_id: 1, total_size: KiB(8),
                erase_blocks: [(opcode: 0x20, regions: [(size: KiB(4), count: 2)])],
            )],
        )"#;
        let mut vendor: VendorDef = ron::from_str(ron).unwrap();
        assert!(vendor.validate().is_ok());
        assert_eq!(vendor.chips[0].page_size, 256);

        vendor.chips.push(vendor.chips[0].clone());
        assert!(vendor.validate().unwrap_err().contains("more than once"));
        vendor.chips.pop();

        vendor.chips[0].erase_blocks[0].regions[0].count = 1;
        assert!(
            vendor
                .validate()
                .unwrap_err()
                .contains("no chip-erase block")
        );

        vendor.chips[0].erase_blocks[0].regions[0].count = u32::MAX;
        assert!(vendor.validate().unwrap_err().contains("exceeding u32"));
    }
}

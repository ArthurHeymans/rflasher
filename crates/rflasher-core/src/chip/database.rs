//! Chip database for runtime loading and lookup
//!
//! This module provides the `ChipDatabase` type for loading chip definitions
//! from RON files at runtime.

use alloc::{string::String, vec::Vec};
use std::fs;
use std::io;
use std::path::Path;

use super::types::{ChipTestStatus, EraseBlock, EraseRegion, FlashChip, TestStatus, WriteGranularity};
use super::Features;

/// Error type for chip database operations
#[derive(Debug)]
pub enum ChipDbError {
    /// I/O error reading files
    Io(io::Error),
    /// RON parsing error
    Parse(ron::error::SpannedError),
    /// Validation error
    Validation(String),
}

impl From<io::Error> for ChipDbError {
    fn from(e: io::Error) -> Self {
        ChipDbError::Io(e)
    }
}

impl From<ron::error::SpannedError> for ChipDbError {
    fn from(e: ron::error::SpannedError) -> Self {
        ChipDbError::Parse(e)
    }
}

impl std::fmt::Display for ChipDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChipDbError::Io(e) => write!(f, "I/O error: {}", e),
            ChipDbError::Parse(e) => write!(f, "Parse error: {}", e),
            ChipDbError::Validation(msg) => write!(f, "Validation error: {}", msg),
        }
    }
}

impl std::error::Error for ChipDbError {}

// ============================================================================
// RON deserialization types (intermediate format)
// ============================================================================

/// Size specification with human-readable units (for RON parsing)
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub enum Size {
    /// Size in bytes
    B(u32),
    /// Size in kibibytes (1024 bytes)
    KiB(u32),
    /// Size in mebibytes (1024 * 1024 bytes)
    MiB(u32),
}

impl Size {
    /// Convert to bytes
    pub fn to_bytes(self) -> u32 {
        match self {
            Size::B(n) => n,
            Size::KiB(n) => n * 1024,
            Size::MiB(n) => n * 1024 * 1024,
        }
    }
}

/// Feature flags for flash chips (RON format)
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(default)]
struct FeaturesDef {
    wrsr_wren: bool,
    wrsr_ewsr: bool,
    wrsr_ext: bool,
    fast_read: bool,
    dual_io: bool,
    quad_io: bool,
    four_byte_addr: bool,
    four_byte_enter: bool,
    four_byte_native: bool,
    ext_addr_reg: bool,
    otp: bool,
    qpi: bool,
    security_reg: bool,
    sfdp: bool,
    write_byte: bool,
    aai_word: bool,
    erase_4k: bool,
    erase_32k: bool,
    erase_64k: bool,
    status_reg_2: bool,
    status_reg_3: bool,
    qe_sr2: bool,
    deep_power_down: bool,
    wp_tb: bool,
    wp_sec: bool,
    wp_cmp: bool,
}

impl From<FeaturesDef> for Features {
    fn from(def: FeaturesDef) -> Self {
        let mut f = Features::empty();
        if def.wrsr_wren {
            f |= Features::WRSR_WREN;
        }
        if def.wrsr_ewsr {
            f |= Features::WRSR_EWSR;
        }
        if def.wrsr_ext {
            f |= Features::WRSR_EXT;
        }
        if def.fast_read {
            f |= Features::FAST_READ;
        }
        if def.dual_io {
            f |= Features::DUAL_IO;
        }
        if def.quad_io {
            f |= Features::QUAD_IO;
        }
        if def.four_byte_addr {
            f |= Features::FOUR_BYTE_ADDR;
        }
        if def.four_byte_enter {
            f |= Features::FOUR_BYTE_ENTER;
        }
        if def.four_byte_native {
            f |= Features::FOUR_BYTE_NATIVE;
        }
        if def.ext_addr_reg {
            f |= Features::EXT_ADDR_REG;
        }
        if def.otp {
            f |= Features::OTP;
        }
        if def.qpi {
            f |= Features::QPI;
        }
        if def.security_reg {
            f |= Features::SECURITY_REG;
        }
        if def.sfdp {
            f |= Features::SFDP;
        }
        if def.write_byte {
            f |= Features::WRITE_BYTE;
        }
        if def.aai_word {
            f |= Features::AAI_WORD;
        }
        if def.erase_4k {
            f |= Features::ERASE_4K;
        }
        if def.erase_32k {
            f |= Features::ERASE_32K;
        }
        if def.erase_64k {
            f |= Features::ERASE_64K;
        }
        if def.status_reg_2 {
            f |= Features::STATUS_REG_2;
        }
        if def.status_reg_3 {
            f |= Features::STATUS_REG_3;
        }
        if def.qe_sr2 {
            f |= Features::QE_SR2;
        }
        if def.deep_power_down {
            f |= Features::DEEP_POWER_DOWN;
        }
        if def.wp_tb {
            f |= Features::WP_TB;
        }
        if def.wp_sec {
            f |= Features::WP_SEC;
        }
        if def.wp_cmp {
            f |= Features::WP_CMP;
        }
        f
    }
}

/// Region definition: size and count pair
#[derive(Debug, Clone, serde::Deserialize)]
struct RegionDef {
    size: Size,
    count: u32,
}

/// Erase block definition in RON format
#[derive(Debug, Clone, serde::Deserialize)]
struct EraseBlockDef {
    opcode: u8,
    regions: Vec<RegionDef>,
}

/// Voltage range in millivolts
#[derive(Debug, Clone, serde::Deserialize)]
struct VoltageDef {
    min: u16,
    max: u16,
}

impl Default for VoltageDef {
    fn default() -> Self {
        Self {
            min: 2700,
            max: 3600,
        }
    }
}

/// Test status (RON format)
#[derive(Debug, Clone, Copy, serde::Deserialize, Default)]
enum TestStatusDef {
    #[default]
    Untested,
    Ok,
    Bad,
    Na,
}

impl From<TestStatusDef> for TestStatus {
    fn from(def: TestStatusDef) -> Self {
        match def {
            TestStatusDef::Untested => TestStatus::Untested,
            TestStatusDef::Ok => TestStatus::Ok,
            TestStatusDef::Bad => TestStatus::Bad,
            TestStatusDef::Na => TestStatus::Na,
        }
    }
}

/// Test results (RON format)
#[derive(Debug, Clone, serde::Deserialize, Default)]
#[serde(default)]
struct TestStatusesDef {
    probe: TestStatusDef,
    read: TestStatusDef,
    erase: TestStatusDef,
    write: TestStatusDef,
    wp: TestStatusDef,
}

impl From<TestStatusesDef> for ChipTestStatus {
    fn from(def: TestStatusesDef) -> Self {
        ChipTestStatus {
            probe: def.probe.into(),
            read: def.read.into(),
            erase: def.erase.into(),
            write: def.write.into(),
            wp: def.wp.into(),
        }
    }
}

/// Write granularity (RON format)
#[derive(Debug, Clone, Copy, serde::Deserialize, Default)]
enum WriteGranularityDef {
    Bit,
    Byte,
    #[default]
    Page,
}

impl From<WriteGranularityDef> for WriteGranularity {
    fn from(def: WriteGranularityDef) -> Self {
        match def {
            WriteGranularityDef::Bit => WriteGranularity::Bit,
            WriteGranularityDef::Byte => WriteGranularity::Byte,
            WriteGranularityDef::Page => WriteGranularity::Page,
        }
    }
}

/// Single chip definition in RON format
#[derive(Debug, Clone, serde::Deserialize)]
struct ChipDef {
    name: String,
    device_id: u16,
    total_size: Size,
    #[serde(default = "default_page_size")]
    page_size: u16,
    #[serde(default)]
    features: FeaturesDef,
    #[serde(default)]
    voltage: VoltageDef,
    #[serde(default)]
    write_granularity: WriteGranularityDef,
    erase_blocks: Vec<EraseBlockDef>,
    #[serde(default)]
    tested: TestStatusesDef,
}

fn default_page_size() -> u16 {
    256
}

/// Vendor definition containing multiple chips
#[derive(Debug, Clone, serde::Deserialize)]
struct VendorDef {
    vendor: String,
    manufacturer_id: u8,
    chips: Vec<ChipDef>,
}

// ============================================================================
// Chip database
// ============================================================================

/// Runtime chip database
///
/// Holds a collection of flash chip definitions that can be loaded from RON files.
#[derive(Debug, Clone, Default)]
pub struct ChipDatabase {
    chips: Vec<FlashChip>,
}

impl ChipDatabase {
    /// Create an empty chip database
    pub fn new() -> Self {
        Self { chips: Vec::new() }
    }

    /// Load chip definitions from a single RON file
    pub fn load_file(&mut self, path: &Path) -> Result<usize, ChipDbError> {
        let content = fs::read_to_string(path)?;
        self.load_ron(&content)
    }

    /// Load chip definitions from a RON string
    pub fn load_ron(&mut self, content: &str) -> Result<usize, ChipDbError> {
        let vendor_def: VendorDef = ron::from_str(content)?;
        let count = vendor_def.chips.len();

        for chip_def in vendor_def.chips {
            let chip = FlashChip {
                vendor: vendor_def.vendor.clone(),
                name: chip_def.name,
                jedec_manufacturer: vendor_def.manufacturer_id,
                jedec_device: chip_def.device_id,
                total_size: chip_def.total_size.to_bytes(),
                page_size: chip_def.page_size,
                features: chip_def.features.into(),
                voltage_min_mv: chip_def.voltage.min,
                voltage_max_mv: chip_def.voltage.max,
                write_granularity: chip_def.write_granularity.into(),
                erase_blocks: chip_def
                    .erase_blocks
                    .into_iter()
                    .map(|eb| {
                        let regions: Vec<EraseRegion> = eb
                            .regions
                            .iter()
                            .map(|r| EraseRegion::new(r.size.to_bytes(), r.count))
                            .collect();
                        EraseBlock::with_regions(eb.opcode, &regions)
                    })
                    .collect(),
                tested: chip_def.tested.into(),
            };
            self.chips.push(chip);
        }

        Ok(count)
    }

    /// Load all RON files from a directory
    pub fn load_dir(&mut self, dir: &Path) -> Result<usize, ChipDbError> {
        let mut total = 0;

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "ron") {
                total += self.load_file(&path)?;
            }
        }

        Ok(total)
    }

    /// Get all chips in the database
    pub fn chips(&self) -> &[FlashChip] {
        &self.chips
    }

    /// Get the number of chips in the database
    pub fn len(&self) -> usize {
        self.chips.len()
    }

    /// Check if the database is empty
    pub fn is_empty(&self) -> bool {
        self.chips.is_empty()
    }

    /// Find a chip by its JEDEC ID
    pub fn find_by_jedec_id(&self, manufacturer: u8, device: u16) -> Option<&FlashChip> {
        self.chips
            .iter()
            .find(|c| c.matches_jedec_id(manufacturer, device))
    }

    /// Find chips by name (case-insensitive partial match)
    pub fn find_by_name(&self, name: &str) -> Vec<&FlashChip> {
        let name_lower = name.to_lowercase();
        self.chips
            .iter()
            .filter(|c| c.name.to_lowercase().contains(&name_lower))
            .collect()
    }

    /// Find chips by vendor (case-insensitive partial match)
    pub fn find_by_vendor(&self, vendor: &str) -> Vec<&FlashChip> {
        let vendor_lower = vendor.to_lowercase();
        self.chips
            .iter()
            .filter(|c| c.vendor.to_lowercase().contains(&vendor_lower))
            .collect()
    }

    /// Iterate over all chips
    pub fn iter(&self) -> impl Iterator<Item = &FlashChip> {
        self.chips.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_ron() {
        let ron = r#"
        (
            vendor: "Winbond",
            manufacturer_id: 0xEF,
            chips: [
                (
                    name: "W25Q128FV",
                    device_id: 0x4018,
                    total_size: MiB(16),
                    page_size: 256,
                    features: (
                        wrsr_wren: true,
                        fast_read: true,
                        dual_io: true,
                        quad_io: true,
                    ),
                    voltage: (min: 2700, max: 3600),
                    erase_blocks: [
                        (opcode: 0x20, size: KiB(4)),
                        (opcode: 0x52, size: KiB(32)),
                        (opcode: 0xD8, size: KiB(64)),
                        (opcode: 0xC7, size: MiB(16)),
                    ],
                    tested: (probe: Ok, read: Ok, erase: Ok, write: Ok),
                ),
            ],
        )
        "#;

        let mut db = ChipDatabase::new();
        let count = db.load_ron(ron).unwrap();

        assert_eq!(count, 1);
        assert_eq!(db.len(), 1);

        let chip = db.find_by_jedec_id(0xEF, 0x4018).unwrap();
        assert_eq!(chip.name, "W25Q128FV");
        assert_eq!(chip.vendor, "Winbond");
        assert_eq!(chip.total_size, 16 * 1024 * 1024);
        assert!(chip.features.contains(Features::WRSR_WREN));
        assert!(chip.features.contains(Features::FAST_READ));
    }

    #[test]
    fn test_size_conversion() {
        assert_eq!(Size::B(256).to_bytes(), 256);
        assert_eq!(Size::KiB(4).to_bytes(), 4096);
        assert_eq!(Size::KiB(64).to_bytes(), 65536);
        assert_eq!(Size::MiB(1).to_bytes(), 1048576);
        assert_eq!(Size::MiB(16).to_bytes(), 16777216);
    }
}

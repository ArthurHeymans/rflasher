//! rflasher-chips-codegen - Build-time code generator for flash chip database
//!
//! This crate parses RON chip definitions and generates Rust code
//! that can be included in rflasher-core at build time.

use proc_macro2::{Literal, TokenStream};
use quote::quote;
use serde::Deserialize;

use std::fs;
use std::io;
use std::path::Path;

/// Error type for codegen operations
#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Ron(ron::error::SpannedError),
    Validation(String),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<ron::error::SpannedError> for Error {
    fn from(e: ron::error::SpannedError) -> Self {
        Error::Ron(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "IO error: {}", e),
            Error::Ron(e) => write!(f, "RON parse error: {}", e),
            Error::Validation(msg) => write!(f, "Validation error: {}", msg),
        }
    }
}

impl std::error::Error for Error {}

// ============================================================================
// Size types - makes RON files more readable
// ============================================================================

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
    /// Supports extended address register
    pub ext_addr_reg: bool,

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

    // Erase behavior
    /// Has 4KB sector erase
    pub erase_4k: bool,
    /// Has 32KB block erase
    pub erase_32k: bool,
    /// Has 64KB block erase
    pub erase_64k: bool,

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

impl FeaturesDef {
    /// Generate token stream for Features bitflags
    fn to_tokens(self) -> TokenStream {
        let mut flags = Vec::new();

        if self.wrsr_wren {
            flags.push(quote!(Features::WRSR_WREN));
        }
        if self.wrsr_ewsr {
            flags.push(quote!(Features::WRSR_EWSR));
        }
        if self.wrsr_ext {
            flags.push(quote!(Features::WRSR_EXT));
        }
        if self.fast_read {
            flags.push(quote!(Features::FAST_READ));
        }
        if self.dual_io {
            flags.push(quote!(Features::DUAL_IO));
        }
        if self.quad_io {
            flags.push(quote!(Features::QUAD_IO));
        }
        if self.four_byte_addr {
            flags.push(quote!(Features::FOUR_BYTE_ADDR));
        }
        if self.four_byte_enter {
            flags.push(quote!(Features::FOUR_BYTE_ENTER));
        }
        if self.four_byte_native {
            flags.push(quote!(Features::FOUR_BYTE_NATIVE));
        }
        if self.ext_addr_reg {
            flags.push(quote!(Features::EXT_ADDR_REG));
        }
        if self.otp {
            flags.push(quote!(Features::OTP));
        }
        if self.qpi {
            flags.push(quote!(Features::QPI));
        }
        if self.security_reg {
            flags.push(quote!(Features::SECURITY_REG));
        }
        if self.sfdp {
            flags.push(quote!(Features::SFDP));
        }
        if self.write_byte {
            flags.push(quote!(Features::WRITE_BYTE));
        }
        if self.aai_word {
            flags.push(quote!(Features::AAI_WORD));
        }
        if self.erase_4k {
            flags.push(quote!(Features::ERASE_4K));
        }
        if self.erase_32k {
            flags.push(quote!(Features::ERASE_32K));
        }
        if self.erase_64k {
            flags.push(quote!(Features::ERASE_64K));
        }
        if self.status_reg_2 {
            flags.push(quote!(Features::STATUS_REG_2));
        }
        if self.status_reg_3 {
            flags.push(quote!(Features::STATUS_REG_3));
        }
        if self.qe_sr2 {
            flags.push(quote!(Features::QE_SR2));
        }
        if self.deep_power_down {
            flags.push(quote!(Features::DEEP_POWER_DOWN));
        }
        if self.wp_tb {
            flags.push(quote!(Features::WP_TB));
        }
        if self.wp_sec {
            flags.push(quote!(Features::WP_SEC));
        }
        if self.wp_cmp {
            flags.push(quote!(Features::WP_CMP));
        }

        if flags.is_empty() {
            quote!(Features::empty())
        } else {
            let first = &flags[0];
            let rest = &flags[1..];
            quote!(#first #(.union(#rest))*)
        }
    }
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
    /// SPI opcode for this erase operation
    pub opcode: u8,
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

impl TestStatus {
    fn to_tokens(self) -> TokenStream {
        match self {
            TestStatus::Untested => quote!(TestStatus::Untested),
            TestStatus::Ok => quote!(TestStatus::Ok),
            TestStatus::Bad => quote!(TestStatus::Bad),
            TestStatus::Na => quote!(TestStatus::Na),
        }
    }
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

impl TestStatusDef {
    fn to_tokens(&self) -> TokenStream {
        let probe = self.probe.to_tokens();
        let read = self.read.to_tokens();
        let erase = self.erase.to_tokens();
        let write = self.write.to_tokens();
        let wp = self.wp.to_tokens();

        quote! {
            ChipTestStatus {
                probe: #probe,
                read: #read,
                erase: #erase,
                write: #write,
                wp: #wp,
            }
        }
    }
}

/// Write granularity
#[derive(Debug, Clone, Copy, Deserialize, Default)]
pub enum WriteGranularity {
    Bit,
    Byte,
    #[default]
    Page,
}

impl WriteGranularity {
    fn to_tokens(self) -> TokenStream {
        match self {
            WriteGranularity::Bit => quote!(WriteGranularity::Bit),
            WriteGranularity::Byte => quote!(WriteGranularity::Byte),
            WriteGranularity::Page => quote!(WriteGranularity::Page),
        }
    }
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

/// Complete chip database
#[derive(Debug, Clone)]
pub struct ChipDatabase {
    pub vendors: Vec<VendorDef>,
}

impl ChipDatabase {
    /// Load chip database from a directory containing RON files
    pub fn load_from_dir(dir: &Path) -> Result<Self, Error> {
        let mut vendors = Vec::new();

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "ron") {
                let content = fs::read_to_string(&path)?;
                let vendor: VendorDef = ron::from_str(&content)?;
                vendors.push(vendor);
            }
        }

        // Sort vendors by name for consistent output
        vendors.sort_by(|a, b| a.vendor.cmp(&b.vendor));

        Ok(ChipDatabase { vendors })
    }

    /// Load chip database from a single RON file (for testing)
    pub fn load_from_file(path: &Path) -> Result<VendorDef, Error> {
        let content = fs::read_to_string(path)?;
        let vendor: VendorDef = ron::from_str(&content)?;
        Ok(vendor)
    }

    /// Validate the chip database
    pub fn validate(&self) -> Result<(), Error> {
        for vendor in &self.vendors {
            for chip in &vendor.chips {
                // Validate erase blocks
                if chip.erase_blocks.is_empty() {
                    return Err(Error::Validation(format!(
                        "Chip {} has no erase blocks defined",
                        chip.name
                    )));
                }

                // Validate that chip erase exists
                let total_size = chip.total_size.to_bytes();
                let has_chip_erase = chip.erase_blocks.iter().any(|eb| {
                    // Check if this erase block covers the entire chip
                    let erase_total: u32 =
                        eb.regions.iter().map(|r| r.size.to_bytes() * r.count).sum();
                    erase_total == total_size
                });
                if !has_chip_erase {
                    return Err(Error::Validation(format!(
                        "Chip {} has no chip-erase block (size {} not found in erase_blocks)",
                        chip.name, total_size
                    )));
                }
            }
        }

        Ok(())
    }

    /// Generate Rust code for the chip database
    pub fn generate_code(&self) -> String {
        let mut chip_defs = Vec::new();

        for vendor in &self.vendors {
            for chip in &vendor.chips {
                // Generate erase blocks using constructors
                let erase_blocks: Vec<_> = chip
                    .erase_blocks
                    .iter()
                    .map(|eb| {
                        let opcode = Literal::u8_unsuffixed(eb.opcode);

                        if eb.regions.len() == 1 {
                            // Uniform erase block - use the simple constructor
                            let size = Literal::u32_unsuffixed(eb.regions[0].size.to_bytes());
                            let count = Literal::u32_unsuffixed(eb.regions[0].count);
                            if eb.regions[0].count == 1 {
                                // Single block (e.g., chip erase) - simplest form
                                quote!(EraseBlock::new(#opcode, #size))
                            } else {
                                // Multiple uniform blocks
                                quote!(EraseBlock::with_count(#opcode, #size, #count))
                            }
                        } else {
                            // Non-uniform erase block - use with_regions
                            let regions: Vec<_> = eb
                                .regions
                                .iter()
                                .map(|region| {
                                    let size = Literal::u32_unsuffixed(region.size.to_bytes());
                                    let count = Literal::u32_unsuffixed(region.count);
                                    quote!(EraseRegion::new(#size, #count))
                                })
                                .collect();
                            quote!(EraseBlock::with_regions(#opcode, &[#(#regions),*]))
                        }
                    })
                    .collect();

                // Generate chip definition
                let vendor_name = &vendor.vendor;
                let chip_name = &chip.name;
                let mfr_id = Literal::u8_unsuffixed(vendor.manufacturer_id);
                let dev_id = Literal::u16_unsuffixed(chip.device_id);
                let total_size = Literal::u32_unsuffixed(chip.total_size.to_bytes());
                let page_size = Literal::u16_unsuffixed(chip.page_size);
                let features = chip.features.to_tokens();
                let voltage_min = Literal::u16_unsuffixed(chip.voltage.min);
                let voltage_max = Literal::u16_unsuffixed(chip.voltage.max);
                let write_gran = chip.write_granularity.to_tokens();
                let tested = chip.tested.to_tokens();

                chip_defs.push(quote! {
                    FlashChip {
                        vendor: #vendor_name,
                        name: #chip_name,
                        jedec_manufacturer: #mfr_id,
                        jedec_device: #dev_id,
                        total_size: #total_size,
                        page_size: #page_size,
                        features: #features,
                        voltage_min_mv: #voltage_min,
                        voltage_max_mv: #voltage_max,
                        write_granularity: #write_gran,
                        erase_blocks: vec![#(#erase_blocks),*],
                        tested: #tested,
                    }
                });
            }
        }

        let tokens = quote! {
            // Auto-generated by rflasher-chips-codegen
            // Do not edit manually!

            use once_cell::sync::Lazy;
            use alloc::vec;
            use alloc::vec::Vec;

            /// Static chip database
            ///
            /// Generated from RON files in chips/vendors/
            /// Lazily initialized on first access.
            pub static CHIPS: Lazy<Vec<FlashChip>> = Lazy::new(|| {
                vec![
                    #(#chip_defs),*
                ]
            });
        };

        // Format the output with prettyplease
        let syntax_tree = syn::parse2(tokens.clone()).expect("Failed to parse generated code");
        prettyplease::unparse(&syntax_tree)
    }

    /// Get total chip count
    pub fn chip_count(&self) -> usize {
        self.vendors.iter().map(|v| v.chips.len()).sum()
    }
}

/// Generate code from a chips directory and write to output file
pub fn generate(chips_dir: &Path, output_file: &Path) -> Result<(), Error> {
    let db = ChipDatabase::load_from_dir(chips_dir)?;
    db.validate()?;

    let code = db.generate_code();
    fs::write(output_file, code)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_vendor() {
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
                        (opcode: 0x20, regions: [(size: KiB(4), count: 4096)]),
                        (opcode: 0x52, regions: [(size: KiB(32), count: 512)]),
                        (opcode: 0xD8, regions: [(size: KiB(64), count: 256)]),
                        (opcode: 0xC7, regions: [(size: MiB(16), count: 1)]),
                    ],
                    tested: (probe: Ok, read: Ok, erase: Ok, write: Ok, wp: Ok),
                ),
            ],
        )
        "#;

        let vendor: VendorDef = ron::from_str(ron).unwrap();
        assert_eq!(vendor.vendor, "Winbond");
        assert_eq!(vendor.manufacturer_id, 0xEF);
        assert_eq!(vendor.chips.len(), 1);

        let chip = &vendor.chips[0];
        assert_eq!(chip.name, "W25Q128FV");
        assert_eq!(chip.device_id, 0x4018);
        assert_eq!(chip.total_size.to_bytes(), 16 * 1024 * 1024);
        assert!(chip.features.wrsr_wren);
        assert!(chip.features.fast_read);
    }

    #[test]
    fn test_parse_non_uniform_erase() {
        // Test parsing a chip with non-uniform erase blocks (boot sector chip)
        let ron = r#"
        (
            vendor: "AMIC",
            manufacturer_id: 0x37,
            chips: [
                (
                    name: "A25L10PT",
                    device_id: 0x2021,
                    total_size: KiB(128),
                    features: (wrsr_wren: true),
                    voltage: (min: 2700, max: 3600),
                    erase_blocks: [
                        (opcode: 0xD8, regions: [
                            (size: KiB(64), count: 1),
                            (size: KiB(32), count: 1),
                            (size: KiB(16), count: 1),
                            (size: KiB(8), count: 1),
                            (size: KiB(4), count: 2),
                        ]),
                        (opcode: 0xC7, regions: [(size: KiB(128), count: 1)]),
                    ],
                ),
            ],
        )
        "#;

        let vendor: VendorDef = ron::from_str(ron).unwrap();
        assert_eq!(vendor.chips.len(), 1);

        let chip = &vendor.chips[0];
        assert_eq!(chip.name, "A25L10PT");
        assert_eq!(chip.erase_blocks.len(), 2);

        // Check the non-uniform D8 erase block
        let d8_block = &chip.erase_blocks[0];
        assert_eq!(d8_block.opcode, 0xD8);
        assert_eq!(d8_block.regions.len(), 5);
        assert_eq!(d8_block.regions[0].size.to_bytes(), 64 * 1024);
        assert_eq!(d8_block.regions[0].count, 1);
        assert_eq!(d8_block.regions[4].size.to_bytes(), 4 * 1024);
        assert_eq!(d8_block.regions[4].count, 2);

        // Verify total size matches: 64 + 32 + 16 + 8 + 4*2 = 128KB
        let total: u32 = d8_block
            .regions
            .iter()
            .map(|r| r.size.to_bytes() * r.count)
            .sum();
        assert_eq!(total, 128 * 1024);
    }

    #[test]
    fn test_size_conversion() {
        assert_eq!(Size::B(256).to_bytes(), 256);
        assert_eq!(Size::KiB(4).to_bytes(), 4096);
        assert_eq!(Size::KiB(64).to_bytes(), 65536);
        assert_eq!(Size::MiB(1).to_bytes(), 1048576);
        assert_eq!(Size::MiB(16).to_bytes(), 16777216);
    }

    #[test]
    fn test_features_to_tokens() {
        let features = FeaturesDef {
            wrsr_wren: true,
            fast_read: true,
            ..Default::default()
        };
        let tokens = features.to_tokens();
        let s = tokens.to_string();
        assert!(s.contains("WRSR_WREN"));
        assert!(s.contains("FAST_READ"));
    }
}

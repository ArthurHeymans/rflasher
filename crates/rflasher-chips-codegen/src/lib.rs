//! rflasher-chips-codegen - Build-time code generator for flash chip database
//!
//! This crate parses RON chip definitions and generates Rust code
//! that can be included in rflasher-core at build time.

use proc_macro2::{Literal, TokenStream};
use quote::quote;
pub use rflasher_chip_defs::{
    ChipDef, EraseBlockDef, FeaturesDef, RegionDef, Size, TestStatus, TestStatusDef, VendorDef,
    VoltageDef, WriteGranularity,
};

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

/// Generate token stream for Features bitflags
fn features_to_tokens(def: FeaturesDef) -> TokenStream {
    let mut flags = Vec::new();

    if def.wrsr_wren {
        flags.push(quote!(Features::WRSR_WREN));
    }
    if def.wrsr_ewsr {
        flags.push(quote!(Features::WRSR_EWSR));
    }
    if def.wrsr_ext {
        flags.push(quote!(Features::WRSR_EXT));
    }
    if def.fast_read {
        flags.push(quote!(Features::FAST_READ));
    }
    if def.dual_io {
        flags.push(quote!(Features::DUAL_IO));
    }
    if def.quad_io {
        flags.push(quote!(Features::QUAD_IO));
    }
    if def.four_byte_addr {
        flags.push(quote!(Features::FOUR_BYTE_ADDR));
    }
    if def.four_byte_enter {
        flags.push(quote!(Features::FOUR_BYTE_ENTER));
    }
    if def.four_byte_native {
        flags.push(quote!(Features::FOUR_BYTE_NATIVE));
    }
    if def.ext_addr_reg || def.ext_addr_reg_c5c8 || def.ext_addr_reg_1716 {
        flags.push(quote!(Features::EXT_ADDR_REG));
    }
    if def.four_byte_enter_wren {
        flags.push(quote!(Features::FOUR_BYTE_ENTER_WREN));
    }
    if def.four_byte_enter_ear7 {
        flags.push(quote!(Features::FOUR_BYTE_ENTER_EAR7));
    }
    if def.ext_addr_reg_c5c8 {
        flags.push(quote!(Features::EXT_ADDR_REG_C5C8));
    }
    if def.ext_addr_reg_1716 {
        flags.push(quote!(Features::EXT_ADDR_REG_1716));
    }
    if def.four_byte_read {
        flags.push(quote!(Features::FOUR_BYTE_READ));
    }
    if def.four_byte_fast_read {
        flags.push(quote!(Features::FOUR_BYTE_FAST_READ));
    }
    if def.four_byte_program {
        flags.push(quote!(Features::FOUR_BYTE_PROGRAM));
    }
    if def.four_byte_dual_out_read {
        flags.push(quote!(Features::FOUR_BYTE_DUAL_OUT_READ));
    }
    if def.four_byte_dual_io_read {
        flags.push(quote!(Features::FOUR_BYTE_DUAL_IO_READ));
    }
    if def.four_byte_quad_out_read {
        flags.push(quote!(Features::FOUR_BYTE_QUAD_OUT_READ));
    }
    if def.four_byte_quad_io_read {
        flags.push(quote!(Features::FOUR_BYTE_QUAD_IO_READ));
    }
    if def.otp {
        flags.push(quote!(Features::OTP));
    }
    if def.qpi {
        flags.push(quote!(Features::QPI));
    }
    if def.security_reg {
        flags.push(quote!(Features::SECURITY_REG));
    }
    if def.sfdp {
        flags.push(quote!(Features::SFDP));
    }
    if def.write_byte {
        flags.push(quote!(Features::WRITE_BYTE));
    }
    if def.aai_word {
        flags.push(quote!(Features::AAI_WORD));
    }
    if def.sst26_bpr {
        flags.push(quote!(Features::SST26_BPR));
    }
    if def.status_reg_2 {
        flags.push(quote!(Features::STATUS_REG_2));
    }
    if def.status_reg_3 {
        flags.push(quote!(Features::STATUS_REG_3));
    }
    if def.qe_sr2 {
        flags.push(quote!(Features::QE_SR2));
    }
    if def.deep_power_down {
        flags.push(quote!(Features::DEEP_POWER_DOWN));
    }
    if def.wp_tb {
        flags.push(quote!(Features::WP_TB));
    }
    if def.wp_sec {
        flags.push(quote!(Features::WP_SEC));
    }
    if def.wp_cmp {
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
fn status_to_tokens(status: TestStatus) -> TokenStream {
    match status {
        TestStatus::Untested => quote!(TestStatus::Untested),
        TestStatus::Ok => quote!(TestStatus::Ok),
        TestStatus::Bad => quote!(TestStatus::Bad),
        TestStatus::Na => quote!(TestStatus::Na),
    }
}
fn test_statuses_to_tokens(statuses: &TestStatusDef) -> TokenStream {
    let probe = status_to_tokens(statuses.probe);
    let read = status_to_tokens(statuses.read);
    let erase = status_to_tokens(statuses.erase);
    let write = status_to_tokens(statuses.write);
    let wp = status_to_tokens(statuses.wp);

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
fn write_granularity_to_tokens(granularity: WriteGranularity) -> TokenStream {
    match granularity {
        WriteGranularity::Bit => quote!(WriteGranularity::Bit),
        WriteGranularity::Byte => quote!(WriteGranularity::Byte),
        WriteGranularity::Page => quote!(WriteGranularity::Page),
    }
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

    /// Validate the chip database using the shared RON rules.
    pub fn validate(&self) -> Result<(), Error> {
        for vendor in &self.vendors {
            vendor.validate().map_err(Error::Validation)?;
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
                        let has_opcode_4b = eb.opcode_4b.is_some();
                        let opcode_4b = eb.opcode_4b.map_or_else(
                            || quote!(None),
                            |opcode| {
                                let opcode = Literal::u8_unsuffixed(opcode);
                                quote!(Some(#opcode))
                            },
                        );

                        if eb.regions.len() == 1 && !has_opcode_4b {
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
                            quote!(EraseBlock::with_regions_and_4b(#opcode, #opcode_4b, &[#(#regions),*]))
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
                let features = features_to_tokens(chip.features);
                let voltage_min = Literal::u16_unsuffixed(chip.voltage.min);
                let voltage_max = Literal::u16_unsuffixed(chip.voltage.max);
                let write_gran = write_granularity_to_tokens(chip.write_granularity);
                let tested = test_statuses_to_tokens(&chip.tested);

                chip_defs.push(quote! {
                    FlashChip {
                        vendor: #vendor_name.to_string(),
                        name: #chip_name.to_string(),
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

            /// Static chip database
            ///
            /// Generated from the bundled `rflasher-chips/data/vendors` RON files.
            /// Lazily initialized on first access.
            pub static CHIPS: once_cell::sync::Lazy<Vec<FlashChip>> = once_cell::sync::Lazy::new(|| {
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
    fn static_validation_rejects_missing_chip_erase() {
        let invalid = r#"(
            vendor: "Test", manufacturer_id: 1,
            chips: [(
                name: "Broken", device_id: 1, total_size: KiB(8),
                erase_blocks: [(opcode: 0x20, regions: [(size: KiB(4), count: 1)])],
            )],
        )"#;
        let vendor: VendorDef = ron::from_str(invalid).unwrap();
        let db = ChipDatabase {
            vendors: vec![vendor],
        };
        assert!(
            matches!(db.validate(), Err(Error::Validation(msg)) if msg.contains("no chip-erase block"))
        );
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
        let tokens = features_to_tokens(features);
        let s = tokens.to_string();
        assert!(s.contains("WRSR_WREN"));
        assert!(s.contains("FAST_READ"));
    }
}

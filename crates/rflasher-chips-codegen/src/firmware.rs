//! Build-time selection for allocation-free firmware catalogs.

use crate::{ChipDatabase, ChipDef, Error, VendorDef, chip_to_tokens};
use quote::quote;
use rflasher_chip_types::name_matches;
use std::path::Path;

/// A board's selected chip definitions and explicit default profiles.
///
/// Generated descriptors borrow static storage in every feature configuration.
/// The generated `CATALOG` resolves matching board defaults and rejects unknown
/// or conflicting identities, without a heap.
#[derive(Debug)]
pub struct SelectedChips<'a> {
    chips: Vec<(&'a VendorDef, &'a ChipDef)>,
    defaults: Vec<usize>,
    crate_path: String,
}

fn resolve<'a>(
    chips: impl Iterator<Item = (&'a VendorDef, &'a ChipDef)> + Clone,
    name: &str,
) -> Result<(&'a VendorDef, &'a ChipDef), Error> {
    let (vendor, part) = match name.split_once("::") {
        Some((vendor, part)) => (Some(vendor), part),
        None => (None, name),
    };
    let chips =
        chips.filter(|(v, _)| vendor.is_none_or(|name| v.vendor.eq_ignore_ascii_case(name)));
    // Prefer an exact catalog entry over its pattern aliases.
    let exact = chips
        .clone()
        .any(|(_, c)| c.name.eq_ignore_ascii_case(part));
    let matches: Vec<_> = chips
        .filter(|(_, c)| {
            if exact {
                c.name.eq_ignore_ascii_case(part)
            } else {
                !c.features.explicit_profile && name_matches(&c.name, part)
            }
        })
        .collect();
    match matches.as_slice() {
        [] => Err(Error::Validation(format!("Unknown chip name: {name}"))),
        [first] => Ok(*first),
        _ => Err(Error::Validation(format!(
            "Ambiguous chip name: {name}; select an exact entry: {}",
            matches
                .iter()
                .map(|(v, c)| format!("{}::{}", v.vendor, c.name))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

impl ChipDatabase {
    /// Select chips by canonical name or a part number matched by a catalog alias.
    ///
    /// Names ignore ASCII case and may be qualified as `Vendor::PartNumber`.
    /// Unknown or ambiguous names fail the build rather than silently including
    /// nothing. Input order is preserved and repeated entries are deduplicated.
    pub fn select(&self, names: &[&str]) -> Result<SelectedChips<'_>, Error> {
        self.validate()?;
        let mut selected = SelectedChips {
            chips: Vec::new(),
            defaults: Vec::new(),
            crate_path: "rflasher_chip_types".into(),
        };
        for name in names {
            let entry = resolve(
                self.vendors
                    .iter()
                    .flat_map(|v| v.chips.iter().map(move |c| (v, c))),
                name,
            )?;
            if !selected
                .chips
                .iter()
                .any(|(_, c)| std::ptr::eq(*c, entry.1))
            {
                selected.chips.push(entry);
            }
        }
        Ok(selected)
    }
}

impl SelectedChips<'_> {
    /// Set ordered board-default profiles, all of which must already be selected.
    ///
    /// Defaults are emitted as `DEFAULT_CHIPS`, references into `CHIPS`. They are
    /// explicit board configuration, never a fallback for an unknown JEDEC ID.
    pub fn with_defaults(mut self, names: &[&str]) -> Result<Self, Error> {
        let mut defaults = Vec::new();
        for name in names {
            let (_, chip) = resolve(self.chips.iter().copied(), name)?;
            let index = self
                .chips
                .iter()
                .position(|(_, c)| std::ptr::eq(*c, chip))
                .unwrap();
            if !defaults.contains(&index) {
                defaults.push(index);
            }
        }
        self.defaults = defaults;
        Ok(self)
    }

    /// Use a type re-export or renamed target dependency, such as `rflasher_core::chip`.
    pub fn with_crate_path(mut self, path: &str) -> Result<Self, Error> {
        syn::parse_str::<syn::Path>(path)
            .map_err(|e| Error::Validation(format!("Invalid chip type path {path}: {e}")))?;
        self.crate_path = path.into();
        Ok(self)
    }

    /// Generate `CHIPS`, `DEFAULT_CHIPS` and the resolving `CATALOG` in read-only storage.
    ///
    /// Include it in a module with `include!(concat!(env!("OUT_DIR"), "/chips.rs"))`.
    /// The same output works with and without `alloc` on the target.
    pub fn generate_code(&self) -> String {
        let chips = self.chips.iter().map(|(v, c)| chip_to_tokens(v, c));
        let defaults = &self.defaults;
        let count = self.chips.len();
        let crate_path: syn::Path = syn::parse_str(&self.crate_path).expect("validated type path");
        let tokens = quote! {
          #[allow(dead_code)] // Consumers may use only the resolver or only diagnostics.
          mod generated_catalog {
            #[allow(unused_imports)]
            use #crate_path::{
                FlashChip, EraseBlock, EraseRegion, Features, Unlock,
                ChipTestStatus, TestStatus, WriteGranularity, ChipName, ChipSlice, ChipCatalog,
            };
            /// Board-selected chip descriptors; no runtime initialization or heap.
            pub static CHIPS: [FlashChip; #count] = [#(#chips),*];
            /// Explicit defaults; callers must still verify the connected chip.
            pub static DEFAULT_CHIPS: &[&FlashChip] = &[#(&CHIPS[#defaults]),*];
            /// Identify matching chips using the board-approved defaults, without guessing.
            pub static CATALOG: ChipCatalog<'static> = ChipCatalog::new(&CHIPS, DEFAULT_CHIPS);
          }
          #[allow(unused_imports)]
          pub use generated_catalog::{CHIPS, DEFAULT_CHIPS, CATALOG};
        };
        prettyplease::unparse(&syn::parse2(tokens).expect("valid catalog code"))
    }

    /// Write the selected firmware catalog to a build output file.
    pub fn write(&self, output_file: &Path) -> Result<(), Error> {
        std::fs::write(output_file, self.generate_code())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> ChipDatabase {
        let vendor: VendorDef = ron::from_str(
            r#"(
            vendor: "Test", manufacturer_id: 1,
            chips: [
                (name: "Part(A)", device_id: 1, total_size: KiB(8), erase_blocks: [
                    (opcode: 0x20, regions: [(size: KiB(4), count: 2)]),
                    (opcode: 0xc7, regions: [(size: KiB(8), count: 1)]),
                ]),
                (name: "Other", device_id: 1, total_size: KiB(8), erase_blocks: [
                    (opcode: 0xd8, opcode_4b: Some(0xdc), regions: [
                        (size: KiB(4), count: 1), (size: KiB(2), count: 2)
                    ]),
                    (opcode: 0xc7, regions: [(size: KiB(8), count: 1)]),
                ]),
            ],
        )"#,
        )
        .unwrap();
        ChipDatabase {
            vendors: vec![vendor],
        }
    }

    #[test]
    fn selection_matches_aliases_and_deduplicates() {
        let db = database();
        let selected = db.select(&["test::parta", "Part(A)"]).unwrap();
        assert_eq!(selected.chips.len(), 1);
        let code = selected
            .with_defaults(&["Part", "PartA"])
            .unwrap()
            .generate_code();
        assert!(code.contains("Part(A)"));
        assert!(!code.contains("Other"));
        assert!(!code.contains("vec!"));
        assert!(!code.contains("to_string"));
        assert_eq!(code.matches("&CHIPS[0").count(), 1);
    }

    #[test]
    fn unknown_ambiguous_and_unselected_defaults_fail() {
        let mut db = database();
        assert!(db.select(&["missing"]).is_err());
        assert!(
            db.select(&["PartA"])
                .unwrap()
                .with_defaults(&["Other"])
                .is_err()
        );
        let mut duplicate = db.vendors[0].clone();
        duplicate.vendor = "Another vendor".into();
        db.vendors.push(duplicate);
        assert!(db.select(&["PartA"]).is_err());
        assert!(db.select(&["Test::PartA"]).is_ok());
    }

    #[test]
    fn ambiguity_lists_exact_same_vendor_choices() {
        let mut db = database();
        let mut duplicate = db.vendors[0].chips[0].clone();
        duplicate.name = "Part.".into();
        db.vendors[0].chips.push(duplicate);
        let error = db.select(&["Test::PartA"]).unwrap_err().to_string();
        assert!(error.contains("Test::Part(A)"));
        assert!(error.contains("Test::Part."));
    }

    #[test]
    fn explicit_profiles_require_exact_names_not_pattern_matches() {
        let mut db = database();
        db.vendors[0].chips[0].features.explicit_profile = true;
        assert!(db.select(&["PartA"]).is_err());
        assert!(db.select(&["Part(A)"]).is_ok());
    }

    #[test]
    fn target_type_path_is_validated_and_can_use_core_reexports() {
        let db = database();
        assert!(
            db.select(&[])
                .unwrap()
                .with_crate_path("not a path!")
                .is_err()
        );
        let code = db
            .select(&["Part"])
            .unwrap()
            .with_crate_path("rflasher_core::chip")
            .unwrap()
            .generate_code();
        assert!(code.contains("use rflasher_core::chip::"));
        assert!(!code.contains("rflasher_chip_types"));
        assert!(code.contains("pub static CATALOG:"));
    }

    #[test]
    fn empty_selection_and_default_order_are_explicit() {
        let db = database();
        assert!(db.select(&[]).unwrap().chips.is_empty());
        let selected = db
            .select(&["Part", "Other"])
            .unwrap()
            .with_defaults(&["Other", "Part"])
            .unwrap();
        assert_eq!(selected.defaults, [1, 0]);
        let code = selected.generate_code();
        assert!(code.contains("Some(220)"));
        syn::parse_file(&code).unwrap();
    }
}

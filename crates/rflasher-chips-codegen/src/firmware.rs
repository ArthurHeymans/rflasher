//! Build-time selection for allocation-free firmware catalogs.

use crate::{ChipDatabase, ChipDef, Error, VendorDef, chip_to_tokens};
use quote::quote;
use rflasher_chip_types::name_matches;
use std::path::Path;

/// A board's selected chip definitions and explicit default profiles.
///
/// Generation reduces them to one operation profile per chip identity, and
/// fails where the selection leaves that undecided. Firmware then only looks
/// an observed identity up: it cannot meet an ambiguous catalog in the field,
/// and carries no descriptor it could never use. Generated descriptors borrow
/// static storage in every feature configuration.
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

/// Everything about a definition but its names, to tell aliases from
/// definitions that share an identity and operate differently.
fn operation_profile(vendor: &VendorDef, chip: &ChipDef) -> String {
    let unnamed = |name: &mut String| name.clear();
    let (mut vendor, mut chip) = (
        VendorDef {
            chips: Vec::new(),
            ..vendor.clone()
        },
        chip.clone(),
    );
    unnamed(&mut vendor.vendor);
    unnamed(&mut chip.name);
    vendor.manufacturer_id = 0;
    // Normalize all identity-derived fields for comparison only. The selected
    // descriptors below always retain their real, explicitly assigned IDs.
    chip.ids = vec![rflasher_chip_types::ChipIdentity::Rdid { id: 0 }];
    chip_to_tokens(&vendor, &chip).to_string()
}

impl<'a> SelectedChips<'a> {
    /// The profile to operate each selected identity with, in selection order.
    fn profiles(&self) -> Result<Vec<(&'a VendorDef, ChipDef)>, Error> {
        let name = |(v, c): &(&VendorDef, &ChipDef)| format!("{}::{}", v.vendor, c.name);
        let names = |chips: &[usize]| {
            chips
                .iter()
                .map(|i| name(&self.chips[*i]))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let identities: Vec<Vec<_>> = self
            .chips
            .iter()
            .map(|(_, c)| c.identities().collect())
            .collect();
        let alike = |chips: &[usize]| {
            let profile = |i: &usize| operation_profile(self.chips[*i].0, self.chips[*i].1);
            chips.iter().all(|i| profile(i) == profile(&chips[0]))
        };
        let mut assigned = vec![Vec::new(); self.chips.len()];
        let mut seen = Vec::new();
        for id in identities.iter().flatten() {
            if seen.contains(id) {
                continue;
            }
            seen.push(*id);
            let sharing: Vec<usize> = (0..self.chips.len())
                .filter(|i| identities[*i].contains(id))
                .collect();
            let defaults: Vec<usize> = self
                .defaults
                .iter()
                .copied()
                .filter(|i| sharing.contains(i))
                .collect();
            let chosen = if let Some(first) = defaults.first() {
                if !alike(&defaults) {
                    return Err(Error::Validation(format!(
                        "Conflicting default profiles for one chip identity: {}",
                        names(&defaults)
                    )));
                }
                *first
            } else if let Some(explicit) = sharing
                .iter()
                .find(|i| self.chips[**i].1.features.explicit_profile)
            {
                return Err(Error::Validation(format!(
                    "{} is an explicit profile: approve it as a default, or do not select it",
                    name(&self.chips[*explicit])
                )));
            } else if alike(&sharing) {
                sharing[0]
            } else {
                return Err(Error::Validation(format!(
                    "Selected chips share an identity but operate differently; \
                     make one of them a default: {}",
                    names(&sharing)
                )));
            };
            assigned[chosen].push(*id);
        }
        // A default can decide only one of another chip's alternative IDs.
        // Keep its other identities, but never reintroduce the overridden one.
        Ok(assigned
            .into_iter()
            .enumerate()
            .filter_map(|(index, ids)| {
                if ids.is_empty() {
                    return None;
                }
                let (vendor, chip) = self.chips[index];
                let mut chip = chip.clone();
                chip.ids = ids;
                Some((vendor, chip))
            })
            .collect())
    }

    /// Set the board's default profiles, all of which must already be selected.
    ///
    /// A default decides between selected entries that share a chip identity,
    /// and approves an explicit profile. It asserts board knowledge (such as a
    /// verified flash marking); it is never a fallback for an unknown identity.
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

    /// Generate `CHIPS`, one profile per chip identity, and the `CATALOG` that
    /// looks an identity up in them, in read-only storage.
    ///
    /// Fails if selected entries share an identity and operate differently
    /// without a default deciding between them, if defaults conflict, or if an
    /// explicit profile is selected without being a default.
    ///
    /// Include it in a module with `include!(concat!(env!("OUT_DIR"), "/chips.rs"))`.
    /// The same output works with and without `alloc` on the target.
    pub fn generate_code(&self) -> Result<String, Error> {
        let profiles = self.profiles()?;
        let chips = profiles.iter().map(|(v, c)| chip_to_tokens(v, c));
        let count = profiles.len();
        let crate_path: syn::Path = syn::parse_str(&self.crate_path).expect("validated type path");
        let tokens = quote! {
          #[allow(dead_code)] // Consumers may use only the resolver or only diagnostics.
          mod generated_catalog {
            #[allow(unused_imports)]
            use #crate_path::{
                FlashChip, EraseBlock, EraseRegion, Features, Unlock,
                ChipTestStatus, TestStatus, WriteGranularity, ChipName, ChipSlice, ChipCatalog, ChipIdentity,
            };
            /// The board's profile for each chip identity; no runtime
            /// initialization or heap.
            pub static CHIPS: [FlashChip; #count] = [#(#chips),*];
            /// Identify the connected chip among the board's profiles.
            pub static CATALOG: ChipCatalog<'static> = ChipCatalog::new(&CHIPS);
          }
          #[allow(unused_imports)]
          pub use generated_catalog::{CHIPS, CATALOG};
        };
        Ok(prettyplease::unparse(
            &syn::parse2(tokens).expect("valid catalog code"),
        ))
    }

    /// Write the selected firmware catalog to a build output file.
    pub fn write(&self, output_file: &Path) -> Result<(), Error> {
        std::fs::write(output_file, self.generate_code()?)?;
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
                (name: "Part(A)", ids: [Rdid(id: 0x010001)], total_size: KiB(8), erase_blocks: [
                    (opcode: 0x20, regions: [(size: KiB(4), count: 2)]),
                    (opcode: 0xc7, regions: [(size: KiB(8), count: 1)]),
                ]),
                (name: "Other", ids: [Rdid(id: 0x010001)], total_size: KiB(8), erase_blocks: [
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
            .generate_code()
            .unwrap();
        assert!(code.contains("Part(A)"));
        assert!(!code.contains("Other"));
        assert!(!code.contains("vec!"));
        assert!(!code.contains("to_string"));
        assert!(!code.contains("DEFAULT_CHIPS"));
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
            .generate_code()
            .unwrap();
        assert!(code.contains("use rflasher_core::chip::"));
        assert!(!code.contains("rflasher_chip_types"));
        assert!(code.contains("pub static CATALOG:"));
    }

    #[test]
    fn one_profile_per_identity_is_decided_at_build_time() {
        let db = database();
        assert!(db.select(&[]).unwrap().profiles().unwrap().is_empty());
        let both = || db.select(&["Part", "Other"]).unwrap();
        // The two share an identity and erase differently.
        let error = both().generate_code().unwrap_err().to_string();
        assert!(error.contains("Test::Part(A), Test::Other"), "{error}");
        let error = both()
            .with_defaults(&["Other", "Part"])
            .unwrap()
            .generate_code()
            .unwrap_err()
            .to_string();
        assert!(error.contains("Conflicting default"), "{error}");
        // The default is the identity's profile; the other is not shipped.
        let code = both()
            .with_defaults(&["Other"])
            .unwrap()
            .generate_code()
            .unwrap();
        assert!(code.contains("Other") && code.contains("Some(220)"));
        assert!(!code.contains("Part(A)"));
        assert!(code.contains("static CHIPS: [FlashChip; 1"));
        syn::parse_file(&code).unwrap();
    }

    #[test]
    fn aliases_collapse_and_other_identities_do_not_compete() {
        let mut db = database();
        let mut alias = db.vendors[0].chips[0].clone();
        alias.name = "Alias".into();
        db.vendors[0].chips.push(alias);
        let code = db
            .select(&["Alias", "Part"])
            .unwrap()
            .generate_code()
            .unwrap();
        assert!(code.contains("Alias") && !code.contains("Part(A)"));
        // Another command namespace is a different identity.
        db.vendors[0].chips[1].ids = vec![rflasher_chip_types::ChipIdentity::Res2 { id: 0x0101 }];
        let selected = db.select(&["Part", "Other"]).unwrap();
        assert_eq!(selected.profiles().unwrap().len(), 2);
        // Missing explicit identities fail validation, never synthesize RDID.
        let ids = std::mem::take(&mut db.vendors[0].chips[0].ids);
        assert!(db.select(&["Part", "Other"]).is_err());
        db.vendors[0].chips[0].ids = ids;
        db.vendors[0].chips[1].ids = vec![rflasher_chip_types::ChipIdentity::Rdid { id: 0x010002 }];
        let selected = db.select(&["Other", "Part"]).unwrap();
        let profiles = selected.profiles().unwrap();
        assert_eq!(profiles[0].1.name, "Other");
        assert_eq!(profiles[1].1.name, "Part(A)");
    }

    #[test]
    fn defaults_resolve_overlapping_alternative_ids_without_losing_other_ids() {
        use rflasher_chip_types::ChipIdentity;
        let mut db = database();
        let rdid = ChipIdentity::Rdid { id: 0x010001 };
        let rems = ChipIdentity::Rems { id: 0x0101 };
        db.vendors[0].chips[0].ids = vec![rdid, rems];
        db.vendors[0].chips[1].ids = vec![rems];
        assert!(db.select(&["Part", "Other"]).unwrap().profiles().is_err());
        let selected = db
            .select(&["Part", "Other"])
            .unwrap()
            .with_defaults(&["Other"])
            .unwrap();
        let profiles = selected.profiles().unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].1.ids, [rdid]);
        assert_eq!(profiles[1].1.ids, [rems]);
        let code = selected
            .with_crate_path("rflasher_core::chip")
            .unwrap()
            .generate_code()
            .unwrap();
        assert!(!code.contains("rflasher_chip_types"));
        syn::parse_file(&code).unwrap();
    }

    #[test]
    fn explicit_profiles_are_approved_as_defaults_or_fail_the_build() {
        let mut db = database();
        db.vendors[0].chips[1].features.explicit_profile = true;
        for selection in [&["Other"][..], &["Part", "Other"]] {
            let error = db
                .select(selection)
                .unwrap()
                .generate_code()
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("Test::Other is an explicit profile"),
                "{error}"
            );
        }
        let code = db
            .select(&["Part", "Other"])
            .unwrap()
            .with_defaults(&["Other"])
            .unwrap()
            .generate_code()
            .unwrap();
        assert!(code.contains("EXPLICIT_PROFILE") && !code.contains("Part(A)"));
    }
}

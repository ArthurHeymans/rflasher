//! Build script for rflasher-core
//!
//! This script generates the chip database from RON files at build time.

use std::env;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    // Chips directory is at the workspace root
    let chips_dir = manifest_dir.parent().unwrap().parent().unwrap().join("chips").join("vendors");
    let output_file = out_dir.join("chips_generated.rs");

    // Re-run if any RON file changes
    println!("cargo::rerun-if-changed={}", chips_dir.display());
    for entry in std::fs::read_dir(&chips_dir).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().map_or(false, |ext| ext == "ron") {
            println!("cargo::rerun-if-changed={}", entry.path().display());
        }
    }

    // Generate the chip database
    rflasher_chips_codegen::generate(&chips_dir, &output_file)
        .expect("Failed to generate chip database");

    println!("cargo::warning=Generated chip database at {}", output_file.display());
}

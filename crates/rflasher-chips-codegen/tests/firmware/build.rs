use std::{env, path::PathBuf};

fn main() {
    let data = rflasher_chips::bundled_data_dir();
    println!("cargo::rerun-if-changed={}", data.display());
    let db = rflasher_chips_codegen::ChipDatabase::load_from_dir(data).unwrap();
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    db.select(&[
        "W25Q256JV_Q",
        "A25L10PT",
        "W25Q128BV-WP",
        "W25Q128FVJV-WP",
        "SST25VF016B",
        "W25Q128.V",
    ])
    .unwrap()
    .with_defaults(&["W25Q128FVJV-WP", "W25Q256JV_Q"])
    .unwrap()
    .with_crate_path("rflasher_core::chip")
    .unwrap()
    .write(&out.join("chips.rs"))
    .unwrap();
    db.select(&[])
        .unwrap()
        .with_crate_path("rflasher_core::chip")
        .unwrap()
        .write(&out.join("empty.rs"))
        .unwrap();
}

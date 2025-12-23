fn main() {
    // Man page generation is handled by a separate utility
    // Run: cargo run --bin gen-manpage
    println!("cargo:rerun-if-changed=src/cli.rs");
}

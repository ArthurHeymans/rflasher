//! Man page generator for rflasher
//!
//! Usage: cargo run --bin gen-manpage -- [output-dir]

use clap::CommandFactory;
use std::fs;
use std::path::PathBuf;

#[path = "../cli.rs"]
mod cli;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    // Default to ./man directory
    let output_dir = if args.len() > 1 {
        PathBuf::from(&args[1])
    } else {
        PathBuf::from("man")
    };

    fs::create_dir_all(&output_dir)?;

    let cmd = cli::Cli::command();
    let man = clap_mangen::Man::new(cmd);
    let mut buffer = Vec::new();
    man.render(&mut buffer)?;

    let output_path = output_dir.join("rflasher.1");
    fs::write(&output_path, buffer)?;

    println!("Man page generated at: {}", output_path.display());
    println!("\nTo view the man page:");
    println!("  man -l {}", output_path.display());
    println!("\nTo install system-wide (requires sudo):");
    println!(
        "  sudo cp {} /usr/local/share/man/man1/",
        output_path.display()
    );
    println!("  sudo mandb");

    Ok(())
}

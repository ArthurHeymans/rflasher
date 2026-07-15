//! Shell completion generator for rflasher
//!
//! Usage: cargo run --bin gen-completions -- [output-dir]

use clap::CommandFactory;
use clap_complete::{Shell, generate_to};
use std::fs;
use std::path::PathBuf;

#[path = "../cli.rs"]
mod cli;

fn main() -> std::io::Result<()> {
    let output_dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("completions"));

    fs::create_dir_all(&output_dir)?;

    const BIN_NAME: &str = "rflasher";
    let mut command = cli::Cli::command();

    for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
        let path = generate_to(shell, &mut command, BIN_NAME, &output_dir)?;
        println!("Generated: {}", path.display());
    }

    Ok(())
}

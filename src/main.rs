//! rflasher - A modern flash chip programmer
//!
//! A Rust port of flashprog for reading, writing, and erasing flash chips.

mod cli;
mod commands;

use clap::Parser;
use cli::{Cli, Commands, LayoutCommands};
use rflasher_core::chip::ChipDatabase;
use rflasher_core::flash;
use rflasher_dummy::DummyFlash;
use std::path::{Path, PathBuf};

fn main() {
    // Initialize logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    // Set log level based on verbosity
    match cli.verbose {
        0 => {} // default (info)
        1 => log::set_max_level(log::LevelFilter::Debug),
        _ => log::set_max_level(log::LevelFilter::Trace),
    }

    // Load chip database
    let db = match load_chip_database(cli.chip_db.as_deref()) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Failed to load chip database: {}", e);
            std::process::exit(1);
        }
    };

    log::info!("Loaded {} chip definitions", db.len());

    let result = match cli.command {
        Commands::Probe { programmer } => cmd_probe(&programmer, &db),
        Commands::Read {
            programmer,
            output,
            chip: _,
            layout: _,
        } => cmd_read(&programmer, &output, &db),
        Commands::Write {
            programmer,
            input,
            chip: _,
            verify,
            no_erase,
            layout: _,
        } => cmd_write(&programmer, &input, verify, no_erase, &db),
        Commands::Erase {
            programmer,
            chip: _,
            start,
            length,
            layout: _,
        } => cmd_erase(&programmer, start, length, &db),
        Commands::Verify {
            programmer,
            input,
            chip: _,
            layout: _,
        } => cmd_verify(&programmer, &input, &db),
        Commands::Info {
            programmer,
            chip: _,
        } => cmd_info(&programmer, &db),
        Commands::ListProgrammers => {
            commands::list_programmers();
            Ok(())
        }
        Commands::ListChips { vendor } => {
            commands::list_chips(&db, vendor.as_deref());
            Ok(())
        }
        Commands::Layout(subcmd) => match subcmd {
            LayoutCommands::Show { file } => commands::layout::cmd_show(&file),
            LayoutCommands::Extract { input, output } => {
                commands::layout::cmd_extract(&input, &output)
            }
            LayoutCommands::Ifd { input, output } => {
                commands::layout::cmd_ifd(&input, output.as_deref())
            }
            LayoutCommands::Fmap { input, output } => {
                commands::layout::cmd_fmap(&input, output.as_deref())
            }
            LayoutCommands::Create { output, size } => {
                commands::layout::cmd_create(&output, &size)
            }
        },
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

/// Load the chip database from the specified path or default locations
fn load_chip_database(path: Option<&Path>) -> Result<ChipDatabase, Box<dyn std::error::Error>> {
    let mut db = ChipDatabase::new();

    if let Some(path) = path {
        // User specified a path
        if path.is_dir() {
            db.load_dir(path)?;
        } else if path.is_file() {
            db.load_file(path)?;
        } else {
            return Err(format!("Chip database path not found: {}", path.display()).into());
        }
    } else {
        // Try default locations
        let default_paths = [
            PathBuf::from("chips/vendors"),
            PathBuf::from("/usr/share/rflasher/chips"),
            PathBuf::from("/usr/local/share/rflasher/chips"),
        ];

        let mut loaded = false;
        for dir in &default_paths {
            if dir.is_dir() {
                match db.load_dir(dir) {
                    Ok(count) => {
                        log::debug!("Loaded {} chips from {}", count, dir.display());
                        loaded = true;
                    }
                    Err(e) => {
                        log::warn!("Failed to load chips from {}: {}", dir.display(), e);
                    }
                }
            }
        }

        if !loaded {
            log::warn!("No chip database found in default locations");
        }
    }

    Ok(db)
}

fn cmd_probe(programmer: &str, db: &ChipDatabase) -> Result<(), Box<dyn std::error::Error>> {
    with_programmer(programmer, |master| commands::run_probe(master, db))
}

fn cmd_read(
    programmer: &str,
    output: &Path,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    with_programmer(programmer, |master| commands::run_read(master, db, output))
}

fn cmd_write(
    programmer: &str,
    input: &Path,
    verify: bool,
    no_erase: bool,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    with_programmer(programmer, |master| {
        commands::run_write(master, db, input, verify, no_erase)
    })
}

fn cmd_erase(
    programmer: &str,
    start: Option<u32>,
    length: Option<u32>,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    with_programmer(programmer, |master| {
        commands::run_erase(master, db, start, length)
    })
}

fn cmd_verify(
    programmer: &str,
    input: &Path,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    with_programmer(programmer, |master| {
        commands::run_verify(master, db, input)
    })
}

fn cmd_info(programmer: &str, db: &ChipDatabase) -> Result<(), Box<dyn std::error::Error>> {
    with_programmer(programmer, |master| {
        let ctx = flash::probe(master, db)?;
        print_chip_info(&ctx);
        Ok(())
    })
}

/// Execute a function with the specified programmer
fn with_programmer<F>(programmer: &str, f: F) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnOnce(&mut DummyFlash) -> Result<(), Box<dyn std::error::Error>>,
{
    match programmer {
        "dummy" => {
            let mut master = DummyFlash::new_default();
            f(&mut master)
        }
        _ => {
            eprintln!("Unknown programmer: {}", programmer);
            eprintln!("Use 'rflasher list-programmers' to see available programmers");
            Err("Unknown programmer".into())
        }
    }
}

fn print_chip_info(ctx: &flash::FlashContext) {
    let chip = &ctx.chip;

    println!("Flash Chip Information");
    println!("======================");
    println!();
    println!("Vendor:          {}", chip.vendor);
    println!("Name:            {}", chip.name);
    println!(
        "JEDEC ID:        {:02X} {:04X}",
        chip.jedec_manufacturer, chip.jedec_device
    );
    println!(
        "Size:            {} bytes ({} KiB / {} MiB)",
        chip.total_size,
        chip.total_size / 1024,
        chip.total_size / (1024 * 1024)
    );
    println!("Page size:       {} bytes", chip.page_size);
    println!();
    println!(
        "Voltage range:   {:.1}V - {:.1}V",
        chip.voltage_min_mv as f32 / 1000.0,
        chip.voltage_max_mv as f32 / 1000.0
    );
    println!();
    println!("Erase blocks:");
    for eb in chip.erase_blocks() {
        let size_str = if eb.size >= 1024 * 1024 {
            format!("{} MiB", eb.size / (1024 * 1024))
        } else if eb.size >= 1024 {
            format!("{} KiB", eb.size / 1024)
        } else {
            format!("{} bytes", eb.size)
        };
        println!("  Opcode 0x{:02X}: {}", eb.opcode, size_str);
    }
    println!();
    println!("Features:        {:?}", chip.features);
}

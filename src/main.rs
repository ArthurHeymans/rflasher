//! rflasher - A modern flash chip programmer
//!
//! A Rust port of flashprog for reading, writing, and erasing flash chips.

mod cli;
mod commands;

use clap::Parser;
use cli::{Cli, Commands};
use rflasher_core::flash;
use rflasher_dummy::DummyFlash;
use std::path::PathBuf;

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

    let result = match cli.command {
        Commands::Probe { programmer } => cmd_probe(&programmer),
        Commands::Read {
            programmer,
            output,
            chip: _,
        } => cmd_read(&programmer, &output),
        Commands::Write {
            programmer,
            input,
            chip: _,
            verify: _,
            no_erase: _,
        } => cmd_write(&programmer, &input),
        Commands::Erase {
            programmer,
            chip: _,
        } => cmd_erase(&programmer),
        Commands::Verify {
            programmer,
            input,
            chip: _,
        } => cmd_verify(&programmer, &input),
        Commands::Info {
            programmer,
            chip: _,
        } => cmd_info(&programmer),
        Commands::ListProgrammers => {
            commands::list_programmers();
            Ok(())
        }
        Commands::ListChips { vendor } => {
            commands::list_chips(vendor.as_deref());
            Ok(())
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn cmd_probe(programmer: &str) -> Result<(), Box<dyn std::error::Error>> {
    match programmer {
        "dummy" => {
            let mut master = DummyFlash::new_default();
            commands::run_probe(&mut master)
        }
        _ => {
            eprintln!("Unknown programmer: {}", programmer);
            eprintln!("Use 'rflasher list-programmers' to see available programmers");
            Err("Unknown programmer".into())
        }
    }
}

fn cmd_read(programmer: &str, output: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    match programmer {
        "dummy" => {
            let mut master = DummyFlash::new_default();
            let ctx = flash::probe(&mut master)?;
            println!("Would read {} bytes to {:?}", ctx.chip.total_size, output);
            Ok(())
        }
        _ => {
            eprintln!("Unknown programmer: {}", programmer);
            Err("Unknown programmer".into())
        }
    }
}

fn cmd_write(programmer: &str, input: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    match programmer {
        "dummy" => {
            let mut master = DummyFlash::new_default();
            let ctx = flash::probe(&mut master)?;
            println!(
                "Would write {:?} to {} byte chip",
                input, ctx.chip.total_size
            );
            Ok(())
        }
        _ => {
            eprintln!("Unknown programmer: {}", programmer);
            Err("Unknown programmer".into())
        }
    }
}

fn cmd_erase(programmer: &str) -> Result<(), Box<dyn std::error::Error>> {
    match programmer {
        "dummy" => {
            let mut master = DummyFlash::new_default();
            let ctx = flash::probe(&mut master)?;
            println!("Would erase {} byte chip", ctx.chip.total_size);
            Ok(())
        }
        _ => {
            eprintln!("Unknown programmer: {}", programmer);
            Err("Unknown programmer".into())
        }
    }
}

fn cmd_verify(programmer: &str, input: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    match programmer {
        "dummy" => {
            let mut master = DummyFlash::new_default();
            let ctx = flash::probe(&mut master)?;
            println!(
                "Would verify {:?} against {} byte chip",
                input, ctx.chip.total_size
            );
            Ok(())
        }
        _ => {
            eprintln!("Unknown programmer: {}", programmer);
            Err("Unknown programmer".into())
        }
    }
}

fn cmd_info(programmer: &str) -> Result<(), Box<dyn std::error::Error>> {
    match programmer {
        "dummy" => {
            let mut master = DummyFlash::new_default();
            let ctx = flash::probe(&mut master)?;
            print_chip_info(&ctx);
            Ok(())
        }
        _ => {
            eprintln!("Unknown programmer: {}", programmer);
            Err("Unknown programmer".into())
        }
    }
}

fn print_chip_info(ctx: &flash::FlashContext) {
    let chip = ctx.chip;

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
    for eb in chip.erase_blocks {
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

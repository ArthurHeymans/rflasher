//! rflasher - A modern flash chip programmer
//!
//! A Rust port of flashprog for reading, writing, and erasing flash chips.
//!
//! # Architecture
//!
//! rflasher uses a unified `FlashDevice` abstraction that works with both:
//! - **SPI-based programmers** (CH341A, FTDI, serprog, linux_spi) - Provide raw
//!   SPI command access, chip identified via JEDEC probing
//! - **Opaque programmers** (Intel internal) - Hardware handles SPI protocol,
//!   we only have address-based read/write/erase
//!
//! This allows the same command implementations (read, write, erase, verify)
//! to work regardless of the underlying programmer type.

mod cli;
mod commands;
mod programmers;

use clap::Parser;
use cli::{Cli, Commands, LayoutArgs, LayoutCommands};
use rflasher_core::chip::ChipDatabase;
use rflasher_core::flash::{self, FlashDevice, OpaqueFlashDevice, SpiFlashDevice};
use rflasher_core::layout::{parse_ifd, Layout};
use rflasher_core::programmer::OpaqueMaster;
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
            layout,
        } => cmd_read(&programmer, &output, layout, &db),
        Commands::Write {
            programmer,
            input,
            chip: _,
            verify,
            no_erase: _,
            layout,
        } => cmd_write(&programmer, &input, verify, layout, &db),
        Commands::Erase {
            programmer,
            chip: _,
            start,
            length,
            layout,
        } => cmd_erase(&programmer, start, length, layout, &db),
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
            LayoutCommands::Create { output, size } => commands::layout::cmd_create(&output, &size),
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

// =============================================================================
// Helper functions for creating FlashDevice from programmers
// =============================================================================

/// Get flash size from an opaque programmer by reading IFD
fn get_flash_size_from_ifd(
    master: &mut dyn OpaqueMaster,
) -> Result<u32, Box<dyn std::error::Error>> {
    let mut header = [0u8; 4096];
    master.read(0, &mut header)?;

    if let Ok(layout) = parse_ifd(&header) {
        let size = layout.regions.iter().map(|r| r.end + 1).max().unwrap_or(0);
        if size > 0 {
            return Ok(size);
        }
    }

    let size = master.size();
    if size > 0 {
        return Ok(size as u32);
    }

    Err("Cannot determine flash size".into())
}

/// Load layout from IFD on an opaque device
fn load_layout_from_opaque(
    device: &mut dyn FlashDevice,
) -> Result<Layout, Box<dyn std::error::Error>> {
    let mut header = [0u8; 4096];
    device.read(0, &mut header)?;
    let layout = parse_ifd(&header)?;
    Ok(layout)
}

// =============================================================================
// Command implementations using unified FlashDevice interface
// =============================================================================

fn cmd_probe(programmer: &str, db: &ChipDatabase) -> Result<(), Box<dyn std::error::Error>> {
    programmers::with_programmer(programmer, |prog| {
        match prog {
            programmers::Programmer::Spi(master) => {
                // SPI probe - use JEDEC ID
                commands::run_probe(master, db)
            }
            programmers::Programmer::Opaque(master) => {
                // Opaque probe - show IFD info
                commands::run_probe_opaque(master)
            }
        }
    })
}

fn cmd_read(
    programmer: &str,
    output: &Path,
    layout_args: LayoutArgs,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    programmers::with_programmer(programmer, |prog| {
        match prog {
            programmers::Programmer::Spi(master) => {
                // Probe chip to get context
                let ctx = flash::probe(master, db)?;
                println!(
                    "Found: {} {} ({} bytes)",
                    ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
                );

                // Create FlashDevice wrapper
                let mut device = SpiFlashDevice::new(master, ctx);

                if layout_args.has_layout_source() || layout_args.has_region_filter() {
                    // Layout-based read
                    let mut layout = load_layout_from_device(&mut device, &layout_args)?;
                    apply_region_filters(&mut layout, &layout_args)?;
                    commands::unified::run_read_with_layout(&mut device, output, &layout)
                } else {
                    commands::unified::run_read(&mut device, output)
                }
            }
            programmers::Programmer::Opaque(master) => {
                // Get flash size from IFD
                let flash_size = get_flash_size_from_ifd(master)?;
                let mut device = OpaqueFlashDevice::with_size(master, flash_size);

                if layout_args.has_layout_source() || layout_args.has_region_filter() {
                    let mut layout = load_layout_from_opaque(&mut device)?;
                    apply_region_filters(&mut layout, &layout_args)?;
                    commands::unified::run_read_with_layout(&mut device, output, &layout)
                } else {
                    commands::unified::run_read(&mut device, output)
                }
            }
        }
    })
}

fn cmd_write(
    programmer: &str,
    input: &Path,
    verify: bool,
    layout_args: LayoutArgs,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    programmers::with_programmer(programmer, |prog| match prog {
        programmers::Programmer::Spi(master) => {
            let ctx = flash::probe(master, db)?;
            println!(
                "Found: {} {} ({} bytes)",
                ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
            );

            let mut device = SpiFlashDevice::new(master, ctx);

            if layout_args.has_layout_source() || layout_args.has_region_filter() {
                let mut layout = load_layout_from_device(&mut device, &layout_args)?;
                apply_region_filters(&mut layout, &layout_args)?;
                commands::unified::run_write_with_layout(&mut device, input, &mut layout, verify)
            } else {
                commands::unified::run_write(&mut device, input, verify)
            }
        }
        programmers::Programmer::Opaque(master) => {
            let flash_size = get_flash_size_from_ifd(master)?;
            let mut device = OpaqueFlashDevice::with_size(master, flash_size);

            if layout_args.has_layout_source() || layout_args.has_region_filter() {
                let mut layout = load_layout_from_opaque(&mut device)?;
                apply_region_filters(&mut layout, &layout_args)?;
                commands::unified::run_write_with_layout(&mut device, input, &mut layout, verify)
            } else {
                commands::unified::run_write(&mut device, input, verify)
            }
        }
    })
}

fn cmd_erase(
    programmer: &str,
    start: Option<u32>,
    length: Option<u32>,
    layout_args: LayoutArgs,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    // Layout-based erase can't be combined with start/length
    if (layout_args.has_layout_source() || layout_args.has_region_filter())
        && (start.is_some() || length.is_some())
    {
        return Err(
            "Cannot use --start/--length with layout options. Use --include to select regions."
                .into(),
        );
    }

    programmers::with_programmer(programmer, |prog| match prog {
        programmers::Programmer::Spi(master) => {
            let ctx = flash::probe(master, db)?;
            println!(
                "Found: {} {} ({} bytes)",
                ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
            );

            let mut device = SpiFlashDevice::new(master, ctx);

            if layout_args.has_layout_source() || layout_args.has_region_filter() {
                let mut layout = load_layout_from_device(&mut device, &layout_args)?;
                apply_region_filters(&mut layout, &layout_args)?;
                commands::unified::run_erase_with_layout(&mut device, &layout)
            } else {
                commands::unified::run_erase(&mut device, start, length)
            }
        }
        programmers::Programmer::Opaque(master) => {
            let flash_size = get_flash_size_from_ifd(master)?;
            let mut device = OpaqueFlashDevice::with_size(master, flash_size);

            if layout_args.has_layout_source() || layout_args.has_region_filter() {
                let mut layout = load_layout_from_opaque(&mut device)?;
                apply_region_filters(&mut layout, &layout_args)?;
                commands::unified::run_erase_with_layout(&mut device, &layout)
            } else {
                commands::unified::run_erase(&mut device, start, length)
            }
        }
    })
}

fn cmd_verify(
    programmer: &str,
    input: &Path,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    programmers::with_programmer(programmer, |prog| match prog {
        programmers::Programmer::Spi(master) => {
            let ctx = flash::probe(master, db)?;
            println!(
                "Found: {} {} ({} bytes)",
                ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
            );

            let mut device = SpiFlashDevice::new(master, ctx);
            commands::unified::run_verify(&mut device, input)
        }
        programmers::Programmer::Opaque(master) => {
            let flash_size = get_flash_size_from_ifd(master)?;
            let mut device = OpaqueFlashDevice::with_size(master, flash_size);
            commands::unified::run_verify(&mut device, input)
        }
    })
}

fn cmd_info(programmer: &str, db: &ChipDatabase) -> Result<(), Box<dyn std::error::Error>> {
    programmers::with_programmer(programmer, |prog| {
        match prog {
            programmers::Programmer::Spi(master) => {
                let ctx = flash::probe(master, db)?;
                print_chip_info(&ctx);
                Ok(())
            }
            programmers::Programmer::Opaque(master) => {
                // Try to get info from IFD
                let flash_size = get_flash_size_from_ifd(master)?;

                println!("Flash Information (Opaque Programmer)");
                println!("=====================================");
                println!();
                println!(
                    "Size: {} bytes ({} MiB)",
                    flash_size,
                    flash_size / (1024 * 1024)
                );
                println!();

                // Try to show IFD regions
                let mut header = [0u8; 4096];
                master.read(0, &mut header)?;
                if let Ok(layout) = parse_ifd(&header) {
                    println!("Intel Flash Descriptor regions:");
                    for region in &layout.regions {
                        println!(
                            "  {:12} 0x{:08X} - 0x{:08X} ({} KiB)",
                            region.name,
                            region.start,
                            region.end,
                            (region.end - region.start + 1) / 1024
                        );
                    }
                } else {
                    println!("Note: No Intel Flash Descriptor found.");
                }
                Ok(())
            }
        }
    })
}

// =============================================================================
// Layout loading helpers
// =============================================================================

/// Load layout from the appropriate source for a FlashDevice
fn load_layout_from_device<D: FlashDevice>(
    device: &mut D,
    args: &LayoutArgs,
) -> Result<Layout, Box<dyn std::error::Error>> {
    if let Some(path) = &args.layout {
        // Load from TOML file
        let layout = Layout::from_toml_file(path)?;
        println!("Loaded layout from {:?}", path);
        Ok(layout)
    } else if args.ifd || args.fmap {
        // Read from flash (IFD or FMAP)
        let mut header = [0u8; 4096];
        device.read(0, &mut header)?;

        if args.ifd {
            println!("Reading Intel Flash Descriptor from chip...");
            let layout = parse_ifd(&header)?;
            println!("Found IFD with {} regions", layout.len());
            commands::layout::print_layout(&layout);
            Ok(layout)
        } else {
            // FMAP - need to search for it
            println!("Searching for FMAP in chip...");
            // For simplicity, just try to find FMAP at common locations
            // In a real implementation, we'd search the entire flash
            Err("FMAP search not yet implemented for unified interface".into())
        }
    } else if args.has_region_filter() {
        Err("Layout source required (--layout, --ifd, or --fmap) when using --include, --exclude, or --region".into())
    } else {
        Err("No layout source specified".into())
    }
}

/// Apply region filters (--include, --exclude, --region) to a layout
fn apply_region_filters(
    layout: &mut Layout,
    args: &LayoutArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // Handle --region shorthand (equivalent to --include with one region)
    if let Some(region_name) = &args.region {
        layout.include_region(region_name)?;
    }

    // Handle --include
    for name in &args.include {
        layout.include_region(name)?;
    }

    // Handle --exclude (only applies if some regions are already included)
    for name in &args.exclude {
        layout.exclude_region(name)?;
    }

    // If no regions specified, include all
    if !layout.has_included_regions() {
        layout.include_all();
    }

    Ok(())
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

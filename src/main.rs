//! rflasher - A modern flash chip programmer
//!
//! A Rust port of flashprog for reading, writing, and erasing flash chips.

mod cli;
mod commands;
mod programmers;

use clap::Parser;
use cli::{Cli, Commands, LayoutArgs, LayoutCommands};
use programmers::Programmer;
use rflasher_core::chip::ChipDatabase;
use rflasher_core::flash::{self, FlashContext};
use rflasher_core::layout::{read_fmap_from_flash, read_ifd_from_flash, Layout};
use rflasher_core::programmer::SpiMaster;
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
            no_erase,
            layout,
        } => cmd_write(&programmer, &input, verify, no_erase, layout, &db),
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

fn cmd_probe(programmer: &str, db: &ChipDatabase) -> Result<(), Box<dyn std::error::Error>> {
    programmers::with_programmer(programmer, |prog| match prog {
        Programmer::Spi(master) => commands::run_probe(master, db),
        Programmer::Opaque(master) => commands::run_probe_opaque(master),
    })
}

fn cmd_read(
    programmer: &str,
    output: &Path,
    layout_args: LayoutArgs,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    programmers::with_programmer(programmer, |prog| match prog {
        Programmer::Spi(master) => {
            // Check if we need layout-based read
            if layout_args.has_layout_source() || layout_args.has_region_filter() {
                // Probe the chip first
                let ctx = flash::probe(master, db)?;

                println!(
                    "Found: {} {} ({} bytes)",
                    ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
                );

                // Load layout from the appropriate source
                let mut layout = load_layout_from_args(&layout_args, master, &ctx)?;

                // Apply region filters
                apply_region_filters(&mut layout, &layout_args)?;

                // Now run the read with layout
                commands::run_read_with_layout(master, &ctx, output, &layout)
            } else {
                // No layout - use standard read
                commands::run_read(master, db, output)
            }
        }
        Programmer::Opaque(master) => {
            // Opaque programmer (e.g., Intel internal) - use IFD-based read
            commands::run_read_opaque(master, output, &layout_args)
        }
    })
}

fn cmd_write(
    programmer: &str,
    input: &Path,
    verify: bool,
    no_erase: bool,
    layout_args: LayoutArgs,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    programmers::with_programmer(programmer, |prog| match prog {
        Programmer::Spi(master) => {
            // Check if we need layout-based write
            if layout_args.has_layout_source() || layout_args.has_region_filter() {
                // Probe the chip first
                let ctx = flash::probe(master, db)?;

                println!(
                    "Found: {} {} ({} bytes)",
                    ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
                );

                // Load layout from the appropriate source
                let mut layout = load_layout_from_args(&layout_args, master, &ctx)?;

                // Apply region filters
                apply_region_filters(&mut layout, &layout_args)?;

                // Now run the write with layout
                commands::run_write_with_layout(master, db, input, &mut layout, verify)
            } else {
                // No layout - use standard write
                commands::run_write(master, db, input, verify, no_erase)
            }
        }
        Programmer::Opaque(master) => {
            // Opaque programmer (e.g., Intel internal)
            commands::run_write_opaque(master, input, verify, &layout_args)
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
    // Layout-based erase - can't combine with start/length
    if (layout_args.has_layout_source() || layout_args.has_region_filter())
        && (start.is_some() || length.is_some())
    {
        return Err(
            "Cannot use --start/--length with layout options. Use --include to select regions."
                .into(),
        );
    }

    programmers::with_programmer(programmer, |prog| match prog {
        Programmer::Spi(master) => {
            // Check if we need layout-based erase
            if layout_args.has_layout_source() || layout_args.has_region_filter() {
                // Probe the chip first
                let ctx = flash::probe(master, db)?;

                println!(
                    "Found: {} {} ({} bytes)",
                    ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
                );

                // Load layout from the appropriate source
                let mut layout = load_layout_from_args(&layout_args, master, &ctx)?;

                // Apply region filters
                apply_region_filters(&mut layout, &layout_args)?;

                // Now run the erase with layout
                commands::run_erase_with_layout(master, db, &layout)
            } else {
                // No layout - use standard erase
                commands::run_erase(master, db, start, length)
            }
        }
        Programmer::Opaque(master) => {
            // Opaque programmer (e.g., Intel internal)
            commands::run_erase_opaque(master, start, length, &layout_args)
        }
    })
}

fn cmd_verify(
    programmer: &str,
    input: &Path,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    programmers::with_programmer(programmer, |prog| match prog {
        Programmer::Spi(master) => commands::run_verify(master, db, input),
        Programmer::Opaque(master) => commands::run_verify_opaque(master, input),
    })
}

fn cmd_info(programmer: &str, db: &ChipDatabase) -> Result<(), Box<dyn std::error::Error>> {
    programmers::with_programmer(programmer, |prog| match prog {
        Programmer::Spi(master) => {
            let ctx = flash::probe(master, db)?;
            print_chip_info(&ctx);
            Ok(())
        }
        Programmer::Opaque(master) => {
            // For opaque programmers, we can't probe JEDEC ID
            // Instead, show what information we have
            println!("Flash Information (Opaque Programmer)");
            println!("=====================================");
            println!();
            println!("Size: {} bytes", master.size());
            println!();
            println!("Note: Opaque programmers don't support JEDEC ID probing.");
            println!("Use --ifd to read flash layout from Intel Flash Descriptor.");
            Ok(())
        }
    })
}

/// Load layout from LayoutArgs (file, IFD from chip, or FMAP from chip)
fn load_layout_from_args<M: SpiMaster + ?Sized>(
    args: &LayoutArgs,
    master: &mut M,
    ctx: &FlashContext,
) -> Result<Layout, Box<dyn std::error::Error>> {
    if let Some(path) = &args.layout {
        // Load from TOML file
        let layout = Layout::from_toml_file(path)?;
        println!("Loaded layout from {:?}", path);
        Ok(layout)
    } else if args.ifd {
        // Read IFD from flash chip
        println!("Reading Intel Flash Descriptor from chip...");
        let layout = read_ifd_from_flash(master, ctx)?;
        println!("Found IFD with {} regions", layout.len());
        commands::layout::print_layout(&layout);
        Ok(layout)
    } else if args.fmap {
        // Read FMAP from flash chip
        println!("Searching for FMAP in chip...");
        let layout = read_fmap_from_flash(master, ctx)?;
        println!("Found FMAP with {} regions", layout.len());
        commands::layout::print_layout(&layout);
        Ok(layout)
    } else if args.has_region_filter() {
        // Region filter specified but no layout source
        Err("Layout source required (--layout, --ifd, or --fmap) when using --include, --exclude, or --region".into())
    } else {
        // No layout specified - this shouldn't happen if called correctly
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

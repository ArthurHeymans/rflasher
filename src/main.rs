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

use clap::Parser;
use cli::{Cli, Commands, LayoutArgs, LayoutCommands, WpCommands};
use rflasher_chips::ChipDatabase;
use rflasher_programmers::{FlashHandle, open_flash};

use rflasher_core::flash::FlashDevice;
use rflasher_core::layout::Layout;
use std::path::{Path, PathBuf};

fn main() {
    // Initialize logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();

    if let Err(e) = futures_lite::future::block_on(run(cli)) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

/// Top-level async entry point.
///
/// The library stack below the CLI is async on every target; the native
/// binary blocks exactly once, in `main`, around this function.
async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Set log level based on verbosity
    match cli.verbose {
        0 => {} // default (info)
        1 => log::set_max_level(log::LevelFilter::Debug),
        _ => log::set_max_level(log::LevelFilter::Trace),
    }

    // Load chip database
    let db = load_chip_database(cli.chip_db.as_deref())
        .map_err(|e| format!("Failed to load chip database: {}", e))?;

    log::info!("Loaded {} chip definitions", db.len());

    let programmer = cli.programmer;

    match cli.command {
        Commands::Probe => {
            // Probe doesn't use the device, just shows info
            let _handle = open_flash(require_programmer(&programmer)?, &db).await?;
            Ok(())
        }
        Commands::Read { file, layout } => {
            let mut handle = open_flash(require_programmer(&programmer)?, &db).await?;
            if layout.has_layout_source() || layout.has_region_filter() {
                let mut layout_obj = load_layout(&mut handle, &layout).await?;
                let region_files = apply_region_filters(&mut layout_obj, &layout)?;
                commands::unified::run_read_with_layout(
                    handle.as_device_mut(),
                    file.as_deref(),
                    &layout_obj,
                    &region_files,
                )
                .await
            } else {
                let file = file.ok_or("Output file required")?;
                commands::unified::run_read(handle.as_device_mut(), &file).await
            }
        }
        Commands::Write {
            file,
            no_verify,
            layout,
        } => {
            let mut handle = open_flash(require_programmer(&programmer)?, &db).await?;
            if layout.has_layout_source() || layout.has_region_filter() {
                let mut layout_obj = load_layout(&mut handle, &layout).await?;
                let region_files = apply_region_filters(&mut layout_obj, &layout)?;
                commands::unified::run_write_with_layout(
                    handle.as_device_mut(),
                    file.as_deref(),
                    &mut layout_obj,
                    &region_files,
                    !no_verify,
                )
                .await
            } else {
                let file = file.ok_or("Input file required")?;
                commands::unified::run_write(handle.as_device_mut(), &file, !no_verify).await
            }
        }
        Commands::Erase { layout } => {
            let mut handle = open_flash(require_programmer(&programmer)?, &db).await?;
            if layout.has_layout_source() || layout.has_region_filter() {
                let mut layout_obj = load_layout(&mut handle, &layout).await?;
                let region_files = apply_region_filters(&mut layout_obj, &layout)?;
                if !region_files.is_empty() {
                    return Err("Per-region files (NAME:FILE) make no sense for erase".into());
                }
                commands::unified::run_erase_with_layout(handle.as_device_mut(), &layout_obj).await
            } else {
                commands::unified::run_erase(handle.as_device_mut()).await
            }
        }
        Commands::Verify { file, layout } => {
            let mut handle = open_flash(require_programmer(&programmer)?, &db).await?;
            if layout.has_layout_source() || layout.has_region_filter() {
                let mut layout_obj = load_layout(&mut handle, &layout).await?;
                let region_files = apply_region_filters(&mut layout_obj, &layout)?;
                commands::unified::run_verify_with_layout(
                    handle.as_device_mut(),
                    file.as_deref(),
                    &layout_obj,
                    &region_files,
                )
                .await
            } else {
                let file = file.ok_or("Input file required")?;
                commands::unified::run_verify(handle.as_device_mut(), &file).await
            }
        }
        Commands::Info => {
            let mut handle = open_flash(require_programmer(&programmer)?, &db).await?;
            print_chip_info(&mut handle).await;
            Ok(())
        }
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
        Commands::Wp(subcmd) => {
            let mut handle = open_flash(require_programmer(&programmer)?, &db).await?;
            match subcmd {
                WpCommands::Status => commands::wp::cmd_status(&mut handle).await,
                WpCommands::List => commands::wp::cmd_list(&mut handle).await,
                WpCommands::Enable => commands::wp::cmd_enable(&mut handle).await,
                WpCommands::Disable => commands::wp::cmd_disable(&mut handle).await,
                WpCommands::Range { range } => commands::wp::cmd_range(&mut handle, &range).await,
                WpCommands::Region {
                    layout,
                    region_name,
                } => {
                    let layout_obj = load_layout(&mut handle, &layout).await?;
                    commands::wp::cmd_region(&mut handle, &layout_obj, &region_name).await
                }
            }
        }
        #[cfg(feature = "repl")]
        Commands::Repl { script } => {
            commands::repl::cmd_repl(require_programmer(&programmer)?, script.as_deref()).await
        }
    }
}

/// Require a programmer to be specified via -p/--programmer or RFLASHER_PROGRAMMER
fn require_programmer(programmer: &Option<String>) -> Result<&str, Box<dyn std::error::Error>> {
    programmer.as_deref().ok_or_else(|| {
        "No programmer specified. Use -p/--programmer or set RFLASHER_PROGRAMMER.".into()
    })
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
            PathBuf::from("crates/rflasher-chips/data/vendors"),
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
// Layout loading helpers
// =============================================================================

/// Load layout from the appropriate source for a FlashHandle
async fn load_layout(
    handle: &mut FlashHandle,
    args: &LayoutArgs,
) -> Result<Layout, Box<dyn std::error::Error>> {
    use rflasher_core::layout::parse_ifd;

    if let Some(path) = &args.layout {
        // Load from TOML file
        let layout = Layout::from_toml_file(path)?;
        log::info!("Loaded layout from {:?}", path);
        Ok(layout)
    } else if args.ifd || args.fmap {
        if args.ifd {
            // IFD is always at the beginning, so we only need to read the header
            log::info!("Reading Intel Flash Descriptor from chip...");
            let mut header = [0u8; 4096];
            handle.as_device_mut().read(0, &mut header).await?;
            let layout = parse_ifd(&header)?;
            log::info!("Found IFD with {} regions", layout.len());
            commands::layout::print_layout(&layout);
            Ok(layout)
        } else {
            // FMAP can be anywhere in the flash, so we need to search for it
            log::info!("Searching for FMAP in chip...");
            let layout = handle.read_fmap().await?;
            log::info!("Found FMAP with {} regions", layout.len());
            commands::layout::print_layout(&layout);
            Ok(layout)
        }
    } else if args.has_region_filter() {
        Err("Layout source required (--layout, --ifd, or --fmap) when using --include, --exclude, or --region".into())
    } else {
        Err("No layout source specified".into())
    }
}

/// Apply region filters (--include, --exclude, --region) to a layout.
///
/// Returns the per-region file assignments collected from NAME:FILE specs.
fn apply_region_filters(
    layout: &mut Layout,
    args: &LayoutArgs,
) -> Result<Vec<(String, std::path::PathBuf)>, Box<dyn std::error::Error>> {
    // --region is shorthand for --include with one region
    let specs = args.region.iter().chain(args.include.iter());

    let region_files: Vec<_> = specs
        .map(|spec| {
            layout.include_region(&spec.name)?;
            Ok(spec
                .file
                .as_ref()
                .map(|file| (spec.name.clone(), file.clone())))
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?
        .into_iter()
        .flatten()
        .collect();

    // Handle --exclude (only applies if some regions are already included)
    for name in &args.exclude {
        layout.exclude_region(name)?;
    }

    // If no regions specified, include all
    if !layout.has_included_regions() {
        layout.include_all();
    }

    commands::unified::validate_region_files(layout, &region_files)?;

    Ok(region_files)
}

async fn print_chip_info(handle: &mut FlashHandle) {
    use rflasher_core::layout::parse_ifd;

    if let Some(info) = handle.chip_info() {
        // SPI device - we have chip information
        println!("Flash Chip Information");
        println!("======================");
        println!();

        // Show source of chip info
        if info.from_database {
            println!("Source:          Database");
        } else {
            println!("Source:          SFDP (chip not in database)");
        }

        println!("Vendor:          {}", info.vendor);
        println!("Name:            {}", info.name);
        println!(
            "JEDEC ID:        {:02X} {:04X}",
            info.jedec_manufacturer, info.jedec_device
        );
        println!(
            "Size:            {} bytes ({})",
            info.total_size,
            commands::format_size(info.total_size)
        );
        println!("Page size:       {} bytes", info.page_size);

        // Show SFDP status
        if info.sfdp.is_some() {
            println!("SFDP:            Supported");
        } else {
            println!("SFDP:            Not detected");
        }

        // Detailed chip information
        {
            let chip = &info.chip;
            println!();
            println!(
                "Voltage range:   {:.1}V - {:.1}V",
                chip.voltage_min_mv as f32 / 1000.0,
                chip.voltage_max_mv as f32 / 1000.0
            );
            println!();
            println!("Erase blocks:");
            for eb in chip.erase_blocks() {
                if eb.is_uniform() {
                    // Uniform erase block - single size
                    let size = eb.uniform_size().unwrap_or(0);
                    println!(
                        "  Opcode 0x{:02X}: {}",
                        eb.opcode,
                        commands::format_size(size)
                    );
                } else {
                    // Non-uniform erase block - show all regions
                    let regions: Vec<String> = eb
                        .regions()
                        .iter()
                        .map(|r| format!("{}x{}", r.count, commands::format_size(r.size)))
                        .collect();
                    println!("  Opcode 0x{:02X}: {}", eb.opcode, regions.join(" + "));
                }
            }
            println!();
            println!("Features:        {:?}", chip.features);
        }

        // Show SFDP mismatches if any
        if !info.mismatches.is_empty() {
            println!();
            println!("SFDP Mismatches:");
            println!("----------------");
            for mismatch in &info.mismatches {
                println!("  {}", mismatch);
            }
        }
    } else {
        // Opaque device - show IFD info
        let flash_size = handle.size();

        println!("Flash Information (Opaque Programmer)");
        println!("=====================================");
        println!();
        println!(
            "Size: {} bytes ({})",
            flash_size,
            commands::format_size(flash_size)
        );
        println!();

        // Try to show IFD regions
        let mut header = [0u8; 4096];
        if handle.as_device_mut().read(0, &mut header).await.is_ok() {
            if let Ok(layout) = parse_ifd(&header) {
                println!("Intel Flash Descriptor regions:");
                for region in &layout.regions {
                    println!(
                        "  {:12} 0x{:08X} - 0x{:08X} ({})",
                        region.name,
                        region.start,
                        region.end,
                        commands::format_size(region.end - region.start + 1)
                    );
                }
            } else {
                println!("Note: No Intel Flash Descriptor found.");
            }
        }
    }
}

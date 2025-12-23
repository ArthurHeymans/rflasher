//! Erase command implementation

use indicatif::{ProgressBar, ProgressStyle};
use rflasher_core::chip::ChipDatabase;
use rflasher_core::flash::{self, FlashContext};
use rflasher_core::layout::Layout;
use rflasher_core::programmer::SpiMaster;
use std::time::Duration;

/// Run the erase command
pub fn run_erase<M: SpiMaster + ?Sized>(
    master: &mut M,
    db: &ChipDatabase,
    start: Option<u32>,
    length: Option<u32>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Probe for chip
    let ctx = flash::probe(master, db)?;

    println!(
        "Found: {} {} ({} bytes)",
        ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
    );

    match (start, length) {
        (Some(start_addr), Some(len)) => {
            // Partial erase
            erase_region_with_progress(master, &ctx, start_addr, len)?;
            println!("Erased {} bytes starting at 0x{:08X}", len, start_addr);
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err("Both --start and --length must be specified for partial erase".into());
        }
        (None, None) => {
            // Full chip erase
            chip_erase_with_progress(master, &ctx)?;
            println!("Chip erase complete");
        }
    }

    Ok(())
}

/// Erase entire chip with progress spinner
pub fn chip_erase_with_progress<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let total_size = ctx.total_size();

    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template("{spinner:.green} {msg}")?);
    pb.set_message(format!(
        "Erasing {} bytes (this may take a while)...",
        total_size
    ));
    pb.enable_steady_tick(Duration::from_millis(100));

    flash::chip_erase(master, ctx)?;

    pb.finish_with_message(format!("Erased {} bytes", total_size));
    Ok(())
}

/// Erase a region with progress bar
pub fn erase_region_with_progress<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    start: u32,
    length: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    // Validate range
    if !ctx.is_valid_range(start, length as usize) {
        return Err(format!(
            "Erase range 0x{:08X}..0x{:08X} is outside chip bounds (0x{:08X})",
            start,
            start + length,
            ctx.chip.total_size
        )
        .into());
    }

    let pb = ProgressBar::new(length as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) Erasing")?
            .progress_chars("#>-"),
    );

    // Use sector-level erase for regions
    // The flash::erase function handles block selection
    flash::erase(master, ctx, start, length)?;

    pb.finish_with_message("Erase complete");
    Ok(())
}

/// Run the erase command with layout support
pub fn run_erase_with_layout<M: SpiMaster + ?Sized>(
    master: &mut M,
    db: &ChipDatabase,
    layout: &Layout,
) -> Result<(), Box<dyn std::error::Error>> {
    // Probe for chip
    let ctx = flash::probe(master, db)?;

    println!(
        "Found: {} {} ({} bytes)",
        ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
    );

    // Validate layout against chip
    layout
        .validate(ctx.chip.total_size)
        .map_err(|e| format!("Layout validation failed: {}", e))?;

    // Collect included regions
    let included: Vec<_> = layout.included_regions().collect();
    if included.is_empty() {
        return Err("No regions selected for erasing. Use --include to select regions.".into());
    }

    // Check for readonly regions
    let readonly = layout.readonly_included();
    if !readonly.is_empty() {
        let names: Vec<_> = readonly.iter().map(|r| r.name.as_str()).collect();
        return Err(format!("Cannot erase readonly region(s): {}", names.join(", ")).into());
    }

    // Display regions to be erased
    let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();
    println!(
        "Erasing {} region(s) ({} bytes total):",
        included.len(),
        total_bytes
    );
    for region in &included {
        println!(
            "  {} (0x{:08X} - 0x{:08X}, {} bytes)",
            region.name,
            region.start,
            region.end,
            region.size()
        );
    }

    // Erase each region
    flash::erase_by_layout(master, &ctx, layout)?;

    println!("Erase complete!");
    Ok(())
}

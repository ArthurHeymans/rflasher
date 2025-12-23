//! Read command implementation

use indicatif::{ProgressBar, ProgressStyle};
use rflasher_core::chip::ChipDatabase;
use rflasher_core::flash::{self, FlashContext};
use rflasher_core::layout::Layout;
use rflasher_core::programmer::SpiMaster;
use std::fs::File;
use std::io::Write;
use std::path::Path;

/// Default chunk size for reading (4 KiB)
const READ_CHUNK_SIZE: usize = 4096;

/// Run the read command
pub fn run_read<M: SpiMaster + ?Sized>(
    master: &mut M,
    db: &ChipDatabase,
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Probe for chip
    let ctx = flash::probe(master, db)?;

    println!(
        "Found: {} {} ({} bytes)",
        ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
    );

    // Read the chip
    let data = read_flash_with_progress(master, &ctx)?;

    // Write to file
    let mut file = File::create(output)?;
    file.write_all(&data)?;

    println!("Wrote {} bytes to {:?}", data.len(), output);

    Ok(())
}

/// Read entire flash contents with progress bar
pub fn read_flash_with_progress<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let total_size = ctx.total_size();
    let mut data = vec![0u8; total_size];

    let pb = ProgressBar::new(total_size as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?
            .progress_chars("#>-"),
    );

    let mut offset = 0usize;
    while offset < total_size {
        let chunk_size = std::cmp::min(READ_CHUNK_SIZE, total_size - offset);
        let chunk = &mut data[offset..offset + chunk_size];

        flash::read(master, ctx, offset as u32, chunk)?;

        offset += chunk_size;
        pb.set_position(offset as u64);
    }

    pb.finish_with_message("Read complete");
    Ok(data)
}

/// Run the read command with layout support
///
/// Reads only the included regions from the layout. Non-included regions
/// in the output file will be filled with 0xFF.
pub fn run_read_with_layout<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    output: &Path,
    layout: &Layout,
) -> Result<(), Box<dyn std::error::Error>> {
    // Display included regions
    let included: Vec<_> = layout.included_regions().collect();
    if included.is_empty() {
        return Err("No regions selected for reading. Use --include to select regions.".into());
    }

    println!("Reading {} region(s):", included.len());
    for region in &included {
        println!(
            "  {} (0x{:08X} - 0x{:08X}, {} bytes)",
            region.name,
            region.start,
            region.end,
            region.size()
        );
    }

    // Calculate total bytes to read
    let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();

    // Allocate buffer for full chip (fill with 0xFF for non-included regions)
    let total_size = ctx.total_size();
    let mut data = vec![0xFFu8; total_size];

    // Create progress bar
    let pb = ProgressBar::new(total_bytes as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?
            .progress_chars("#>-"),
    );

    let mut bytes_read = 0usize;

    // Read each included region
    for region in included {
        let mut offset = region.start;
        while offset <= region.end {
            let remaining = (region.end - offset + 1) as usize;
            let chunk_size = std::cmp::min(READ_CHUNK_SIZE, remaining);
            let chunk = &mut data[offset as usize..offset as usize + chunk_size];

            flash::read(master, ctx, offset, chunk)?;

            offset += chunk_size as u32;
            bytes_read += chunk_size;
            pb.set_position(bytes_read as u64);
        }
    }

    pb.finish_with_message("Read complete");

    // Write to file
    let mut file = File::create(output)?;
    file.write_all(&data)?;

    println!("Wrote {} bytes to {:?}", data.len(), output);
    println!(
        "  ({} bytes from included regions, rest filled with 0xFF)",
        bytes_read
    );

    Ok(())
}

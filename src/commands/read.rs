//! Read command implementation

use indicatif::{ProgressBar, ProgressStyle};
use rflasher_core::chip::ChipDatabase;
use rflasher_core::flash::{self, FlashContext};
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

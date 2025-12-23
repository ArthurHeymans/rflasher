//! Write command implementation

use indicatif::{ProgressBar, ProgressStyle};
use rflasher_core::chip::ChipDatabase;
use rflasher_core::flash::{self, FlashContext};
use rflasher_core::programmer::SpiMaster;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Default chunk size for writing (page size is typically 256 bytes)
const WRITE_CHUNK_SIZE: usize = 4096;
/// Default chunk size for verification
const VERIFY_CHUNK_SIZE: usize = 4096;

/// Run the write command
pub fn run_write<M: SpiMaster + ?Sized>(
    master: &mut M,
    db: &ChipDatabase,
    input: &Path,
    do_verify: bool,
    no_erase: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Probe for chip
    let ctx = flash::probe(master, db)?;

    println!(
        "Found: {} {} ({} bytes)",
        ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
    );

    // Read input file
    let mut file = File::open(input)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;

    println!("Read {} bytes from {:?}", data.len(), input);

    // Validate size
    if data.len() > ctx.total_size() {
        return Err(format!(
            "File size ({} bytes) exceeds chip size ({} bytes)",
            data.len(),
            ctx.total_size()
        )
        .into());
    }

    // Pad to chip size if needed (with 0xFF)
    if data.len() < ctx.total_size() {
        println!(
            "Padding file from {} to {} bytes with 0xFF",
            data.len(),
            ctx.total_size()
        );
        data.resize(ctx.total_size(), 0xFF);
    }

    // Erase if requested
    if !no_erase {
        erase_flash_with_progress(master, &ctx)?;
    }

    // Write
    write_flash_with_progress(master, &ctx, &data)?;

    // Verify if requested
    if do_verify {
        verify_flash_with_progress(master, &ctx, &data)?;
    }

    println!("Write complete!");

    Ok(())
}

/// Erase entire flash with progress bar
pub fn erase_flash_with_progress<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let total_size = ctx.total_size();

    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.green} {msg}")?,
    );
    pb.set_message(format!("Erasing {} bytes...", total_size));
    pb.enable_steady_tick(std::time::Duration::from_millis(100));

    flash::chip_erase(master, ctx)?;

    pb.finish_with_message("Erase complete");
    Ok(())
}

/// Write data to flash with progress bar
pub fn write_flash_with_progress<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    data: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let total_size = data.len();

    let pb = ProgressBar::new(total_size as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta}) Writing")?
            .progress_chars("#>-"),
    );

    let mut offset = 0usize;
    while offset < total_size {
        let chunk_size = std::cmp::min(WRITE_CHUNK_SIZE, total_size - offset);
        let chunk = &data[offset..offset + chunk_size];

        flash::write(master, ctx, offset as u32, chunk)?;

        offset += chunk_size;
        pb.set_position(offset as u64);
    }

    pb.finish_with_message("Write complete");
    Ok(())
}

/// Verify flash contents against expected data with progress bar
pub fn verify_flash_with_progress<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    expected: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let total_size = expected.len();
    let mut buf = vec![0u8; VERIFY_CHUNK_SIZE];

    let pb = ProgressBar::new(total_size as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta}) Verifying")?
            .progress_chars("#>-"),
    );

    let mut offset = 0usize;
    while offset < total_size {
        let chunk_size = std::cmp::min(VERIFY_CHUNK_SIZE, total_size - offset);
        let chunk = &mut buf[..chunk_size];

        flash::read(master, ctx, offset as u32, chunk)?;

        // Compare
        let expected_chunk = &expected[offset..offset + chunk_size];
        if chunk != expected_chunk {
            pb.abandon_with_message("Verification failed!");
            // Find first difference
            for (i, (a, b)) in chunk.iter().zip(expected_chunk.iter()).enumerate() {
                if a != b {
                    return Err(format!(
                        "Verification failed at offset 0x{:08X}: expected 0x{:02X}, got 0x{:02X}",
                        offset + i,
                        b,
                        a
                    )
                    .into());
                }
            }
        }

        offset += chunk_size;
        pb.set_position(offset as u64);
    }

    pb.finish_with_message("Verification passed");
    Ok(())
}

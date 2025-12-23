//! Verify command implementation

use indicatif::{ProgressBar, ProgressStyle};
use rflasher_core::chip::ChipDatabase;
use rflasher_core::flash::{self, FlashContext};
use rflasher_core::programmer::SpiMaster;
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Default chunk size for verification
const VERIFY_CHUNK_SIZE: usize = 4096;

/// Run the verify command
pub fn run_verify<M: SpiMaster + ?Sized>(
    master: &mut M,
    db: &ChipDatabase,
    input: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Probe for chip
    let ctx = flash::probe(master, db)?;

    println!(
        "Found: {} {} ({} bytes)",
        ctx.chip.vendor, ctx.chip.name, ctx.chip.total_size
    );

    // Read input file
    let mut file = File::open(input)?;
    let mut expected = Vec::new();
    file.read_to_end(&mut expected)?;

    println!("Read {} bytes from {:?}", expected.len(), input);

    // Validate size
    if expected.len() > ctx.total_size() {
        return Err(format!(
            "File size ({} bytes) exceeds chip size ({} bytes)",
            expected.len(),
            ctx.total_size()
        )
        .into());
    }

    // Verify the portion covered by the file
    verify_flash_with_progress(master, &ctx, &expected)?;

    // If file is smaller than chip, optionally check that the rest is 0xFF
    if expected.len() < ctx.total_size() {
        let remaining = ctx.total_size() - expected.len();
        println!(
            "Note: File is {} bytes smaller than chip. Remaining {} bytes not verified.",
            remaining, remaining
        );
    }

    println!("Verification passed!");

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
    let mut mismatch_count = 0usize;
    let mut first_mismatch: Option<(usize, u8, u8)> = None;

    while offset < total_size {
        let chunk_size = std::cmp::min(VERIFY_CHUNK_SIZE, total_size - offset);
        let chunk = &mut buf[..chunk_size];

        flash::read(master, ctx, offset as u32, chunk)?;

        // Compare
        let expected_chunk = &expected[offset..offset + chunk_size];
        for (i, (actual, expected)) in chunk.iter().zip(expected_chunk.iter()).enumerate() {
            if actual != expected {
                if first_mismatch.is_none() {
                    first_mismatch = Some((offset + i, *actual, *expected));
                }
                mismatch_count += 1;
            }
        }

        offset += chunk_size;
        pb.set_position(offset as u64);
    }

    if let Some((addr, actual, expected)) = first_mismatch {
        pb.abandon_with_message("Verification failed!");
        return Err(format!(
            "Verification failed: {} byte(s) differ. First mismatch at 0x{:08X}: expected 0x{:02X}, got 0x{:02X}",
            mismatch_count, addr, expected, actual
        )
        .into());
    }

    pb.finish_with_message("Verification passed");
    Ok(())
}

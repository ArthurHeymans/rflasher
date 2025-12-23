//! Write command implementation

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rflasher_core::chip::ChipDatabase;
use rflasher_core::flash::{self, FlashContext, WriteProgress, WriteStats};
use rflasher_core::programmer::SpiMaster;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// Default chunk size for writing (page size is typically 256 bytes)
const WRITE_CHUNK_SIZE: usize = 4096;
/// Default chunk size for verification
const VERIFY_CHUNK_SIZE: usize = 4096;

/// Progress reporter using indicatif progress bars
struct IndicatifProgress {
    multi: MultiProgress,
    current_bar: Option<ProgressBar>,
    phase: &'static str,
}

impl IndicatifProgress {
    fn new() -> Self {
        Self {
            multi: MultiProgress::new(),
            current_bar: None,
            phase: "",
        }
    }

    fn create_bar(&mut self, total: u64, phase: &'static str) {
        self.phase = phase;
        let pb = self.multi.add(ProgressBar::new(total));
        pb.set_style(
            ProgressStyle::default_bar()
                .template(&format!(
                    "{{spinner:.green}} [{{elapsed_precise}}] [{{bar:40.cyan/blue}}] {{bytes}}/{{total_bytes}} ({{bytes_per_sec}}, {{eta}}) {}",
                    phase
                ))
                .unwrap_or_else(|_| ProgressStyle::default_bar())
                .progress_chars("#>-"),
        );
        self.current_bar = Some(pb);
    }

    fn create_spinner(&mut self, message: String) {
        let pb = self.multi.add(ProgressBar::new_spinner());
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        pb.set_message(message);
        pb.enable_steady_tick(Duration::from_millis(100));
        self.current_bar = Some(pb);
    }

    fn finish(&mut self, message: &str) {
        if let Some(pb) = self.current_bar.take() {
            pb.finish_with_message(message.to_string());
        }
    }
}

impl WriteProgress for IndicatifProgress {
    fn reading(&mut self, total_bytes: usize) {
        self.create_bar(total_bytes as u64, "Reading");
    }

    fn read_progress(&mut self, bytes_read: usize) {
        if let Some(pb) = &self.current_bar {
            pb.set_position(bytes_read as u64);
        }
    }

    fn erasing(&mut self, blocks_to_erase: usize, bytes_to_erase: usize) {
        self.finish("Read complete");
        self.create_spinner(format!(
            "Erasing {} blocks ({} bytes)...",
            blocks_to_erase, bytes_to_erase
        ));
    }

    fn erase_progress(&mut self, blocks_erased: usize, _bytes_erased: usize) {
        if let Some(pb) = &self.current_bar {
            pb.set_message(format!("Erased {} blocks...", blocks_erased));
        }
    }

    fn writing(&mut self, bytes_to_write: usize) {
        self.finish("Erase complete");
        self.create_bar(bytes_to_write as u64, "Writing");
    }

    fn write_progress(&mut self, bytes_written: usize) {
        if let Some(pb) = &self.current_bar {
            pb.set_position(bytes_written as u64);
        }
    }

    fn complete(&mut self, stats: &WriteStats) {
        self.finish("Write complete");

        if !stats.flash_modified {
            println!("Flash already contains the desired data - no changes needed");
        } else {
            println!(
                "Smart write: {} bytes changed, {} blocks erased ({} bytes), {} bytes written",
                stats.bytes_changed,
                stats.erases_performed,
                stats.bytes_erased,
                stats.bytes_written
            );
        }
    }
}

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

    if no_erase {
        // Legacy mode: just write without smart comparison
        // (User explicitly requested no erase, implying they know what they're doing)
        write_flash_with_progress(master, &ctx, &data)?;
    } else {
        // Smart write mode: compare, erase only changed blocks, write only changed bytes
        let mut progress = IndicatifProgress::new();
        flash::smart_write(master, &ctx, &data, &mut progress)?;
    }

    // Verify if requested
    if do_verify {
        verify_flash_with_progress(master, &ctx, &data)?;
    }

    println!("Write complete!");

    Ok(())
}

/// Write data to flash with progress bar (legacy mode - writes all data)
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

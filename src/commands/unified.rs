//! Unified command implementations that work with any FlashDevice
//!
//! These commands work the same way regardless of whether the underlying
//! programmer is SPI-based or opaque.

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rflasher_core::flash::unified::{WriteProgress, WriteStats};
use rflasher_core::flash::{unified, FlashDevice};
use rflasher_core::layout::Layout;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

// =============================================================================
// Helper functions
// =============================================================================

/// Print flash size information
fn print_flash_size(flash_size: u32) {
    println!(
        "Flash size: {} bytes ({} KiB)",
        flash_size,
        flash_size / 1024
    );
}

/// Read file contents into a Vec
fn read_file(path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut file = File::open(path)?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    println!("Read {} bytes from {:?}", data.len(), path);
    Ok(data)
}

/// Create a standard progress bar style
fn create_progress_bar_style() -> Result<ProgressStyle, Box<dyn std::error::Error>> {
    Ok(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?
        .progress_chars("#>-"))
}

/// Create a progress bar with custom phase message
fn create_progress_bar_with_phase(
    total: u64,
    phase: &str,
) -> Result<ProgressBar, Box<dyn std::error::Error>> {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(&format!(
                "{{spinner:.green}} [{{elapsed_precise}}] [{{bar:40.cyan/blue}}] {{bytes}}/{{total_bytes}} ({{bytes_per_sec}}, {{eta}}) {}",
                phase
            ))?
            .progress_chars("#>-"),
    );
    Ok(pb)
}

/// Create a standard spinner style
fn create_spinner_style() -> Result<ProgressStyle, Box<dyn std::error::Error>> {
    Ok(ProgressStyle::default_spinner().template("{spinner:.green} {msg}")?)
}

/// Display included regions
fn display_included_regions(included: &[&rflasher_core::layout::Region], action: &str) {
    println!("{} {} region(s):", action, included.len());
    for region in included {
        println!(
            "  {} (0x{:08X} - 0x{:08X}, {} bytes)",
            region.name,
            region.start,
            region.end,
            region.size()
        );
    }
}

/// Create a layout covering the entire flash
fn full_flash_layout(flash_size: u32) -> Layout {
    use rflasher_core::layout::{LayoutSource, Region};

    let mut layout = Layout::with_source(LayoutSource::Manual);
    let mut region = Region::new("full", 0, flash_size - 1);
    region.included = true;
    layout.add_region(region);
    layout
}

// =============================================================================
// Progress reporting
// =============================================================================

/// Progress reporter using indicatif progress bars
pub struct IndicatifProgress {
    multi: MultiProgress,
    current_bar: Option<ProgressBar>,
    phase: &'static str,
}

impl IndicatifProgress {
    pub fn new() -> Self {
        Self {
            multi: MultiProgress::new(),
            current_bar: None,
            phase: "",
        }
    }

    fn create_bar(&mut self, total: u64, phase: &'static str) {
        self.phase = phase;
        let pb = self.multi.add(
            create_progress_bar_with_phase(total, phase)
                .unwrap_or_else(|_| ProgressBar::new(total)),
        );
        self.current_bar = Some(pb);
    }

    fn create_spinner(&mut self, message: String) {
        let pb = self.multi.add(ProgressBar::new_spinner());
        pb.set_style(create_spinner_style().unwrap_or_else(|_| ProgressStyle::default_spinner()));
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

impl Default for IndicatifProgress {
    fn default() -> Self {
        Self::new()
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

// =============================================================================
// Read operations
// =============================================================================

/// Default chunk size for reading (4 KiB)
const READ_CHUNK_SIZE: usize = 4096;

/// Run the unified read command
pub fn run_read<D: FlashDevice + ?Sized>(
    device: &mut D,
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let layout = full_flash_layout(device.size());
    run_read_with_layout(device, output, &layout)
}

/// Run the unified read command with layout
pub fn run_read_with_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    output: &Path,
    layout: &Layout,
) -> Result<(), Box<dyn std::error::Error>> {
    let flash_size = device.size();
    print_flash_size(flash_size);

    // Display included regions
    let included: Vec<_> = layout.included_regions().collect();
    if included.is_empty() {
        return Err("No regions selected for reading. Use --include to select regions.".into());
    }

    display_included_regions(&included, "Reading");

    // Calculate total bytes to read
    let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();

    // Allocate buffer for full chip (fill with 0xFF for non-included regions)
    let mut data = vec![0xFFu8; flash_size as usize];

    // Create progress bar
    let pb = ProgressBar::new(total_bytes as u64);
    pb.set_style(create_progress_bar_style()?);

    let mut bytes_read = 0usize;

    // Read each included region
    for region in included {
        let mut offset = region.start;
        while offset <= region.end {
            let remaining = (region.end - offset + 1) as usize;
            let chunk_size = std::cmp::min(READ_CHUNK_SIZE, remaining);
            let chunk = &mut data[offset as usize..offset as usize + chunk_size];

            device.read(offset, chunk)?;

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

// =============================================================================
// Write operations
// =============================================================================

/// Run the unified write command
pub fn run_write<D: FlashDevice + ?Sized>(
    device: &mut D,
    input: &Path,
    do_verify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut layout = full_flash_layout(device.size());
    run_write_with_layout(device, input, &mut layout, do_verify)
}

/// Run the unified write command with layout
pub fn run_write_with_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    input: &Path,
    layout: &mut Layout,
    do_verify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let flash_size = device.size();
    print_flash_size(flash_size);

    // Read input file
    let file_data = read_file(input)?;
    let file_size = file_data.len();

    // Display included regions
    let included: Vec<_> = layout.included_regions().collect();
    if included.is_empty() {
        return Err("No regions selected for writing. Use --include to select regions.".into());
    }

    display_included_regions(&included, "Writing");

    // Check for readonly regions
    let readonly = layout.readonly_included();
    if !readonly.is_empty() {
        let names: Vec<_> = readonly.iter().map(|r| r.name.as_str()).collect();
        return Err(format!("Cannot write to readonly region(s): {}", names.join(", ")).into());
    }

    // Validate file size
    if file_size > flash_size as usize {
        return Err(format!(
            "File size ({} bytes) exceeds flash size ({} bytes)",
            file_size, flash_size
        )
        .into());
    }

    if included.len() > 1 && file_size != flash_size as usize {
        return Err(format!(
            "Multiple regions selected: file must be exactly flash size ({} bytes), got {} bytes",
            flash_size, file_size
        )
        .into());
    }

    let (image, effective_write_size) = if file_size == flash_size as usize {
        // Full flash image
        (file_data, included.iter().map(|r| r.size() as usize).sum())
    } else {
        // Single region, file <= region size
        let region = &included[0];
        let region_size = region.size() as usize;

        if file_size > region_size {
            return Err(format!(
                "File size ({} bytes) larger than region '{}' ({} bytes) but smaller than flash size",
                file_size, region.name, region_size
            )
            .into());
        }

        let mut chip_image = vec![0xFFu8; flash_size as usize];
        let dest_start = region.start as usize;
        chip_image[dest_start..dest_start + file_size].copy_from_slice(&file_data);

        if file_size < region_size {
            println!(
                "Note: File ({} bytes) is smaller than region ({} bytes)",
                file_size, region_size
            );
        }

        (chip_image, file_size)
    };

    // Adjust layout if file is smaller than region
    let effective_layout = if included.len() == 1 && file_size < included[0].size() as usize {
        let region = &included[0];
        let mut modified_layout = layout.clone();
        let actual_end = region.start + file_size as u32 - 1;
        modified_layout.update_region_end(&region.name, actual_end)?;
        modified_layout
    } else {
        layout.clone()
    };

    // Smart write using layout
    let mut progress = IndicatifProgress::new();
    let stats = unified::smart_write_by_layout(device, &effective_layout, &image, &mut progress)?;

    // Verify if requested
    if do_verify {
        if stats.flash_modified {
            verify_by_layout(device, &effective_layout, &image)?;
        } else {
            println!("Skipping verification - no changes were made");
        }
    }

    println!(
        "Write complete! ({} bytes written to flash)",
        effective_write_size
    );

    Ok(())
}

// =============================================================================
// Erase operations
// =============================================================================

/// Run the unified erase command
pub fn run_erase<D: FlashDevice + ?Sized>(
    device: &mut D,
) -> Result<(), Box<dyn std::error::Error>> {
    let layout = full_flash_layout(device.size());
    run_erase_with_layout(device, &layout)
}

/// Run the unified erase command with layout
pub fn run_erase_with_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    layout: &Layout,
) -> Result<(), Box<dyn std::error::Error>> {
    print_flash_size(device.size());

    let included: Vec<_> = layout.included_regions().collect();
    if included.is_empty() {
        return Err("No regions selected for erasing. Use --include to select regions.".into());
    }

    let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();
    println!(
        "Erasing {} region(s) ({} bytes):",
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

    let pb = ProgressBar::new_spinner();
    pb.set_style(create_spinner_style()?);
    pb.enable_steady_tick(Duration::from_millis(100));

    for region in included {
        pb.set_message(format!("Erasing {}...", region.name));
        unified::erase_region(device, region)?;
    }

    pb.finish_with_message("Erase complete");

    Ok(())
}

// =============================================================================
// Verify operations
// =============================================================================

/// Compare a chunk with expected data and return detailed error on mismatch
fn verify_chunk(
    chunk: &[u8],
    expected_chunk: &[u8],
    base_offset: usize,
    region_name: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    if chunk != expected_chunk {
        // Find first difference
        for (i, (a, b)) in chunk.iter().zip(expected_chunk.iter()).enumerate() {
            if a != b {
                let error_msg = if let Some(name) = region_name {
                    format!(
                        "Verification failed in region '{}' at offset 0x{:08X}: expected 0x{:02X}, got 0x{:02X}",
                        name,
                        base_offset + i,
                        b,
                        a
                    )
                } else {
                    format!(
                        "Verification failed at offset 0x{:08X}: expected 0x{:02X}, got 0x{:02X}",
                        base_offset + i,
                        b,
                        a
                    )
                };
                return Err(error_msg.into());
            }
        }
    }
    Ok(())
}

/// Verify flash contents against expected data
pub fn verify_flash<D: FlashDevice + ?Sized>(
    device: &mut D,
    expected: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let total_size = expected.len();
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    let pb = create_progress_bar_with_phase(total_size as u64, "Verifying")?;

    let mut offset = 0usize;
    while offset < total_size {
        let chunk_size = std::cmp::min(READ_CHUNK_SIZE, total_size - offset);
        let chunk = &mut buf[..chunk_size];

        device.read(offset as u32, chunk)?;

        // Compare
        let expected_chunk = &expected[offset..offset + chunk_size];
        if let Err(e) = verify_chunk(chunk, expected_chunk, offset, None) {
            pb.abandon_with_message("Verification failed!");
            return Err(e);
        }

        offset += chunk_size;
        pb.set_position(offset as u64);
    }

    pb.finish_with_message("Verification passed");
    Ok(())
}

/// Run the unified verify command
pub fn run_verify<D: FlashDevice + ?Sized>(
    device: &mut D,
    input: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let flash_size = device.size();
    print_flash_size(flash_size);

    // Read input file
    let expected = read_file(input)?;

    // Validate size
    if expected.len() > flash_size as usize {
        return Err(format!(
            "File size ({} bytes) exceeds flash size ({} bytes)",
            expected.len(),
            flash_size
        )
        .into());
    }

    verify_flash(device, &expected)?;
    println!("Verification passed!");

    Ok(())
}

/// Verify included regions against expected data
pub fn verify_by_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    layout: &Layout,
    expected: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let included: Vec<_> = layout.included_regions().collect();
    let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();

    let pb = create_progress_bar_with_phase(total_bytes as u64, "Verifying")?;

    let mut bytes_verified = 0usize;
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    for region in included {
        let mut offset = region.start;
        while offset <= region.end {
            let chunk_size = std::cmp::min(READ_CHUNK_SIZE, (region.end - offset + 1) as usize);
            let chunk = &mut buf[..chunk_size];

            device.read(offset, chunk)?;

            // Compare
            let expected_chunk = &expected[offset as usize..offset as usize + chunk_size];
            if let Err(e) = verify_chunk(chunk, expected_chunk, offset as usize, Some(&region.name))
            {
                pb.abandon_with_message("Verification failed!");
                return Err(e);
            }

            offset += chunk_size as u32;
            bytes_verified += chunk_size;
            pb.set_position(bytes_verified as u64);
        }
    }

    pb.finish_with_message("Verification passed");
    Ok(())
}

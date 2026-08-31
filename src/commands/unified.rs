//! Unified command implementations that work with any FlashDevice
//!
//! These commands work the same way regardless of whether the underlying
//! programmer is SPI-based or opaque.

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rflasher_core::flash::unified::{WriteProgress, WriteStats};
use rflasher_core::flash::{FlashDevice, unified};
use rflasher_core::layout::{Layout, LayoutError};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Per-region file assignments: (region name, file path)
pub type RegionFiles = [(String, PathBuf)];
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
    included.iter().for_each(|region| {
        println!(
            "  {} (0x{:08X} - 0x{:08X}, {} bytes)",
            region.name,
            region.start,
            region.end,
            region.size()
        );
    });
}

/// Validate a layout against the flash size with a friendly error message
fn validate_layout(layout: &Layout, flash_size: u32) -> Result<(), Box<dyn std::error::Error>> {
    layout.validate(flash_size).map_err(|e| match e {
        LayoutError::RegionOutOfBounds => format!(
            "Layout region extends beyond flash size ({} bytes)",
            flash_size
        )
        .into(),
        e => format!("Invalid layout: {}", e).into(),
    })
}

/// Validate per-region file assignments against the final filtered layout.
pub(crate) fn validate_region_files(
    layout: &Layout,
    region_files: &RegionFiles,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut seen = HashSet::new();

    for (name, _) in region_files {
        if !seen.insert(name.as_str()) {
            return Err(format!("Region '{}' was given more than one file", name).into());
        }

        let region = layout
            .find_region(name)
            .ok_or_else(|| format!("Region '{}' not found in layout", name))?;
        if !region.included {
            return Err(format!("Region '{}' has a file assignment but is excluded", name).into());
        }
    }

    Ok(())
}

/// Ensure every included region has a per-region file assigned.
///
/// Used when no whole-image file was given, so regions without their own
/// file would have no data source (write/verify) or destination (read).
fn require_full_region_file_coverage(
    included: &[&rflasher_core::layout::Region],
    region_files: &RegionFiles,
    what: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let missing: Vec<_> = included
        .iter()
        .filter(|r| !region_files.iter().any(|(name, _)| *name == r.name))
        .map(|r| r.name.as_str())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "No {} file given and no NAME:FILE for region(s): {}",
            what,
            missing.join(", ")
        )
        .into())
    }
}

/// Overlay per-region files onto a full-chip image.
///
/// Each file is copied to its region's base address and must not exceed the
/// region size. Returns an adjusted layout where regions covered by a file
/// smaller than the region are shrunk to the file's extent.
fn apply_region_files(
    layout: &Layout,
    image: &mut [u8],
    region_files: &RegionFiles,
) -> Result<Layout, Box<dyn std::error::Error>> {
    validate_region_files(layout, region_files)?;

    let mut effective = layout.clone();

    for (name, path) in region_files {
        let region = layout
            .find_region(name)
            .ok_or_else(|| format!("Region '{}' not found in layout", name))?;
        let data = read_file(path)?;
        if data.is_empty() {
            return Err(format!("Region file {:?} is empty", path).into());
        }

        let region_size = region.size() as usize;
        if data.len() > region_size {
            return Err(format!(
                "File {:?} ({} bytes) larger than region '{}' ({} bytes)",
                path,
                data.len(),
                name,
                region_size
            )
            .into());
        }

        let start = region.start as usize;
        image[start..start + data.len()].copy_from_slice(&data);

        if data.len() < region_size {
            println!(
                "Note: File {:?} ({} bytes) is smaller than region '{}' ({} bytes)",
                path,
                data.len(),
                name,
                region_size
            );
            effective.update_region_end(name, region.start + data.len() as u32 - 1)?;
        }
    }

    Ok(effective)
}

/// Build the full-chip image to write/verify from an optional whole-image
/// file plus per-region files.
///
/// Returns the image, the effective layout (regions shrunk to partial file
/// extents), and the effective number of bytes covered.
fn build_image(
    input: Option<&Path>,
    layout: &Layout,
    region_files: &RegionFiles,
    included: &[&rflasher_core::layout::Region],
    flash_size: u32,
) -> Result<(Vec<u8>, Layout, usize), Box<dyn std::error::Error>> {
    if !region_files.is_empty() {
        // Per-region files, optionally on top of a full chip image
        let mut image = match input {
            Some(path) => {
                let data = read_file(path)?;
                if data.len() != flash_size as usize {
                    return Err(format!(
                        "With per-region files, FILE must be a full chip image ({} bytes), got {} bytes",
                        flash_size,
                        data.len()
                    )
                    .into());
                }
                data
            }
            None => {
                require_full_region_file_coverage(included, region_files, "input")?;
                vec![0xFFu8; flash_size as usize]
            }
        };

        let effective_layout = apply_region_files(layout, &mut image, region_files)?;
        let covered = effective_layout
            .included_regions()
            .map(|r| r.size() as usize)
            .sum();
        return Ok((image, effective_layout, covered));
    }

    // Single whole-image file; interpretation depends on its size
    let input = input.ok_or("Input file required (no per-region NAME:FILE given)")?;
    let file_data = read_file(input)?;
    if file_data.is_empty() {
        return Err("Input file is empty".into());
    }
    let file_size = file_data.len();

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

    if file_size == flash_size as usize {
        // Full flash image
        let covered = included.iter().map(|r| r.size() as usize).sum();
        return Ok((file_data, layout.clone(), covered));
    }

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

    // Shrink the region to the portion covered by the file
    let effective_layout = if file_size < region_size {
        println!(
            "Note: File ({} bytes) is smaller than region ({} bytes)",
            file_size, region_size
        );
        let mut modified_layout = layout.clone();
        modified_layout.update_region_end(&region.name, region.start + file_size as u32 - 1)?;
        modified_layout
    } else {
        layout.clone()
    };

    Ok((chip_image, effective_layout, file_size))
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
pub async fn run_read<D: FlashDevice + ?Sized>(
    device: &mut D,
    output: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let layout = full_flash_layout(device.size());
    run_read_with_layout(device, Some(output), &layout, &[]).await
}

/// Run the unified read command with layout
///
/// `output` receives the full (0xFF-padded) image when given. Each entry in
/// `region_files` additionally gets that region's data written to its own
/// file. At least one destination must cover every included region.
pub async fn run_read_with_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    output: Option<&Path>,
    layout: &Layout,
    region_files: &RegionFiles,
) -> Result<(), Box<dyn std::error::Error>> {
    let flash_size = device.size();

    // Validate region bounds against the actual chip before slicing into a
    // flash-sized buffer; a corrupt FMAP could otherwise panic on read.
    validate_layout(layout, flash_size)?;
    validate_region_files(layout, region_files)?;

    print_flash_size(flash_size);

    // Display included regions
    let included: Vec<_> = layout.included_regions().collect();
    if included.is_empty() {
        return Err("No regions selected for reading. Use --include to select regions.".into());
    }

    if output.is_none() {
        require_full_region_file_coverage(&included, region_files, "output")?;
    }

    display_included_regions(&included, "Reading");

    // Calculate total bytes to read
    let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();

    // Allocate buffer for full chip (fill with 0xFF for non-included regions)
    let mut data = vec![0xFFu8; flash_size as usize];

    // Create progress bar
    let pb = ProgressBar::new(total_bytes as u64);
    pb.set_style(create_progress_bar_style()?);

    // Read each included region
    let mut bytes_read = 0usize;
    for (region, offset) in included.iter().flat_map(|region| {
        (region.start..=region.end)
            .step_by(READ_CHUNK_SIZE)
            .map(move |offset| (region, offset))
    }) {
        let remaining = (region.end - offset + 1) as usize;
        let chunk_size = std::cmp::min(READ_CHUNK_SIZE, remaining);
        let chunk = &mut data[offset as usize..offset as usize + chunk_size];

        device.read(offset, chunk).await?;

        bytes_read += chunk_size;
        pb.set_position(bytes_read as u64);
    }

    pb.finish_with_message("Read complete");

    // Write per-region files
    for (name, path) in region_files {
        let region = layout
            .find_region(name)
            .ok_or_else(|| format!("Region '{}' not found in layout", name))?;
        let region_data = &data[region.start as usize..=region.end as usize];
        std::fs::write(path, region_data)?;
        println!(
            "Wrote region '{}' ({} bytes) to {:?}",
            name,
            region_data.len(),
            path
        );
    }

    // Write full image
    if let Some(output) = output {
        let mut file = File::create(output)?;
        file.write_all(&data)?;

        println!("Wrote {} bytes to {:?}", data.len(), output);
        println!(
            "  ({} bytes from included regions, rest filled with 0xFF)",
            bytes_read
        );
    }

    Ok(())
}

// =============================================================================
// Write operations
// =============================================================================

/// Run the unified write command
pub async fn run_write<D: FlashDevice + ?Sized>(
    device: &mut D,
    input: &Path,
    do_verify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut layout = full_flash_layout(device.size());
    run_write_with_layout(device, Some(input), &mut layout, &[], do_verify).await
}

/// Run the unified write command with layout
///
/// `region_files` provide per-region data (each file written at its region's
/// base address). `input`, when given alongside region files, must be a full
/// chip image and supplies data for the remaining regions; without region
/// files the size-based rules documented on the CLI apply.
pub async fn run_write_with_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    input: Option<&Path>,
    layout: &mut Layout,
    region_files: &RegionFiles,
    do_verify: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let flash_size = device.size();

    // Validate region bounds against the actual chip before constructing or
    // slicing a flash-sized image; a corrupt layout could otherwise panic.
    validate_layout(layout, flash_size)?;

    print_flash_size(flash_size);

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

    let (image, effective_layout, effective_write_size) =
        build_image(input, layout, region_files, &included, flash_size)?;

    // Smart write using layout
    let mut progress = IndicatifProgress::new();
    let stats =
        unified::smart_write_by_layout(device, &effective_layout, &image, &mut progress).await?;

    // Verify if requested
    if do_verify {
        if stats.flash_modified {
            verify_by_layout(device, &effective_layout, &image).await?;
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
pub async fn run_erase<D: FlashDevice + ?Sized>(
    device: &mut D,
) -> Result<(), Box<dyn std::error::Error>> {
    let layout = full_flash_layout(device.size());
    run_erase_with_layout(device, &layout).await
}

/// Run the unified erase command with layout
pub async fn run_erase_with_layout<D: FlashDevice + ?Sized>(
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

    included.iter().for_each(|region| {
        println!(
            "  {} (0x{:08X} - 0x{:08X}, {} bytes)",
            region.name,
            region.start,
            region.end,
            region.size()
        );
    });

    let pb = ProgressBar::new_spinner();
    pb.set_style(create_spinner_style()?);
    pb.enable_steady_tick(Duration::from_millis(100));

    for region in &included {
        pb.set_message(format!("Erasing {}...", region.name));
        unified::erase_region(device, region).await?;
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
    chunk
        .iter()
        .zip(expected_chunk.iter())
        .enumerate()
        .find(|(_, (a, b))| a != b)
        .map_or(Ok(()), |(i, (a, b))| {
            let error_msg = match region_name {
                Some(name) => format!(
                    "Verification failed in region '{}' at offset 0x{:08X}: expected 0x{:02X}, got 0x{:02X}",
                    name,
                    base_offset + i,
                    b,
                    a
                ),
                None => format!(
                    "Verification failed at offset 0x{:08X}: expected 0x{:02X}, got 0x{:02X}",
                    base_offset + i,
                    b,
                    a
                ),
            };
            Err(error_msg.into())
        })
}

/// Verify flash contents against expected data
pub async fn verify_flash<D: FlashDevice + ?Sized>(
    device: &mut D,
    expected: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let total_size = expected.len();
    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    let pb = create_progress_bar_with_phase(total_size as u64, "Verifying")?;

    let result = async {
        for offset in (0..total_size).step_by(READ_CHUNK_SIZE) {
            let chunk_size = std::cmp::min(READ_CHUNK_SIZE, total_size - offset);
            let chunk = &mut buf[..chunk_size];

            device.read(offset as u32, chunk).await?;

            // Compare
            let expected_chunk = &expected[offset..offset + chunk_size];
            verify_chunk(chunk, expected_chunk, offset, None)?;

            pb.set_position((offset + chunk_size) as u64);
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;

    match result {
        Ok(()) => {
            pb.finish_with_message("Verification passed");
            Ok(())
        }
        Err(e) => {
            pb.abandon_with_message("Verification failed!");
            Err(e)
        }
    }
}

/// Run the unified verify command
///
/// The image must match the flash size exactly; verifying a smaller file
/// against offset 0 would be ambiguous and silently compare the wrong data.
pub async fn run_verify<D: FlashDevice + ?Sized>(
    device: &mut D,
    input: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let flash_size = device.size();
    print_flash_size(flash_size);

    // Read input file
    let expected = read_file(input)?;

    // Validate size
    if expected.len() != flash_size as usize {
        return Err(format!(
            "File size ({} bytes) does not match flash size ({} bytes). \
             Use --layout/--ifd/--fmap with --region to verify a region.",
            expected.len(),
            flash_size
        )
        .into());
    }

    verify_flash(device, &expected).await?;
    println!("Verification passed!");

    Ok(())
}

/// Run the unified verify command with layout
///
/// Size rules mirror `run_write_with_layout`, including per-region files.
pub async fn run_verify_with_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    input: Option<&Path>,
    layout: &Layout,
    region_files: &RegionFiles,
) -> Result<(), Box<dyn std::error::Error>> {
    let flash_size = device.size();

    // Validate region bounds against the actual chip before slicing into a
    // flash-sized buffer; a corrupt FMAP could otherwise panic on verify.
    validate_layout(layout, flash_size)?;

    print_flash_size(flash_size);

    let included: Vec<_> = layout.included_regions().collect();
    if included.is_empty() {
        return Err(
            "No regions selected for verification. Use --include to select regions.".into(),
        );
    }
    display_included_regions(&included, "Verifying");

    let (image, effective_layout, _) =
        build_image(input, layout, region_files, &included, flash_size)?;

    verify_by_layout(device, &effective_layout, &image).await?;
    println!("Verification passed!");

    Ok(())
}

/// Verify included regions against expected data
pub async fn verify_by_layout<D: FlashDevice + ?Sized>(
    device: &mut D,
    layout: &Layout,
    expected: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let included: Vec<_> = layout.included_regions().collect();
    let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();

    let pb = create_progress_bar_with_phase(total_bytes as u64, "Verifying")?;

    let mut buf = vec![0u8; READ_CHUNK_SIZE];

    let result = async {
        let mut bytes_verified = 0usize;
        for (region, offset) in included.iter().flat_map(|region| {
            (region.start..=region.end)
                .step_by(READ_CHUNK_SIZE)
                .map(move |offset| (region, offset))
        }) {
            let chunk_size = std::cmp::min(READ_CHUNK_SIZE, (region.end - offset + 1) as usize);
            let chunk = &mut buf[..chunk_size];

            device.read(offset, chunk).await?;

            // Compare
            let expected_chunk = &expected[offset as usize..offset as usize + chunk_size];
            verify_chunk(chunk, expected_chunk, offset as usize, Some(&region.name))?;

            bytes_verified += chunk_size;
            pb.set_position(bytes_verified as u64);
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;

    match result {
        Ok(_) => {
            pb.finish_with_message("Verification passed");
            Ok(())
        }
        Err(e) => {
            pb.abandon_with_message("Verification failed!");
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rflasher_core::layout::{LayoutSource, Region};

    const FLASH_SIZE: u32 = 0x1000;

    /// Layout with two included regions: a (0x000-0x7FF), b (0x800-0xFFF)
    fn two_region_layout() -> Layout {
        let mut layout = Layout::with_source(LayoutSource::Manual);
        for (name, start, end) in [("a", 0x000, 0x7FF), ("b", 0x800, 0xFFF)] {
            let mut region = Region::new(name, start, end);
            region.included = true;
            layout.add_region(region);
        }
        layout
    }

    fn temp_file(name: &str, data: &[u8]) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("rflasher-test-{}-{}", std::process::id(), name));
        std::fs::write(&path, data).unwrap();
        path
    }

    #[test]
    fn region_files_without_image_file() {
        let layout = two_region_layout();
        let included: Vec<_> = layout.included_regions().collect();
        let files = vec![
            ("a".to_string(), temp_file("a.bin", &[0xAA; 0x800])),
            ("b".to_string(), temp_file("b.bin", &[0xBB; 0x800])),
        ];

        let (image, effective, covered) =
            build_image(None, &layout, &files, &included, FLASH_SIZE).unwrap();

        assert_eq!(covered, 0x1000);
        assert!(image[..0x800].iter().all(|&b| b == 0xAA));
        assert!(image[0x800..].iter().all(|&b| b == 0xBB));
        assert_eq!(effective.included_regions().count(), 2);
    }

    #[test]
    fn partial_region_file_shrinks_region() {
        let layout = two_region_layout();
        let included: Vec<_> = layout.included_regions().collect();
        let files = vec![
            ("a".to_string(), temp_file("a-part.bin", &[0xAA; 0x100])),
            ("b".to_string(), temp_file("b-full.bin", &[0xBB; 0x800])),
        ];

        let (image, effective, covered) =
            build_image(None, &layout, &files, &included, FLASH_SIZE).unwrap();

        assert_eq!(covered, 0x100 + 0x800);
        assert!(image[..0x100].iter().all(|&b| b == 0xAA));
        assert!(image[0x100..0x800].iter().all(|&b| b == 0xFF));
        assert_eq!(effective.find_region("a").unwrap().end, 0x0FF);
    }

    #[test]
    fn missing_region_file_without_image_errors() {
        let layout = two_region_layout();
        let included: Vec<_> = layout.included_regions().collect();
        let files = vec![("a".to_string(), temp_file("a-only.bin", &[0xAA; 0x800]))];

        let err = build_image(None, &layout, &files, &included, FLASH_SIZE).unwrap_err();
        assert!(err.to_string().contains("b"), "unexpected error: {}", err);
    }

    #[test]
    fn region_file_plus_full_image_fills_rest() {
        let layout = two_region_layout();
        let included: Vec<_> = layout.included_regions().collect();
        let full = temp_file("full.bin", &[0x11; 0x1000]);
        let files = vec![("b".to_string(), temp_file("b2.bin", &[0xBB; 0x800]))];

        let (image, _, covered) =
            build_image(Some(&full), &layout, &files, &included, FLASH_SIZE).unwrap();

        assert_eq!(covered, 0x1000);
        assert!(image[..0x800].iter().all(|&b| b == 0x11));
        assert!(image[0x800..].iter().all(|&b| b == 0xBB));
    }

    #[test]
    fn region_file_plus_partial_image_errors() {
        let layout = two_region_layout();
        let included: Vec<_> = layout.included_regions().collect();
        let small = temp_file("small.bin", &[0x11; 0x800]);
        let files = vec![("b".to_string(), temp_file("b3.bin", &[0xBB; 0x800]))];

        assert!(build_image(Some(&small), &layout, &files, &included, FLASH_SIZE).is_err());
    }

    #[test]
    fn oversized_region_file_errors() {
        let layout = two_region_layout();
        let included: Vec<_> = layout.included_regions().collect();
        let files = vec![("a".to_string(), temp_file("a-big.bin", &[0xAA; 0x900]))];

        assert!(build_image(None, &layout, &files, &included, FLASH_SIZE).is_err());
    }

    #[test]
    fn duplicate_region_files_error() {
        let layout = two_region_layout();
        let included: Vec<_> = layout.included_regions().collect();
        let full = temp_file("duplicate-full.bin", &[0x11; FLASH_SIZE as usize]);
        let files = vec![
            ("a".to_string(), temp_file("a-first.bin", &[0xAA; 0x100])),
            ("a".to_string(), temp_file("a-second.bin", &[0xBB; 0x800])),
        ];

        let err = build_image(Some(&full), &layout, &files, &included, FLASH_SIZE).unwrap_err();
        assert!(
            err.to_string().contains("more than one file"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn excluded_region_file_errors() {
        let mut layout = two_region_layout();
        layout.exclude_region("a").unwrap();
        let included: Vec<_> = layout.included_regions().collect();
        let full = temp_file("excluded-full.bin", &[0x11; FLASH_SIZE as usize]);
        let files = vec![("a".to_string(), temp_file("a-excluded.bin", &[0xAA; 0x800]))];

        let err = build_image(Some(&full), &layout, &files, &included, FLASH_SIZE).unwrap_err();
        assert!(
            err.to_string().contains("excluded"),
            "unexpected error: {}",
            err
        );
    }
}

//! Command implementations for opaque programmers (e.g., Intel internal)
//!
//! Opaque programmers don't expose raw SPI access, so we can't probe JEDEC IDs.
//! Instead, we use the Intel Flash Descriptor (IFD) to determine flash layout
//! and size, and perform read/write/erase operations using the programmer's
//! hardware sequencing capabilities.

use crate::cli::LayoutArgs;
use indicatif::{ProgressBar, ProgressStyle};
use rflasher_core::layout::parse_ifd;
use rflasher_core::programmer::OpaqueMaster;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

/// Default chunk size for operations (matches hwseq erase block size)
const CHUNK_SIZE: usize = 4096;

/// Erase block size for hardware sequencing (4KB on PCH100+)
const ERASE_BLOCK_SIZE: usize = 4096;

/// Get flash size from an opaque programmer
///
/// For Intel internal programmers, we read the IFD to determine flash size.
/// If IFD is not available, we use the size reported by the programmer.
fn get_flash_size(master: &mut dyn OpaqueMaster) -> Result<u32, Box<dyn std::error::Error>> {
    // First, try to read the IFD header to get the real flash size
    let mut header = [0u8; 4096];
    master.read(0, &mut header)?;

    // Try to parse IFD - it returns a Layout directly
    if let Ok(layout) = parse_ifd(&header) {
        // Get the highest region end address
        let size = layout.regions.iter().map(|r| r.end + 1).max().unwrap_or(0);

        if size > 0 {
            return Ok(size);
        }
    }

    // Fallback to programmer-reported size
    let size = master.size();
    if size > 0 {
        return Ok(size as u32);
    }

    Err("Cannot determine flash size. Use --length to specify manually.".into())
}

/// Probe an opaque programmer
///
/// For opaque programmers, we show what information is available without
/// JEDEC ID probing.
pub fn run_probe_opaque(master: &mut dyn OpaqueMaster) -> Result<(), Box<dyn std::error::Error>> {
    println!("Opaque Programmer Probe");
    println!("========================");
    println!();

    // Try to read the IFD to get flash information
    let mut header = [0u8; 4096];
    master.read(0, &mut header)?;

    match parse_ifd(&header) {
        Ok(layout) => {
            println!("Intel Flash Descriptor found!");
            println!();

            // Calculate flash size from regions
            let flash_size: u32 = layout.regions.iter().map(|r| r.end + 1).max().unwrap_or(0);

            println!(
                "Flash size: {} bytes ({} MiB)",
                flash_size,
                flash_size / (1024 * 1024)
            );
            println!();
            println!("Regions:");
            for region in &layout.regions {
                println!(
                    "  {:12} 0x{:08X} - 0x{:08X} ({} KiB)",
                    region.name,
                    region.start,
                    region.end,
                    (region.end - region.start + 1) / 1024
                );
            }
        }
        Err(_) => {
            println!("No Intel Flash Descriptor found.");
            println!();
            let size = master.size();
            if size > 0 {
                println!("Programmer reports flash size: {} bytes", size);
            } else {
                println!("Flash size unknown. Use --length to specify size for reads.");
            }
        }
    }

    Ok(())
}

/// Read flash using an opaque programmer
pub fn run_read_opaque(
    master: &mut dyn OpaqueMaster,
    output: &Path,
    layout_args: &LayoutArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // Determine flash size
    let flash_size = get_flash_size(master)?;

    println!(
        "Flash size: {} bytes ({} MiB)",
        flash_size,
        flash_size / (1024 * 1024)
    );

    // Determine what to read
    let (start, length, layout) = if layout_args.has_layout_source() {
        // Read IFD-based layout
        let mut header = [0u8; 4096];
        master.read(0, &mut header)?;
        let mut layout = parse_ifd(&header)?;

        // Apply region filters
        if let Some(region_name) = &layout_args.region {
            layout.include_region(region_name)?;
        }
        for name in &layout_args.include {
            layout.include_region(name)?;
        }
        for name in &layout_args.exclude {
            layout.exclude_region(name)?;
        }
        if !layout.has_included_regions() {
            layout.include_all();
        }

        // Read all included regions
        (0, flash_size, Some(layout))
    } else {
        // Read entire flash
        (0, flash_size, None)
    };

    // Allocate buffer
    let mut data = vec![0xFFu8; length as usize];

    // Read with progress
    let pb = ProgressBar::new(length as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?
            .progress_chars("#>-"),
    );

    if let Some(layout) = &layout {
        // Read only included regions
        let included: Vec<_> = layout.included_regions().collect();
        let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();
        pb.set_length(total_bytes as u64);

        let mut bytes_read = 0usize;
        for region in included {
            let mut offset = region.start;
            while offset <= region.end {
                let chunk_len = std::cmp::min(CHUNK_SIZE, (region.end - offset + 1) as usize);
                master.read(
                    offset,
                    &mut data[offset as usize..offset as usize + chunk_len],
                )?;
                offset += chunk_len as u32;
                bytes_read += chunk_len;
                pb.set_position(bytes_read as u64);
            }
        }
    } else {
        // Read entire flash
        let mut offset = start;
        while offset < start + length {
            let chunk_len = std::cmp::min(CHUNK_SIZE, (start + length - offset) as usize);
            master.read(
                offset,
                &mut data[offset as usize..offset as usize + chunk_len],
            )?;
            offset += chunk_len as u32;
            pb.set_position((offset - start) as u64);
        }
    }

    pb.finish_with_message("Read complete");

    // Write to file
    let mut file = File::create(output)?;
    file.write_all(&data)?;

    println!("Wrote {} bytes to {:?}", data.len(), output);

    Ok(())
}

/// Check if an erase is needed for a block transition
///
/// Flash can only transition bits from 1 to 0 during writes.
/// To go from 0 to 1, an erase is required.
fn need_erase(have: &[u8], want: &[u8]) -> bool {
    have.iter().zip(want.iter()).any(|(h, w)| {
        // If we want a bit that's currently 0 to become 1, we need erase
        // (h & w) != w means some bit in want is 1 but in have is 0
        (h & w) != *w
    })
}

/// Check if a block needs to be written (differs from current contents)
fn need_write(have: &[u8], want: &[u8]) -> bool {
    have != want
}

/// Statistics from a smart write operation
struct SmartWriteStats {
    /// Number of bytes that were different
    pub bytes_changed: usize,
    /// Number of erase operations performed
    pub blocks_erased: usize,
    /// Total bytes erased
    pub bytes_erased: usize,
    /// Number of write operations performed
    pub blocks_written: usize,
    /// Total bytes written
    pub bytes_written: usize,
}

/// Write flash using an opaque programmer with smart write logic
///
/// Smart write reads the current flash contents first, then only erases/writes
/// blocks that actually need to change. This is much faster and safer than
/// erasing the entire flash.
pub fn run_write_opaque(
    master: &mut dyn OpaqueMaster,
    input: &Path,
    verify: bool,
    layout_args: &LayoutArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // Determine flash size
    let flash_size = get_flash_size(master)?;

    println!(
        "Flash size: {} bytes ({} MiB)",
        flash_size,
        flash_size / (1024 * 1024)
    );

    // Read input file
    let mut file = File::open(input)?;
    let mut new_data = Vec::new();
    file.read_to_end(&mut new_data)?;

    println!("Read {} bytes from {:?}", new_data.len(), input);

    // Validate size
    if new_data.len() > flash_size as usize {
        return Err(format!(
            "File size ({} bytes) exceeds flash size ({} bytes)",
            new_data.len(),
            flash_size
        )
        .into());
    }

    // Pad to flash size if needed
    if new_data.len() < flash_size as usize {
        println!(
            "Padding file from {} to {} bytes with 0xFF",
            new_data.len(),
            flash_size
        );
        new_data.resize(flash_size as usize, 0xFF);
    }

    // Determine what to write
    let layout = if layout_args.has_layout_source() {
        // Read IFD-based layout
        let mut header = [0u8; 4096];
        master.read(0, &mut header)?;
        let mut layout = parse_ifd(&header)?;

        // Apply region filters
        if let Some(region_name) = &layout_args.region {
            layout.include_region(region_name)?;
        }
        for name in &layout_args.include {
            layout.include_region(name)?;
        }
        for name in &layout_args.exclude {
            layout.exclude_region(name)?;
        }
        if !layout.has_included_regions() {
            layout.include_all();
        }

        Some(layout)
    } else {
        None
    };

    // Perform smart write
    let stats = if let Some(layout) = &layout {
        smart_write_regions(master, layout, &new_data)?
    } else {
        smart_write_full(master, flash_size, &new_data)?
    };

    // Print statistics
    if stats.bytes_changed == 0 {
        println!("Flash already contains the desired data - no changes needed");
    } else {
        println!(
            "Smart write: {} bytes changed, {} blocks erased ({} bytes), {} blocks written ({} bytes)",
            stats.bytes_changed,
            stats.blocks_erased,
            stats.bytes_erased,
            stats.blocks_written,
            stats.bytes_written
        );
    }

    // Verify if requested (but skip if no changes were made)
    if verify && stats.bytes_changed > 0 {
        verify_flash(master, flash_size, &new_data, layout.as_ref())?;
    } else if verify {
        println!("Skipping verification - no changes were made");
    }

    println!("Write complete!");

    Ok(())
}

/// Perform smart write on full flash
fn smart_write_full(
    master: &mut dyn OpaqueMaster,
    flash_size: u32,
    new_data: &[u8],
) -> Result<SmartWriteStats, Box<dyn std::error::Error>> {
    let total_blocks = (flash_size as usize).div_ceil(ERASE_BLOCK_SIZE);

    // Phase 1: Read current flash contents
    println!("Reading current flash contents...");
    let read_pb = ProgressBar::new(flash_size as u64);
    read_pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta}) Reading")?
            .progress_chars("#>-"),
    );

    let mut current_data = vec![0u8; flash_size as usize];
    let mut offset = 0u32;
    while offset < flash_size {
        let chunk_len = std::cmp::min(CHUNK_SIZE, (flash_size - offset) as usize);
        master.read(
            offset,
            &mut current_data[offset as usize..offset as usize + chunk_len],
        )?;
        offset += chunk_len as u32;
        read_pb.set_position(offset as u64);
    }
    read_pb.finish_with_message("Read complete");

    // Phase 2: Analyze blocks and plan operations
    let mut blocks_to_erase = Vec::new();
    let mut blocks_to_write = Vec::new();
    let mut bytes_changed = 0usize;

    for block_idx in 0..total_blocks {
        let block_start = block_idx * ERASE_BLOCK_SIZE;
        let block_end = std::cmp::min(block_start + ERASE_BLOCK_SIZE, flash_size as usize);

        let current_block = &current_data[block_start..block_end];
        let new_block = &new_data[block_start..block_end];

        if need_write(current_block, new_block) {
            bytes_changed += current_block
                .iter()
                .zip(new_block.iter())
                .filter(|(c, n)| c != n)
                .count();

            if need_erase(current_block, new_block) {
                blocks_to_erase.push(block_idx);
            }
            blocks_to_write.push(block_idx);
        }
    }

    if blocks_to_write.is_empty() {
        return Ok(SmartWriteStats {
            bytes_changed: 0,
            blocks_erased: 0,
            bytes_erased: 0,
            blocks_written: 0,
            bytes_written: 0,
        });
    }

    println!(
        "Analysis: {} of {} blocks need updates ({} need erase)",
        blocks_to_write.len(),
        total_blocks,
        blocks_to_erase.len()
    );

    // Phase 3: Erase blocks that need it
    let mut blocks_erased = 0;
    let mut bytes_erased = 0;

    if !blocks_to_erase.is_empty() {
        let erase_pb = ProgressBar::new(blocks_to_erase.len() as u64);
        erase_pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({eta}) Erasing")?
                .progress_chars("#>-"),
        );
        erase_pb.enable_steady_tick(Duration::from_millis(100));

        for &block_idx in &blocks_to_erase {
            let block_start = (block_idx * ERASE_BLOCK_SIZE) as u32;
            master.erase(block_start, ERASE_BLOCK_SIZE as u32)?;
            blocks_erased += 1;
            bytes_erased += ERASE_BLOCK_SIZE;
            erase_pb.set_position(blocks_erased as u64);
        }
        erase_pb.finish_with_message("Erase complete");
    }

    // Phase 4: Write blocks that differ
    let write_pb = ProgressBar::new(blocks_to_write.len() as u64);
    write_pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({eta}) Writing")?
            .progress_chars("#>-"),
    );

    let mut blocks_written = 0;
    let mut bytes_written = 0;

    for &block_idx in &blocks_to_write {
        let block_start = block_idx * ERASE_BLOCK_SIZE;
        let block_end = std::cmp::min(block_start + ERASE_BLOCK_SIZE, flash_size as usize);
        let block_len = block_end - block_start;

        master.write(block_start as u32, &new_data[block_start..block_end])?;
        blocks_written += 1;
        bytes_written += block_len;
        write_pb.set_position(blocks_written as u64);
    }
    write_pb.finish_with_message("Write complete");

    Ok(SmartWriteStats {
        bytes_changed,
        blocks_erased,
        bytes_erased,
        blocks_written,
        bytes_written,
    })
}

/// Perform smart write on specific regions from layout
fn smart_write_regions(
    master: &mut dyn OpaqueMaster,
    layout: &rflasher_core::layout::Layout,
    new_data: &[u8],
) -> Result<SmartWriteStats, Box<dyn std::error::Error>> {
    let included: Vec<_> = layout.included_regions().collect();
    let total_region_bytes: usize = included.iter().map(|r| r.size() as usize).sum();

    // Phase 1: Read current contents of included regions
    println!(
        "Reading current flash contents for {} region(s)...",
        included.len()
    );
    let read_pb = ProgressBar::new(total_region_bytes as u64);
    read_pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta}) Reading")?
            .progress_chars("#>-"),
    );

    // We need to read the full flash image to compare
    let max_addr = included.iter().map(|r| r.end + 1).max().unwrap_or(0) as usize;
    let mut current_data = vec![0xFFu8; max_addr];

    let mut bytes_read = 0usize;
    for region in &included {
        let mut offset = region.start;
        while offset <= region.end {
            let chunk_len = std::cmp::min(CHUNK_SIZE, (region.end - offset + 1) as usize);
            master.read(
                offset,
                &mut current_data[offset as usize..offset as usize + chunk_len],
            )?;
            offset += chunk_len as u32;
            bytes_read += chunk_len;
            read_pb.set_position(bytes_read as u64);
        }
    }
    read_pb.finish_with_message("Read complete");

    // Phase 2: Analyze blocks within regions
    let mut blocks_to_erase = Vec::new();
    let mut blocks_to_write = Vec::new();
    let mut bytes_changed = 0usize;

    for region in &included {
        // Calculate block indices for this region
        let first_block = region.start as usize / ERASE_BLOCK_SIZE;
        let last_block = region.end as usize / ERASE_BLOCK_SIZE;

        for block_idx in first_block..=last_block {
            // Calculate the intersection of this block with the region
            let block_start = block_idx * ERASE_BLOCK_SIZE;
            let block_end = block_start + ERASE_BLOCK_SIZE;

            let overlap_start = std::cmp::max(block_start, region.start as usize);
            let overlap_end = std::cmp::min(block_end, region.end as usize + 1);

            if overlap_start >= overlap_end {
                continue;
            }

            let current_slice = &current_data[overlap_start..overlap_end];
            let new_slice = &new_data[overlap_start..overlap_end];

            if need_write(current_slice, new_slice) {
                bytes_changed += current_slice
                    .iter()
                    .zip(new_slice.iter())
                    .filter(|(c, n)| c != n)
                    .count();

                // Check if we already have this block
                if !blocks_to_write.contains(&block_idx) {
                    // For erase decision, we need to check the full block overlap with region
                    let current_block = &current_data[overlap_start..overlap_end];
                    let new_block = &new_data[overlap_start..overlap_end];

                    if need_erase(current_block, new_block) && !blocks_to_erase.contains(&block_idx)
                    {
                        blocks_to_erase.push(block_idx);
                    }
                    blocks_to_write.push(block_idx);
                }
            }
        }
    }

    if blocks_to_write.is_empty() {
        return Ok(SmartWriteStats {
            bytes_changed: 0,
            blocks_erased: 0,
            bytes_erased: 0,
            blocks_written: 0,
            bytes_written: 0,
        });
    }

    let total_blocks: usize = included
        .iter()
        .map(|r| {
            let first = r.start as usize / ERASE_BLOCK_SIZE;
            let last = r.end as usize / ERASE_BLOCK_SIZE;
            last - first + 1
        })
        .sum();

    println!(
        "Analysis: {} of {} blocks need updates ({} need erase)",
        blocks_to_write.len(),
        total_blocks,
        blocks_to_erase.len()
    );

    // Phase 3: Erase blocks that need it
    let mut blocks_erased = 0;
    let mut bytes_erased = 0;

    if !blocks_to_erase.is_empty() {
        let erase_pb = ProgressBar::new(blocks_to_erase.len() as u64);
        erase_pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({eta}) Erasing")?
                .progress_chars("#>-"),
        );
        erase_pb.enable_steady_tick(Duration::from_millis(100));

        for &block_idx in &blocks_to_erase {
            let block_start = (block_idx * ERASE_BLOCK_SIZE) as u32;
            master.erase(block_start, ERASE_BLOCK_SIZE as u32)?;
            blocks_erased += 1;
            bytes_erased += ERASE_BLOCK_SIZE;
            erase_pb.set_position(blocks_erased as u64);
        }
        erase_pb.finish_with_message("Erase complete");
    }

    // Phase 4: Write blocks that differ (only the parts within regions)
    let write_pb = ProgressBar::new(blocks_to_write.len() as u64);
    write_pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} blocks ({eta}) Writing")?
            .progress_chars("#>-"),
    );

    let mut blocks_written = 0;
    let mut bytes_written = 0;

    for &block_idx in &blocks_to_write {
        let block_start = block_idx * ERASE_BLOCK_SIZE;
        let block_end = block_start + ERASE_BLOCK_SIZE;

        // Write the portion of this block that falls within included regions
        for region in &included {
            let overlap_start = std::cmp::max(block_start, region.start as usize);
            let overlap_end = std::cmp::min(block_end, region.end as usize + 1);

            if overlap_start < overlap_end {
                master.write(overlap_start as u32, &new_data[overlap_start..overlap_end])?;
                bytes_written += overlap_end - overlap_start;
            }
        }

        blocks_written += 1;
        write_pb.set_position(blocks_written as u64);
    }
    write_pb.finish_with_message("Write complete");

    Ok(SmartWriteStats {
        bytes_changed,
        blocks_erased,
        bytes_erased,
        blocks_written,
        bytes_written,
    })
}

/// Verify flash contents against expected data
fn verify_flash(
    master: &mut dyn OpaqueMaster,
    flash_size: u32,
    expected: &[u8],
    layout: Option<&rflasher_core::layout::Layout>,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Verifying...");

    let verify_len = if let Some(layout) = layout {
        layout.included_regions().map(|r| r.size() as u64).sum()
    } else {
        flash_size as u64
    };

    let verify_pb = ProgressBar::new(verify_len);
    verify_pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta}) Verifying")?
            .progress_chars("#>-"),
    );

    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut bytes_verified = 0u64;

    if let Some(layout) = layout {
        // Verify only included regions
        for region in layout.included_regions() {
            let mut offset = region.start;
            while offset <= region.end {
                let chunk_len = std::cmp::min(CHUNK_SIZE, (region.end - offset + 1) as usize);
                let chunk_buf = &mut buf[..chunk_len];
                master.read(offset, chunk_buf)?;

                let expected_chunk = &expected[offset as usize..offset as usize + chunk_len];
                if chunk_buf != expected_chunk {
                    verify_pb.abandon_with_message("Verification failed!");
                    for (i, (a, b)) in chunk_buf.iter().zip(expected_chunk.iter()).enumerate() {
                        if a != b {
                            return Err(format!(
                                "Verification failed in region '{}' at offset 0x{:08X}: expected 0x{:02X}, got 0x{:02X}",
                                region.name,
                                offset as usize + i,
                                b,
                                a
                            )
                            .into());
                        }
                    }
                }

                offset += chunk_len as u32;
                bytes_verified += chunk_len as u64;
                verify_pb.set_position(bytes_verified);
            }
        }
    } else {
        // Verify entire flash
        let mut offset = 0u32;
        while offset < flash_size {
            let chunk_len = std::cmp::min(CHUNK_SIZE, (flash_size - offset) as usize);
            let chunk_buf = &mut buf[..chunk_len];
            master.read(offset, chunk_buf)?;

            let expected_chunk = &expected[offset as usize..offset as usize + chunk_len];
            if chunk_buf != expected_chunk {
                verify_pb.abandon_with_message("Verification failed!");
                for (i, (a, b)) in chunk_buf.iter().zip(expected_chunk.iter()).enumerate() {
                    if a != b {
                        return Err(format!(
                            "Verification failed at offset 0x{:08X}: expected 0x{:02X}, got 0x{:02X}",
                            offset as usize + i,
                            b,
                            a
                        )
                        .into());
                    }
                }
            }

            offset += chunk_len as u32;
            bytes_verified += chunk_len as u64;
            verify_pb.set_position(bytes_verified);
        }
    }

    verify_pb.finish_with_message("Verification passed");
    Ok(())
}

/// Erase flash using an opaque programmer
pub fn run_erase_opaque(
    master: &mut dyn OpaqueMaster,
    start: Option<u32>,
    length: Option<u32>,
    layout_args: &LayoutArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    // Determine flash size
    let flash_size = get_flash_size(master)?;

    println!(
        "Flash size: {} bytes ({} MiB)",
        flash_size,
        flash_size / (1024 * 1024)
    );

    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template("{spinner:.green} {msg}")?);
    pb.enable_steady_tick(std::time::Duration::from_millis(100));

    if layout_args.has_layout_source() || layout_args.has_region_filter() {
        // Erase by layout
        let mut header = [0u8; 4096];
        master.read(0, &mut header)?;
        let mut layout = parse_ifd(&header)?;

        // Apply region filters
        if let Some(region_name) = &layout_args.region {
            layout.include_region(region_name)?;
        }
        for name in &layout_args.include {
            layout.include_region(name)?;
        }
        for name in &layout_args.exclude {
            layout.exclude_region(name)?;
        }
        if !layout.has_included_regions() {
            layout.include_all();
        }

        let included: Vec<_> = layout.included_regions().collect();
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

        for region in included {
            pb.set_message(format!("Erasing {}...", region.name));
            master.erase(region.start, region.size())?;
        }
    } else if let (Some(start_addr), Some(len)) = (start, length) {
        // Partial erase
        pb.set_message(format!("Erasing {} bytes at 0x{:08X}...", len, start_addr));
        master.erase(start_addr, len)?;
        println!("Erased {} bytes starting at 0x{:08X}", len, start_addr);
    } else if start.is_some() || length.is_some() {
        return Err("Both --start and --length must be specified for partial erase".into());
    } else {
        // Full chip erase
        pb.set_message(format!("Erasing {} bytes...", flash_size));
        master.erase(0, flash_size)?;
    }

    pb.finish_with_message("Erase complete");

    Ok(())
}

/// Verify flash using an opaque programmer
pub fn run_verify_opaque(
    master: &mut dyn OpaqueMaster,
    input: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Determine flash size
    let flash_size = get_flash_size(master)?;

    println!(
        "Flash size: {} bytes ({} MiB)",
        flash_size,
        flash_size / (1024 * 1024)
    );

    // Read input file
    let mut file = File::open(input)?;
    let mut expected = Vec::new();
    file.read_to_end(&mut expected)?;

    println!("Read {} bytes from {:?}", expected.len(), input);

    // Validate size
    if expected.len() > flash_size as usize {
        return Err(format!(
            "File size ({} bytes) exceeds flash size ({} bytes)",
            expected.len(),
            flash_size
        )
        .into());
    }

    // Verify with progress
    let pb = ProgressBar::new(expected.len() as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta}) Verifying")?
            .progress_chars("#>-"),
    );

    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut offset = 0u32;
    let verify_len = expected.len() as u32;

    while offset < verify_len {
        let chunk_len = std::cmp::min(CHUNK_SIZE, (verify_len - offset) as usize);
        let chunk_buf = &mut buf[..chunk_len];
        master.read(offset, chunk_buf)?;

        let expected_chunk = &expected[offset as usize..offset as usize + chunk_len];
        if chunk_buf != expected_chunk {
            pb.abandon_with_message("Verification failed!");
            for (i, (a, b)) in chunk_buf.iter().zip(expected_chunk.iter()).enumerate() {
                if a != b {
                    return Err(format!(
                        "Verification failed at offset 0x{:08X}: expected 0x{:02X}, got 0x{:02X}",
                        offset as usize + i,
                        b,
                        a
                    )
                    .into());
                }
            }
        }

        offset += chunk_len as u32;
        pb.set_position(offset as u64);
    }

    pb.finish_with_message("Verification passed");
    println!("Verification passed!");

    Ok(())
}

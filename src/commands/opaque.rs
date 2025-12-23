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

/// Default chunk size for operations
const CHUNK_SIZE: usize = 4096;

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

/// Write flash using an opaque programmer
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
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;

    println!("Read {} bytes from {:?}", data.len(), input);

    // Validate size
    if data.len() > flash_size as usize {
        return Err(format!(
            "File size ({} bytes) exceeds flash size ({} bytes)",
            data.len(),
            flash_size
        )
        .into());
    }

    // Pad to flash size if needed
    if data.len() < flash_size as usize {
        println!(
            "Padding file from {} to {} bytes with 0xFF",
            data.len(),
            flash_size
        );
        data.resize(flash_size as usize, 0xFF);
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

    // Erase before writing
    println!("Erasing...");
    let erase_pb = ProgressBar::new_spinner();
    erase_pb.set_style(ProgressStyle::default_spinner().template("{spinner:.green} {msg}")?);
    erase_pb.set_message("Erasing flash...");
    erase_pb.enable_steady_tick(std::time::Duration::from_millis(100));

    if let Some(layout) = &layout {
        // Erase only included regions
        for region in layout.included_regions() {
            master.erase(region.start, region.size())?;
        }
    } else {
        // Erase entire flash
        master.erase(0, flash_size)?;
    }

    erase_pb.finish_with_message("Erase complete");

    // Write with progress
    println!("Writing...");
    let pb = ProgressBar::new(flash_size as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})")?
            .progress_chars("#>-"),
    );

    if let Some(layout) = &layout {
        // Write only included regions
        let included: Vec<_> = layout.included_regions().collect();
        let total_bytes: usize = included.iter().map(|r| r.size() as usize).sum();
        pb.set_length(total_bytes as u64);

        let mut bytes_written = 0usize;
        for region in included {
            let mut offset = region.start;
            while offset <= region.end {
                let chunk_len = std::cmp::min(CHUNK_SIZE, (region.end - offset + 1) as usize);
                let chunk = &data[offset as usize..offset as usize + chunk_len];
                master.write(offset, chunk)?;
                offset += chunk_len as u32;
                bytes_written += chunk_len;
                pb.set_position(bytes_written as u64);
            }
        }
    } else {
        // Write entire flash
        let mut offset = 0u32;
        while offset < flash_size {
            let chunk_len = std::cmp::min(CHUNK_SIZE, (flash_size - offset) as usize);
            let chunk = &data[offset as usize..offset as usize + chunk_len];
            master.write(offset, chunk)?;
            offset += chunk_len as u32;
            pb.set_position(offset as u64);
        }
    }

    pb.finish_with_message("Write complete");

    // Verify if requested
    if verify {
        println!("Verifying...");
        let verify_pb = ProgressBar::new(flash_size as u64);
        verify_pb.set_style(
            ProgressStyle::default_bar()
                .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta}) Verifying")?
                .progress_chars("#>-"),
        );

        let mut buf = vec![0u8; CHUNK_SIZE];
        let mut offset = 0u32;

        while offset < flash_size {
            let chunk_len = std::cmp::min(CHUNK_SIZE, (flash_size - offset) as usize);
            let chunk_buf = &mut buf[..chunk_len];
            master.read(offset, chunk_buf)?;

            let expected = &data[offset as usize..offset as usize + chunk_len];
            if chunk_buf != expected {
                verify_pb.abandon_with_message("Verification failed!");
                for (i, (a, b)) in chunk_buf.iter().zip(expected.iter()).enumerate() {
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
            verify_pb.set_position(offset as u64);
        }

        verify_pb.finish_with_message("Verification passed");
    }

    println!("Write complete!");

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

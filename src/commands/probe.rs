//! Probe command implementation

use rflasher_core::chip::ChipDatabase;
use rflasher_core::flash;
use rflasher_core::layout::parse_ifd;
use rflasher_core::programmer::{OpaqueMaster, SpiMaster};

/// Probe a SPI-based programmer using JEDEC ID
#[allow(dead_code)]
pub fn run_probe<M: SpiMaster + ?Sized>(
    master: &mut M,
    db: &ChipDatabase,
) -> Result<(), Box<dyn std::error::Error>> {
    match flash::probe(master, db) {
        Ok(ctx) => {
            println!("Found flash chip:");
            println!("  Vendor: {}", ctx.chip.vendor);
            println!("  Name:   {}", ctx.chip.name);
            println!(
                "  Size:   {} bytes ({} KiB)",
                ctx.chip.total_size,
                ctx.chip.total_size / 1024
            );
            println!(
                "  JEDEC ID: {:02X} {:04X}",
                ctx.chip.jedec_manufacturer, ctx.chip.jedec_device
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("Probe failed: {}", e);
            Err(Box::new(e))
        }
    }
}

/// Probe an opaque programmer (e.g., Intel internal)
///
/// For opaque programmers, we can't use JEDEC ID probing. Instead, we
/// read the Intel Flash Descriptor to determine flash layout and size.
#[allow(dead_code)]
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

//! Probe command implementation

use rflasher_core::flash;
use rflasher_core::programmer::SpiMaster;

pub fn run_probe<M: SpiMaster>(master: &mut M) -> Result<(), Box<dyn std::error::Error>> {
    match flash::probe(master) {
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

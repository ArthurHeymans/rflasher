//! List commands implementation

use rflasher_core::chip::ChipDatabase;

/// List all supported programmers
pub fn list_programmers() {
    println!("Supported programmers:");
    println!();
    println!("  dummy     - In-memory flash emulator for testing");
    println!("  ch341a    - CH341A USB SPI programmer (VID:1a86 PID:5512)");
    println!("  serprog   - Serial Flasher Protocol (not yet implemented)");
    println!("  ftdi      - FTDI MPSSE programmer (not yet implemented)");
    println!("  internal  - Intel chipset internal flash (not yet implemented)");
    println!("  linux_spi - Linux spidev (not yet implemented)");
    println!();
    println!("Usage: rflasher <command> -p <programmer>");
    println!();
    println!("Examples:");
    println!("  rflasher probe -p ch341a");
    println!("  rflasher read -p ch341a -o flash.bin");
    println!("  rflasher write -p ch341a -i flash.bin");
}

/// List all supported chips from the database
pub fn list_chips(db: &ChipDatabase, vendor_filter: Option<&str>) {
    println!("Supported flash chips ({} total):", db.len());
    println!();
    println!(
        "{:<12} {:<20} {:>10} {:>10}",
        "Vendor", "Name", "Size", "JEDEC ID"
    );
    println!("{}", "-".repeat(60));

    for chip in db.iter() {
        // Apply vendor filter if specified
        if let Some(vendor) = vendor_filter {
            if !chip.vendor.to_lowercase().contains(&vendor.to_lowercase()) {
                continue;
            }
        }

        let size_str = format_size(chip.total_size);
        let jedec_str = format!("{:02X} {:04X}", chip.jedec_manufacturer, chip.jedec_device);

        println!(
            "{:<12} {:<20} {:>10} {:>10}",
            chip.vendor, chip.name, size_str, jedec_str
        );
    }
}

fn format_size(bytes: u32) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MiB", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{} B", bytes)
    }
}

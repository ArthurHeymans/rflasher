//! List commands implementation

use rflasher_core::chip::ChipDatabase;
use rflasher_flash::available_programmers;

/// List all supported programmers
pub fn list_programmers() {
    let progs = available_programmers();

    println!("Supported programmers ({} enabled):", progs.len());
    println!();

    for p in &progs {
        print!("  {:12} - {}", p.name, p.description);
        if !p.aliases.is_empty() {
            print!(" (aliases: {})", p.aliases.join(", "));
        }
        println!();
    }

    println!();
    println!("Usage: rflasher <command> -p <programmer>");
    println!();
    println!("Examples:");
    println!("  rflasher probe -p ch341a");
    println!("  rflasher read -p ch341a -o flash.bin");
    println!("  rflasher write -p ch341a -i flash.bin");

    // Show which programmers are available vs compiled out
    #[cfg(not(all(
        feature = "dummy",
        feature = "ch341a",
        feature = "serprog",
        feature = "ftdi",
        feature = "linux-spi",
        feature = "internal"
    )))]
    {
        println!();
        println!("Note: Some programmers may be disabled at compile time.");
        println!("Rebuild with --features all-programmers to enable all.");
    }
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

        let size_str = super::format_size(chip.total_size);
        let jedec_str = format!("{:02X} {:04X}", chip.jedec_manufacturer, chip.jedec_device);

        println!(
            "{:<12} {:<20} {:>10} {:>10}",
            chip.vendor, chip.name, size_str, jedec_str
        );
    }
}

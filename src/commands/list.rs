//! List commands implementation

use rflasher_chips::ChipDatabase;
use rflasher_programmers::available_programmers;

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
        let options = p.options_syntax();
        if !options.is_empty() {
            println!("  {:12}   options: {}", "", options);
        }
    }

    println!();
    println!("Usage: rflasher <command> -p <programmer>");
    println!();
    println!("Examples:");
    println!("  rflasher probe -p ch341a");
    println!("  rflasher read -p ch341a flash.bin");
    println!("  rflasher write -p ch341a flash.bin");

    // Show which programmers are available vs compiled out
    #[cfg(not(all(
        feature = "ati-spi",
        feature = "dummy",
        feature = "ch341a",
        feature = "ch347",
        feature = "dediprog",
        feature = "serprog",
        feature = "ftdi",
        feature = "ft4222",
        feature = "linux-spi",
        feature = "linux-mtd",
        feature = "linux-gpio",
        feature = "internal",
        feature = "raiden",
        feature = "sunxi-fel"
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

    let vendor_filter_lower = vendor_filter.map(str::to_lowercase);
    for chip in db.iter() {
        // Apply vendor filter if specified
        if let Some(ref vendor) = vendor_filter_lower
            && !chip.vendor.to_lowercase().contains(vendor.as_str())
        {
            continue;
        }

        let size_str = super::format_size(chip.total_size);
        let jedec_str = format!("{:02X} {:04X}", chip.jedec_manufacturer, chip.jedec_device);

        println!(
            "{:<12} {:<20} {:>10} {:>10}",
            chip.vendor, chip.name, size_str, jedec_str
        );
    }
}

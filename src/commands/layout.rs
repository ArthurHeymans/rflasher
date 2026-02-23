//! Layout command implementations

use rflasher_core::layout::{has_fmap, has_ifd, Layout, LayoutSource};
use std::fs;
use std::path::Path;

/// Show layout from a file
pub fn cmd_show(file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let layout = Layout::from_toml_file(file)?;
    print_layout(&layout);
    Ok(())
}

/// Extract layout from flash image (auto-detect IFD or FMAP)
pub fn cmd_extract(input: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let data = fs::read(input)?;

    let layout = if has_ifd(&data) {
        println!("Detected Intel Flash Descriptor");
        Layout::from_ifd(&data)?
    } else if has_fmap(&data) {
        println!("Detected FMAP");
        Layout::from_fmap(&data)?
    } else {
        return Err("No IFD or FMAP found in image".into());
    };

    print_layout(&layout);

    layout.to_toml_file(output)?;
    println!("\nSaved layout to {:?}", output);

    Ok(())
}

/// Extract IFD layout from image
pub fn cmd_ifd(input: &Path, output: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    let data = fs::read(input)?;

    if !has_ifd(&data) {
        return Err("No Intel Flash Descriptor found in image".into());
    }

    let layout = Layout::from_ifd(&data)?;
    print_layout(&layout);

    if let Some(out) = output {
        layout.to_toml_file(out)?;
        println!("\nSaved layout to {:?}", out);
    } else {
        println!("\n--- TOML Output ---\n");
        println!("{}", layout.to_toml_string()?);
    }

    Ok(())
}

/// Extract FMAP layout from image
pub fn cmd_fmap(input: &Path, output: Option<&Path>) -> Result<(), Box<dyn std::error::Error>> {
    use rflasher_core::layout::search_fmap;

    let data = fs::read(input)?;

    // Use the generic search algorithm on the file buffer
    let layout = search_fmap(&mut data.as_slice())?;
    print_layout(&layout);

    if let Some(out) = output {
        layout.to_toml_file(out)?;
        println!("\nSaved layout to {:?}", out);
    } else {
        println!("\n--- TOML Output ---\n");
        println!("{}", layout.to_toml_string()?);
    }

    Ok(())
}

/// Create a new layout file template
pub fn cmd_create(output: &Path, size: &str) -> Result<(), Box<dyn std::error::Error>> {
    let chip_size = parse_size(size)?;

    let mut layout = Layout::new();
    layout.name = Some("New Layout".to_string());
    layout.chip_size = Some(chip_size);
    layout.source = LayoutSource::Manual;

    // Add a single region covering the whole chip
    use rflasher_core::layout::Region;
    layout.add_region(Region::new("firmware", 0, chip_size - 1));

    layout.to_toml_file(output)?;
    println!("Created layout template at {:?}", output);
    println!("Edit the file to define your regions.");

    Ok(())
}

/// Parse a size string like "16 MiB" or "0x1000000"
fn parse_size(s: &str) -> Result<u32, String> {
    let s = s.trim();

    // Try plain number first
    if let Ok(n) = s.parse::<u32>() {
        return Ok(n);
    }

    // Try hex
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        if let Ok(n) = u32::from_str_radix(hex.trim(), 16) {
            return Ok(n);
        }
    }

    // Try with suffix
    let s_lower = s.to_lowercase();
    let (num_str, multiplier) = if let Some(n) = s_lower.strip_suffix("mib") {
        (n.trim(), 1024 * 1024)
    } else if let Some(n) = s_lower.strip_suffix("mb") {
        (n.trim(), 1024 * 1024)
    } else if let Some(n) = s_lower.strip_suffix("kib") {
        (n.trim(), 1024)
    } else if let Some(n) = s_lower.strip_suffix("kb") {
        (n.trim(), 1024)
    } else if let Some(n) = s_lower.strip_suffix('b') {
        (n.trim(), 1)
    } else {
        return Err(format!("invalid size: {}", s));
    };

    let num: u32 = num_str
        .parse()
        .map_err(|_| format!("invalid size: {}", s))?;
    Ok(num * multiplier)
}

/// Print layout information
pub fn print_layout(layout: &Layout) {
    println!("Layout Information");
    println!("==================");

    if let Some(name) = &layout.name {
        println!("Name:   {}", name);
    }

    println!(
        "Source: {}",
        match layout.source {
            LayoutSource::Toml => "TOML file",
            LayoutSource::Ifd => "Intel Flash Descriptor",
            LayoutSource::Fmap => "FMAP",
            LayoutSource::Manual => "Manual",
        }
    );

    if let Some(size) = layout.chip_size {
        println!("Chip:   {} bytes ({})", size, super::format_size(size));
    }

    println!("\nRegions ({}):", layout.len());
    println!(
        "{:<20} {:>10} {:>10} {:>10} {:>8} {:>8}",
        "Name", "Start", "End", "Size", "RO", "Danger"
    );
    println!("{:-<74}", "");

    for region in &layout.regions {
        let size = region.size();
        let size_str = super::format_size(size);

        println!(
            "{:<20} {:#010X} {:#010X} {:>10} {:>8} {:>8}",
            region.name,
            region.start,
            region.end,
            size_str,
            if region.readonly { "yes" } else { "-" },
            if region.dangerous { "yes" } else { "-" }
        );
    }
}

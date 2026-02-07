//! Write protection command implementations

use rflasher_core::wp::{WpMode, WpRange, WriteOptions};
use rflasher_flash::FlashHandle;
use std::error::Error;

/// Format a range as a human-readable string with fraction of chip
fn format_range(range: &WpRange, total_size: u32) -> String {
    if range.len == 0 {
        return "none".to_string();
    }
    if range.len == total_size {
        return "all".to_string();
    }

    // Calculate the fraction
    let fraction = range.len as f64 / total_size as f64;

    // Try common fractions
    let fraction_str = if (fraction - 0.5).abs() < 0.001 {
        "1/2".to_string()
    } else if (fraction - 0.25).abs() < 0.001 {
        "1/4".to_string()
    } else if (fraction - 0.125).abs() < 0.001 {
        "1/8".to_string()
    } else if (fraction - 0.0625).abs() < 0.001 {
        "1/16".to_string()
    } else if (fraction - 0.03125).abs() < 0.001 {
        "1/32".to_string()
    } else if (fraction - 0.75).abs() < 0.001 {
        "3/4".to_string()
    } else if (fraction - 0.875).abs() < 0.001 {
        "7/8".to_string()
    } else {
        format!("{:.1}%", fraction * 100.0)
    };

    // Determine if it's upper or lower
    let position = if range.start == 0 {
        "lower"
    } else if range.start + range.len == total_size {
        "upper"
    } else {
        "middle"
    };

    format!("{} {}", position, fraction_str)
}

/// Format a protection mode for display
fn format_mode(mode: WpMode) -> &'static str {
    match mode {
        WpMode::Disabled => "disabled",
        WpMode::Hardware => "hardware",
        WpMode::PowerCycle => "power_cycle",
        WpMode::Permanent => "permanent",
    }
}

/// Parse a range specification like "0,0x100000" or "0x10000,65536"
fn parse_range(spec: &str) -> Result<WpRange, Box<dyn Error>> {
    let parts: Vec<&str> = spec.split(',').collect();
    if parts.len() != 2 {
        return Err(format!(
            "Invalid range format '{}'. Expected 'start,length' (e.g., '0,0x100000')",
            spec
        )
        .into());
    }

    let start = parse_number(parts[0].trim())?;
    let len = parse_number(parts[1].trim())?;

    Ok(WpRange::new(start, len))
}

/// Parse a number that may be decimal or hex
fn parse_number(s: &str) -> Result<u32, Box<dyn Error>> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16)
            .map_err(|e| format!("Invalid hex number '{}': {}", s, e).into())
    } else {
        s.parse::<u32>()
            .map_err(|e| format!("Invalid number '{}': {}", s, e).into())
    }
}

/// Show current write protection status
pub fn cmd_status(handle: &mut FlashHandle) -> Result<(), Box<dyn Error>> {
    if !handle.wp_supported() {
        return Err("Write protection operations are not supported for this chip".into());
    }

    let config = handle
        .read_wp_config()
        .map_err(|e| format!("Failed to read WP config: {}", e))?;
    let total_size = handle.size();

    println!(
        "Protection range: start=0x{:08x} length=0x{:08x} ({})",
        config.range.start,
        config.range.len,
        format_range(&config.range, total_size)
    );
    println!("Protection mode: {}", format_mode(config.mode));

    Ok(())
}

/// List available protection ranges
pub fn cmd_list(handle: &mut FlashHandle) -> Result<(), Box<dyn Error>> {
    if !handle.wp_supported() {
        return Err("Write protection operations are not supported for this chip".into());
    }

    let ranges = handle.get_available_wp_ranges();
    let total_size = handle.size();

    if ranges.is_empty() {
        println!("No protection ranges available.");
        return Ok(());
    }

    println!("Available protection ranges:");
    for range in &ranges {
        println!(
            "    start=0x{:08x} length=0x{:08x} ({})",
            range.start,
            range.len,
            format_range(range, total_size)
        );
    }

    Ok(())
}

/// Enable hardware write protection
pub fn cmd_enable(handle: &mut FlashHandle, temporary: bool) -> Result<(), Box<dyn Error>> {
    if !handle.wp_supported() {
        return Err("Write protection operations are not supported for this chip".into());
    }

    let options = WriteOptions {
        volatile: temporary,
    };

    handle
        .set_wp_mode(WpMode::Hardware, options)
        .map_err(|e| format!("Failed to enable write protection: {}", e))?;

    println!(
        "Hardware write protection enabled{}.",
        if temporary { " (temporary)" } else { "" }
    );
    Ok(())
}

/// Disable write protection
pub fn cmd_disable(handle: &mut FlashHandle, temporary: bool) -> Result<(), Box<dyn Error>> {
    if !handle.wp_supported() {
        return Err("Write protection operations are not supported for this chip".into());
    }

    let options = WriteOptions {
        volatile: temporary,
    };

    handle
        .disable_wp(options)
        .map_err(|e| format!("Failed to disable write protection: {}", e))?;

    println!(
        "Write protection disabled{}.",
        if temporary { " (temporary)" } else { "" }
    );
    Ok(())
}

/// Set protection range
pub fn cmd_range(
    handle: &mut FlashHandle,
    range_spec: &str,
    temporary: bool,
) -> Result<(), Box<dyn Error>> {
    if !handle.wp_supported() {
        return Err("Write protection operations are not supported for this chip".into());
    }

    let range = parse_range(range_spec)?;
    let options = WriteOptions {
        volatile: temporary,
    };
    let total_size = handle.size();

    // Validate range doesn't exceed chip size
    if range.start + range.len > total_size {
        return Err(format!(
            "Range 0x{:x},0x{:x} exceeds chip size (0x{:x} bytes)",
            range.start, range.len, total_size
        )
        .into());
    }

    handle
        .set_wp_range(&range, options)
        .map_err(|e| format!("Failed to set protection range: {}", e))?;

    println!(
        "Protection range set to start=0x{:08x} length=0x{:08x} ({}){}.",
        range.start,
        range.len,
        format_range(&range, total_size),
        if temporary { " (temporary)" } else { "" }
    );
    Ok(())
}

/// Set protection by region name
pub fn cmd_region(
    handle: &mut FlashHandle,
    layout: &rflasher_core::layout::Layout,
    region_name: &str,
    temporary: bool,
) -> Result<(), Box<dyn Error>> {
    if !handle.wp_supported() {
        return Err("Write protection operations are not supported for this chip".into());
    }

    // Find the region in the layout
    let region = layout
        .regions
        .iter()
        .find(|r| r.name == region_name)
        .ok_or_else(|| format!("Region '{}' not found in layout", region_name))?;

    let range = WpRange::new(region.start, region.end - region.start + 1);
    let options = WriteOptions {
        volatile: temporary,
    };
    let total_size = handle.size();

    handle.set_wp_range(&range, options).map_err(|e| {
        format!(
            "Failed to set protection range for region '{}': {}",
            region_name, e
        )
    })?;

    println!(
        "Protection set for region '{}': start=0x{:08x} length=0x{:08x} ({}){}.",
        region_name,
        range.start,
        range.len,
        format_range(&range, total_size),
        if temporary { " (temporary)" } else { "" }
    );
    Ok(())
}

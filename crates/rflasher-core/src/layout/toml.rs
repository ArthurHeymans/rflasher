//! TOML layout file parsing
//!
//! Parses layout files in TOML format:
//!
//! ```toml
//! [layout]
//! name = "My BIOS"
//! chip_size = "16 MiB"
//!
//! [[region]]
//! name = "descriptor"
//! start = 0x000000
//! end = 0x000FFF
//! readonly = true
//!
//! [[region]]
//! name = "bios"
//! start = 0x001000
//! end = 0x7FFFFF
//! ```

use std::fs;
use std::path::Path;
use std::string::String;
use std::vec::Vec;
use std::format;

use super::{Layout, LayoutError, LayoutSource, Region};

/// TOML layout file structure
#[derive(Debug, serde::Deserialize)]
struct TomlLayoutFile {
    layout: Option<TomlLayoutMeta>,
    region: Vec<TomlRegion>,
}

/// Layout metadata
#[derive(Debug, serde::Deserialize)]
struct TomlLayoutMeta {
    name: Option<String>,
    chip_size: Option<String>,
}

/// Region definition in TOML
#[derive(Debug, serde::Deserialize)]
struct TomlRegion {
    name: String,
    #[serde(deserialize_with = "deserialize_hex_u32")]
    start: u32,
    #[serde(deserialize_with = "deserialize_hex_u32")]
    end: u32,
    #[serde(default)]
    readonly: bool,
    #[serde(default)]
    dangerous: bool,
}

/// Deserialize a u32 that can be hex (0x...) or decimal
fn deserialize_hex_u32<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;

    // Try to deserialize as a number first, then as a string
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum HexOrInt {
        Int(u32),
        Str(String),
    }

    match HexOrInt::deserialize(deserializer)? {
        HexOrInt::Int(n) => Ok(n),
        HexOrInt::Str(s) => parse_number(&s).map_err(serde::de::Error::custom),
    }
}

/// Parse a number that can be hex (0x...) or decimal
fn parse_number(s: &str) -> Result<u32, String> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| format!("invalid hex: {}", e))
    } else {
        s.parse().map_err(|e| format!("invalid number: {}", e))
    }
}

/// Parse a size string like "16 MiB" or "4096"
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
    } else if let Some(n) = s_lower.strip_suffix("b") {
        (n.trim(), 1)
    } else {
        return Err(format!("invalid size: {}", s));
    };

    let num: u32 = num_str.parse().map_err(|_| format!("invalid size: {}", s))?;
    Ok(num * multiplier)
}

impl Layout {
    /// Load a layout from a TOML file
    pub fn from_toml_file(path: impl AsRef<Path>) -> Result<Self, LayoutError> {
        let content = fs::read_to_string(path).map_err(|_| LayoutError::IoError)?;
        Self::from_toml_str(&content)
    }

    /// Parse a layout from a TOML string
    pub fn from_toml_str(content: &str) -> Result<Self, LayoutError> {
        let file: TomlLayoutFile =
            toml::from_str(content).map_err(|_| LayoutError::ParseError)?;

        let mut layout = Layout::with_source(LayoutSource::Toml);

        // Set metadata
        if let Some(meta) = file.layout {
            layout.name = meta.name;
            if let Some(size_str) = meta.chip_size {
                layout.chip_size = Some(parse_size(&size_str).map_err(|_| LayoutError::ParseError)?);
            }
        }

        // Add regions
        for toml_region in file.region {
            layout.add_region(Region {
                name: toml_region.name,
                start: toml_region.start,
                end: toml_region.end,
                readonly: toml_region.readonly,
                dangerous: toml_region.dangerous,
                included: false,
            });
        }

        layout.sort_by_address();
        Ok(layout)
    }

    /// Save layout to a TOML file
    pub fn to_toml_file(&self, path: impl AsRef<Path>) -> Result<(), LayoutError> {
        let content = self.to_toml_string()?;
        fs::write(path, content).map_err(|_| LayoutError::IoError)
    }

    /// Convert layout to TOML string
    pub fn to_toml_string(&self) -> Result<String, LayoutError> {
        let mut output = String::new();

        // Write metadata
        output.push_str("[layout]\n");
        if let Some(name) = &self.name {
            output.push_str(&format!("name = \"{}\"\n", name));
        }
        if let Some(size) = self.chip_size {
            output.push_str(&format!("chip_size = \"{}\"\n", format_size(size)));
        }
        output.push('\n');

        // Write regions
        for region in &self.regions {
            output.push_str("[[region]]\n");
            output.push_str(&format!("name = \"{}\"\n", region.name));
            output.push_str(&format!("start = 0x{:08X}\n", region.start));
            output.push_str(&format!("end = 0x{:08X}\n", region.end));
            if region.readonly {
                output.push_str("readonly = true\n");
            }
            if region.dangerous {
                output.push_str("dangerous = true\n");
            }
            output.push('\n');
        }

        Ok(output)
    }
}

/// Format a size as human-readable string
fn format_size(size: u32) -> String {
    if size >= 1024 * 1024 && size % (1024 * 1024) == 0 {
        format!("{} MiB", size / (1024 * 1024))
    } else if size >= 1024 && size % 1024 == 0 {
        format!("{} KiB", size / 1024)
    } else {
        format!("{}", size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("4096").unwrap(), 4096);
        assert_eq!(parse_size("0x1000").unwrap(), 4096);
        assert_eq!(parse_size("4 KiB").unwrap(), 4096);
        assert_eq!(parse_size("4KiB").unwrap(), 4096);
        assert_eq!(parse_size("16 MiB").unwrap(), 16 * 1024 * 1024);
        assert_eq!(parse_size("16MiB").unwrap(), 16 * 1024 * 1024);
    }

    #[test]
    fn test_parse_toml() {
        let toml = r#"
[layout]
name = "Test Layout"
chip_size = "16 MiB"

[[region]]
name = "descriptor"
start = 0x000000
end = 0x000FFF
readonly = true

[[region]]
name = "bios"
start = 0x001000
end = 0xFFFFFF
"#;
        let layout = Layout::from_toml_str(toml).unwrap();
        assert_eq!(layout.name, Some("Test Layout".to_string()));
        assert_eq!(layout.chip_size, Some(16 * 1024 * 1024));
        assert_eq!(layout.regions.len(), 2);
        assert_eq!(layout.regions[0].name, "descriptor");
        assert!(layout.regions[0].readonly);
        assert_eq!(layout.regions[1].name, "bios");
        assert!(!layout.regions[1].readonly);
    }
}

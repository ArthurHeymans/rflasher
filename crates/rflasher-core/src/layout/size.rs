//! Checked parsing shared by layout files and the CLI.

/// Parse a byte count, hexadecimal address, or binary byte unit.
/// Historical `KB`/`MB` spellings are aliases for KiB/MiB.
pub fn parse_size(input: &str) -> Result<u32, &'static str> {
    let input = input.trim();
    if let Some(hex) = input
        .strip_prefix("0x")
        .or_else(|| input.strip_prefix("0X"))
    {
        return u32::from_str_radix(hex.trim(), 16).map_err(|_| "invalid hexadecimal size");
    }
    if let Ok(size) = input.parse::<u32>() {
        return Ok(size);
    }
    let lower = input.to_ascii_lowercase();
    let (number, multiplier) = [
        ("mib", 1 << 20),
        ("mb", 1 << 20),
        ("kib", 1 << 10),
        ("kb", 1 << 10),
        ("b", 1),
    ]
    .into_iter()
    .find_map(|(suffix, multiplier)| lower.strip_suffix(suffix).map(|n| (n.trim(), multiplier)))
    .ok_or("invalid size unit")?;
    number
        .parse::<u32>()
        .map_err(|_| "invalid size")?
        .checked_mul(multiplier)
        .ok_or("size exceeds 32-bit address space")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_and_overflow() {
        assert_eq!(parse_size(" 0Xff "), Ok(255));
        assert_eq!(parse_size("2 MiB"), Ok(2 << 20));
        assert_eq!(parse_size("4294967295 B"), Ok(u32::MAX));
        assert!(parse_size("4096 MiB").is_err());
        assert!(parse_size("4294967296").is_err());
        assert!(parse_size("-1 KB").is_err());
    }
}

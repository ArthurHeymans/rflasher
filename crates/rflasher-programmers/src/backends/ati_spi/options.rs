use rflasher_pci::PciAddress;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AtiSpiOptions {
    /// Exact domain:bus:device.function address; required for multiple GPUs.
    pub pci: Option<PciAddress>,
}

pub fn parse_options(options: &[(&str, &str)]) -> Result<AtiSpiOptions, String> {
    let mut config = AtiSpiOptions::default();
    for &(key, value) in options {
        match key {
            "pci" if config.pci.is_none() => config.pci = Some(parse_address(value)?),
            "pci" => return Err("duplicate pci option".into()),
            _ => return Err(format!("unknown option: {key}")),
        }
    }
    Ok(config)
}

fn parse_address(value: &str) -> Result<PciAddress, String> {
    let invalid = || format!("invalid PCI address '{value}': expected DDDD:BB:DD.F");
    // Validate before constructing PciAddress, whose bitfields truncate values.
    let bytes = value.as_bytes();
    if bytes.len() != 12
        || bytes[4] != b':'
        || bytes[7] != b':'
        || bytes[10] != b'.'
        || !bytes
            .iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7 | 10) || b.is_ascii_hexdigit())
    {
        return Err(invalid());
    }
    let segment = u16::from_str_radix(&value[..4], 16).map_err(|_| invalid())?;
    let bus = u8::from_str_radix(&value[5..7], 16).map_err(|_| invalid())?;
    let device = u8::from_str_radix(&value[8..10], 16).map_err(|_| invalid())?;
    let function = u8::from_str_radix(&value[11..], 16).map_err(|_| invalid())?;
    if device > 31 || function > 7 {
        return Err(invalid());
    }
    Ok(PciAddress::new(segment, bus, device, function))
}

pub mod schema {
    use crate::catalog::{OptionKind, OptionSpec, Scope};

    pub const OPTIONS: &[OptionSpec] = &[OptionSpec {
        key: "pci",
        label: "PCI address",
        help: "Exact GPU address, DDDD:BB:DD.F (required when multiple GPUs match)",
        kind: OptionKind::Text,
        default: None,
        scope: Scope::NativeOnly,
    }];

    pub fn validate_options(options: &[(&str, &str)]) -> Result<(), String> {
        super::parse_options(options).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_exact_bdf_without_truncating_invalid_values() {
        assert_eq!(parse_options(&[]).unwrap(), AtiSpiOptions::default());
        assert_eq!(
            parse_options(&[("pci", "1234:ab:1F.7")]).unwrap().pci,
            Some(PciAddress::new(0x1234, 0xab, 0x1f, 7))
        );
        for bdf in [
            "03:00.0",
            "0000:03:20.0",
            "0000:03:00.8",
            "0000:03:00.g",
            "0000:03:00.0/",
            "💻:00:00.0",
        ] {
            assert!(parse_options(&[("pci", bdf)]).is_err(), "{bdf}");
        }
        assert!(parse_options(&[("pci", "0000:03:00.0"), ("pci", "0000:04:00.0")]).is_err());
        assert!(parse_options(&[("bad", "1")]).is_err());
    }
}

//! Derived, human-readable identity output. Never used for matching or probing.

use rflasher_core::chip::{ChipIdentity, ProbeMethod};

/// Show the recorded identity, preserving every byte in its method namespace.
/// Device/data bytes are the remaining response, not a universal u16 model code.
pub fn format_identity(identity: Option<ChipIdentity>) -> String {
    let Some(identity) = identity else {
        return format!("{:<17}Not recorded\n", "Observed ID:");
    };
    let (value, length): (u32, usize) = match identity {
        ChipIdentity::Rdid { id } | ChipIdentity::Res3 { id } => {
            (id, if id > 0xff_ffff { 4 } else { 3 })
        }
        ChipIdentity::Rems { id } | ChipIdentity::Res2 { id } | ChipIdentity::At25f { id } => {
            (u32::from(id), 2)
        }
        ChipIdentity::Res1 { id } => (u32::from(id), 1),
    };
    let bytes = value.to_be_bytes();
    let raw = &bytes[4 - length..];
    if !identity.is_valid() {
        return format!(
            "{:<17}{}\n{:<17}{}\n{:<17}Unsupported identity encoding\n",
            "Observed ID:",
            method_name(identity.method()),
            "Raw ID bytes:",
            hex(raw),
            "Decoded fields:",
        );
    }
    format_response(identity.method(), raw)
}

fn method_name(method: ProbeMethod) -> &'static str {
    match method {
        ProbeMethod::Rdid => "RDID (0x9F)",
        ProbeMethod::Rems => "REMS (0x90)",
        ProbeMethod::Res1 => "RES1 (0xAB)",
        ProbeMethod::Res2 => "RES2 (0xAB)",
        ProbeMethod::Res3 => "RES3 (0xAB)",
        ProbeMethod::At25f => "AT25F (0x15)",
    }
}

fn hex(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "Not recorded".to_owned();
    }
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Slice-based so display does not impose a fixed device-field width. This
/// does not add support for collecting or matching longer RDID responses.
fn format_response(method: ProbeMethod, raw: &[u8]) -> String {
    let mut output = format!(
        "{:<17}{}\n{:<17}{}\n",
        "Observed ID:",
        method_name(method),
        "Raw ID bytes:",
        hex(raw)
    );
    if method == ProbeMethod::Res1 {
        output.push_str(&format!(
            "{:<17}Not present (device-only signature)\n{:<17}{}\n",
            "Manufacturer ID:",
            "Device/data:",
            hex(raw)
        ));
        return output;
    }
    // The prober treats these uniform RDID replies as absent. Keep the raw
    // reply visible, but do not invent a manufacturer from it.
    if raw.is_empty()
        || (method == ProbeMethod::Rdid
            && (raw.iter().all(|b| *b == 0) || raw.iter().all(|b| *b == 0xff)))
    {
        output.push_str(&format!(
            "{:<17}Not decoded (no ID response)\n",
            "Decoded fields:"
        ));
        return output;
    }
    let continuation = if matches!(method, ProbeMethod::Rdid | ProbeMethod::Res3) {
        raw.iter().take_while(|b| **b == 0x7f).count()
    } else {
        0
    };
    if continuation != 0 {
        output.push_str(&format!(
            "{:<17}{}\n",
            "Continuation:",
            hex(&raw[..continuation])
        ));
    }
    if let Some((&manufacturer, device)) = raw[continuation..].split_first() {
        output.push_str(&format!(
            "{:<17}{manufacturer:02X}\n{:<17}{}\n",
            "Manufacturer ID:",
            "Device/data:",
            hex(device)
        ));
    } else {
        output.push_str(&format!(
            "{:<17}Not recorded after continuation\n",
            "Manufacturer ID:"
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_rdid_is_split_without_changing_the_raw_value() {
        let identity = ChipIdentity::Rdid { id: 0xef4018 };
        assert_eq!(
            format_identity(Some(identity)),
            concat!(
                "Observed ID:     RDID (0x9F)\n",
                "Raw ID bytes:    EF 40 18\n",
                "Manufacturer ID: EF\n",
                "Device/data:     40 18\n",
            )
        );
        assert_eq!(identity, ChipIdentity::Rdid { id: 0xef4018 });
    }

    #[test]
    fn continuation_is_not_misreported_as_the_manufacturer() {
        for (identity, manufacturer, device) in [
            (ChipIdentity::Rdid { id: 0x7f9d21 }, "9D", "21"),
            (ChipIdentity::Rdid { id: 0x7f372021 }, "37", "20 21"),
            (ChipIdentity::Res3 { id: 0x7f9d7b }, "9D", "7B"),
        ] {
            let output = format_identity(Some(identity));
            assert!(output.contains("Continuation:    7F\n"));
            assert!(output.contains(&format!("Manufacturer ID: {manufacturer}\n")));
            assert!(output.contains(&format!("Device/data:     {device}\n")));
        }
        let output = format_identity(Some(ChipIdentity::Rdid { id: 0x7f7f9d21 }));
        assert!(output.contains("Raw ID bytes:    7F 7F 9D 21\n"));
        assert!(output.contains("Continuation:    7F 7F\n"));
        assert!(output.contains("Manufacturer ID: 9D\n"));
    }

    #[test]
    fn legacy_methods_keep_their_own_namespaces_and_byte_widths() {
        for (identity, method) in [
            (ChipIdentity::Rems { id: 0xbf48 }, "REMS (0x90)"),
            (ChipIdentity::Res2 { id: 0xbf48 }, "RES2 (0xAB)"),
            (ChipIdentity::At25f { id: 0x1f63 }, "AT25F (0x15)"),
        ] {
            let output = format_identity(Some(identity));
            assert!(output.starts_with(&format!("Observed ID:     {method}\n")));
            assert!(!output.contains("Continuation:"));
        }
        let output = format_identity(Some(ChipIdentity::Rems { id: 0x7f48 }));
        assert!(output.contains("Manufacturer ID: 7F\n"));
    }

    #[test]
    fn one_byte_signature_does_not_invent_a_manufacturer() {
        let output = format_identity(Some(ChipIdentity::Res1 { id: 0x12 }));
        assert!(output.contains("RES1 (0xAB)"));
        assert!(output.contains("Raw ID bytes:    12\n"));
        assert!(output.contains("Manufacturer ID: Not present (device-only signature)\n"));
        assert!(output.contains("Device/data:     12\n"));
    }

    #[test]
    fn extra_and_short_device_data_are_not_truncated_or_padded() {
        // Formatting-only examples: no new extended-ID probing/matching.
        let output = format_response(ProbeMethod::Rdid, &[0x01, 0x20, 0x18, 0x03, 0x01]);
        assert!(output.contains("Raw ID bytes:    01 20 18 03 01\n"));
        assert!(output.contains("Device/data:     20 18 03 01\n"));
        let output = format_response(ProbeMethod::Rdid, &[0x62, 0x1e]);
        assert!(output.contains("Raw ID bytes:    62 1E\n"));
        assert!(output.contains("Device/data:     1E\n"));
        let output = format_identity(Some(ChipIdentity::Rdid { id: 0x621e62 }));
        assert!(output.contains("Raw ID bytes:    62 1E 62\n"));
        assert!(output.contains("Device/data:     1E 62\n"));
    }

    #[test]
    fn absent_or_invalid_observations_are_not_fabricated() {
        assert_eq!(format_identity(None), "Observed ID:     Not recorded\n");
        for id in [0, 0xffffff] {
            let output = format_identity(Some(ChipIdentity::Rdid { id }));
            assert!(output.contains("Not decoded (no ID response)"));
            assert!(!output.contains("Manufacturer ID:"));
        }
        let output = format_identity(Some(ChipIdentity::Rdid { id: 0x12345678 }));
        assert!(output.contains("Raw ID bytes:    12 34 56 78\n"));
        assert!(output.contains("Unsupported identity encoding"));
        assert!(!output.contains("Manufacturer ID:"));
    }
}

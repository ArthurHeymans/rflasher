//! Schema-driven programmer option form state.
//!
//! This module is deliberately free of egui and browser APIs: it only turns
//! the shared [`ProgrammerInfo`] schema into editable string values and back
//! into the `(key, value)` pairs the backend parsers consume. `app.rs` renders
//! it and dispatches the connection.

use rflasher_programmers::catalog::{OptionKind, OptionSpec, ProgrammerInfo, Scope, Transport};

/// One editable option: the schema entry plus the user's raw text.
///
/// The value is always the raw string the backend parser will see, so free
/// text like `30m` for a speed is forwarded untouched. An empty value means
/// "not set" and is omitted from the pairs, leaving the backend default.
pub struct Field {
    pub spec: &'static OptionSpec,
    pub value: String,
}

/// Editable option values for the currently selected programmer.
pub struct ProgrammerForm {
    /// Canonical programmer name (`ProgrammerInfo::name`).
    pub name: &'static str,
    /// Web-visible options in schema order, seeded from `OptionSpec::default`.
    pub fields: Vec<Field>,
}

/// Whether an option should be rendered by the web frontend.
///
/// `NativeOnly` options and filesystem paths are replaced by the browser
/// device picker.
pub fn is_web_visible(spec: &OptionSpec) -> bool {
    spec.scope != Scope::NativeOnly && !matches!(spec.kind, OptionKind::Path)
}

/// Whether the browser can open a programmer with this transport.
pub fn is_web_transport(transport: Transport) -> bool {
    matches!(
        transport,
        Transport::Usb | Transport::Serial | Transport::SerialOrTcp
    )
}

impl ProgrammerForm {
    /// Build a form for `info`, pre-filled with the schema defaults.
    pub fn new(info: &ProgrammerInfo) -> Self {
        let fields = info
            .options
            .iter()
            .filter(|spec| is_web_visible(spec))
            .map(|spec| Field {
                spec,
                value: spec.default.unwrap_or_default().to_string(),
            })
            .collect();
        Self {
            name: info.name,
            fields,
        }
    }

    /// Non-empty values as borrowed `(key, value)` pairs in schema order.
    pub fn pairs(&self) -> Vec<(&str, &str)> {
        self.fields
            .iter()
            .filter(|f| !f.value.is_empty())
            .map(|f| (f.spec.key, f.value.as_str()))
            .collect()
    }

    /// Non-empty values as owned pairs, for moving into an async task.
    pub fn owned_pairs(&self) -> Vec<(String, String)> {
        self.pairs()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Run the backend's real parser over the current values.
    pub fn validate(&self, info: &ProgrammerInfo) -> Result<(), String> {
        (info.validate)(&self.pairs())
    }
}

/// Local range check for `Int` options, giving immediate feedback before the
/// backend parser (which may not know the range) sees the value.
pub fn int_error(spec: &OptionSpec, value: &str) -> Option<String> {
    let OptionKind::Int { min, max } = spec.kind else {
        return None;
    };
    if value.is_empty() {
        return None;
    }
    match value.trim().parse::<i64>() {
        Ok(n) if (min..=max).contains(&n) => None,
        Ok(_) => Some(format!("must be between {min} and {max}")),
        Err(_) => Some("must be a whole number".to_string()),
    }
}

/// Human-readable label for a kHz preset (`30000` -> `30 MHz`).
pub fn speed_label(khz: u32) -> String {
    if khz.is_multiple_of(1000) {
        format!("{} MHz", khz / 1000)
    } else if khz >= 1000 {
        format!("{:.3} MHz", khz as f64 / 1000.0)
    } else {
        format!("{khz} kHz")
    }
}

/// Borrow owned pairs as `&str` pairs for the backend parsers.
pub fn borrow_pairs(pairs: &[(String, String)]) -> Vec<(&str, &str)> {
    pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rflasher_programmers::catalog::{Choice, choice};
    use wasm_bindgen_test::wasm_bindgen_test;

    const CHOICES: &[Choice] = &[choice("a", "A"), choice("b", "B")];

    const OPTIONS: &[OptionSpec] = &[
        OptionSpec {
            key: "mode",
            label: "Mode",
            help: "",
            kind: OptionKind::Choice(CHOICES),
            default: Some("a"),
            scope: Scope::Any,
        },
        OptionSpec {
            key: "count",
            label: "Count",
            help: "",
            kind: OptionKind::Int { min: 2, max: 10 },
            default: None,
            scope: Scope::Any,
        },
        OptionSpec {
            key: "serial",
            label: "Serial",
            help: "",
            kind: OptionKind::Text,
            default: None,
            scope: Scope::NativeOnly,
        },
        OptionSpec {
            key: "dev",
            label: "Device",
            help: "",
            kind: OptionKind::Path,
            default: Some("/dev/x"),
            scope: Scope::Any,
        },
    ];

    fn reject_count_over_five(options: &[(&str, &str)]) -> Result<(), String> {
        match options.iter().find(|(k, _)| *k == "count") {
            Some((_, v)) if v.parse::<i64>().is_ok_and(|n| n > 5) || v.parse::<i64>().is_err() => {
                Err("too big".into())
            }
            _ => Ok(()),
        }
    }

    const INFO: ProgrammerInfo = ProgrammerInfo {
        name: "test",
        aliases: &[],
        description: "",
        transport: Transport::Usb,
        options: OPTIONS,
        validate: reject_count_over_five,
    };

    #[wasm_bindgen_test]
    fn seeds_web_visible_fields_from_defaults() {
        let form = ProgrammerForm::new(&INFO);
        let keys: Vec<&str> = form.fields.iter().map(|f| f.spec.key).collect();
        assert_eq!(keys, vec!["mode", "count"]);
        assert_eq!(form.fields[0].value, "a");
        assert_eq!(form.fields[1].value, "");
    }

    #[wasm_bindgen_test]
    fn pairs_omit_empty_values_and_keep_schema_order() {
        let mut form = ProgrammerForm::new(&INFO);
        assert_eq!(form.pairs(), vec![("mode", "a")]);
        form.fields[1].value = "3".into();
        assert_eq!(form.pairs(), vec![("mode", "a"), ("count", "3")]);
        assert_eq!(
            form.owned_pairs(),
            vec![
                ("mode".to_string(), "a".to_string()),
                ("count".to_string(), "3".to_string())
            ]
        );
    }

    #[wasm_bindgen_test]
    fn validate_runs_the_backend_validator() {
        let mut form = ProgrammerForm::new(&INFO);
        assert!(form.validate(&INFO).is_ok());
        form.fields[1].value = "9".into();
        assert_eq!(form.validate(&INFO), Err("too big".to_string()));
    }

    #[wasm_bindgen_test]
    fn int_error_checks_range_and_syntax() {
        let spec = &OPTIONS[1];
        assert_eq!(int_error(spec, ""), None);
        assert_eq!(int_error(spec, "2"), None);
        assert_eq!(int_error(spec, "10"), None);
        assert!(int_error(spec, "11").is_some());
        assert!(int_error(spec, "x").is_some());
        assert_eq!(int_error(&OPTIONS[0], "anything"), None);
    }

    #[wasm_bindgen_test]
    fn speed_labels_are_human_readable() {
        assert_eq!(speed_label(30000), "30 MHz");
        assert_eq!(speed_label(1875), "1.875 MHz");
        assert_eq!(speed_label(937), "937 kHz");
    }

    /// The real catalog: every web-visible default must survive its own
    /// backend parser, and native-only keys must never reach the form.
    #[wasm_bindgen_test]
    fn catalog_defaults_validate_for_every_web_programmer() {
        use rflasher_programmers::catalog::available_programmers;
        for info in available_programmers()
            .iter()
            .filter(|p| is_web_transport(p.transport))
        {
            let form = ProgrammerForm::new(info);
            assert!(
                form.fields.iter().all(|f| is_web_visible(f.spec)),
                "{} exposes a native-only option",
                info.name
            );
            form.validate(info)
                .unwrap_or_else(|e| panic!("{} rejects its own defaults: {e}", info.name));
        }
    }

    #[wasm_bindgen_test]
    fn only_browser_transports_are_offered() {
        assert!(is_web_transport(Transport::Usb));
        assert!(is_web_transport(Transport::SerialOrTcp));
        assert!(!is_web_transport(Transport::LinuxDevice));
        assert!(!is_web_transport(Transport::Internal));
    }
}

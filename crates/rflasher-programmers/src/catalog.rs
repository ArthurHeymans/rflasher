//! Shared programmer catalog: option schema, parameter parsing, and programmer
//! listing.
//!
//! This module is the single description of *what options each programmer
//! accepts*. It is compiled on every target that has `std` (native and wasm),
//! so the CLI and the web frontend can both consume it:
//!
//! - The CLI renders `-p <name>:key=value,...` and passes the raw pairs to each
//!   backend's own `parse_options`.
//! - The web frontend renders widgets from [`ProgrammerInfo::options`] and
//!   passes the same raw pairs to [`ProgrammerInfo::validate`], which runs the
//!   very same backend parser.
//!
//! The typed config structs and their `parse_options` functions remain the
//! semantic source of truth (they encode the irregular cases such as speed
//! suffixes, `dev=path:baud`, and cross-field defaults). The schema here is
//! descriptive: labels, help, ranges, choice lists, and platform scope.
//!
//! The `schema_tests` tests in this module are what keep the two from
//! drifting: every declared key, every choice value, and every declared default
//! is fed back through the real parser.

// Option tables and their shared choice lists are gated by backend features. A
// build that enables only some backends legitimately leaves the others unused.
#![allow(dead_code)]

/// Where an option can meaningfully be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Usable from both the CLI and the web frontend.
    Any,
    /// Only meaningful on native targets (device paths, serial numbers,
    /// kernel device selection). Hidden by the web frontend, which uses the
    /// browser device picker instead.
    NativeOnly,
}

/// A single accepted value for an enumerated option.
#[derive(Debug, Clone, Copy)]
pub struct Choice {
    /// The literal value written to the config (`-p x:key=value`).
    pub value: &'static str,
    /// Human-readable label for the web frontend.
    pub label: &'static str,
}

/// Const constructor for [`Choice`] tables.
pub const fn choice(value: &'static str, label: &'static str) -> Choice {
    Choice { value, label }
}

/// Value domain of an option, used to pick a widget and to render help text.
#[derive(Debug, Clone, Copy)]
pub enum OptionKind {
    /// Integer in an inclusive range.
    Int { min: i64, max: i64 },
    /// One of a fixed set of string values.
    Choice(&'static [Choice]),
    /// SPI clock in kHz; accepts a plain number or a `k`/`m`/`g` suffix.
    SpeedKhz { presets: &'static [u32] },
    /// Free-form text (serial numbers, device ids).
    Text,
    /// A host filesystem path (native only).
    Path,
}

impl OptionKind {
    /// Placeholder shown in generated help text, e.g. `N`, `A|B`, `khz`.
    pub fn value_hint(&self) -> String {
        match self {
            OptionKind::Int { min, max } => format!("{min}-{max}"),
            OptionKind::Choice(choices) => choices
                .iter()
                .map(|c| c.value)
                .collect::<Vec<_>>()
                .join("|"),
            OptionKind::SpeedKhz { .. } => "khz".to_string(),
            OptionKind::Text => "value".to_string(),
            OptionKind::Path => "path".to_string(),
        }
    }
}

/// Description of one programmer option.
#[derive(Debug, Clone, Copy)]
pub struct OptionSpec {
    /// The key used in `-p <name>:key=value`.
    pub key: &'static str,
    /// Human-readable label for the web frontend.
    pub label: &'static str,
    /// One-line help text.
    pub help: &'static str,
    /// Value domain.
    pub kind: OptionKind,
    /// Value the web frontend pre-fills. `None` means "omit unless the user
    /// sets it", leaving the backend's own default in place.
    pub default: Option<&'static str>,
    /// Where the option applies.
    pub scope: Scope,
}

impl OptionSpec {
    /// Render as `key=<hint>` for generated help text.
    pub fn syntax(&self) -> String {
        format!("{}=<{}>", self.key, self.kind.value_hint())
    }
}

/// How the frontend reaches a programmer's transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// WebUSB / native USB enumeration.
    Usb,
    /// WebSerial / native serial port.
    Serial,
    /// Serial or TCP (native), WebSerial (web).
    SerialOrTcp,
    /// A kernel device node (`/dev/...`); native only.
    LinuxDevice,
    /// Chipset-internal controller; native only.
    Internal,
    /// No transport selection (in-memory).
    None,
}

/// Runs a backend's real parser and discards the config.
pub type ValidateFn = fn(&[(&str, &str)]) -> Result<(), String>;

/// Everything the frontends need to describe and validate a programmer.
pub struct ProgrammerInfo {
    /// Canonical name used with `-p`.
    pub name: &'static str,
    /// Alternative names accepted by the CLI.
    pub aliases: &'static [&'static str],
    /// One-line description.
    pub description: &'static str,
    /// Transport used to open the device.
    pub transport: Transport,
    /// Accepted options.
    pub options: &'static [OptionSpec],
    /// Runs the backend's real parser and discards the config.
    ///
    /// This is the single validation entry point the web frontend uses without
    /// knowing any backend config type.
    pub validate: ValidateFn,
}

impl ProgrammerInfo {
    /// Render the option list as `key=<hint>,...` for human-readable help.
    pub fn options_syntax(&self) -> String {
        self.options
            .iter()
            .map(|o| o.syntax())
            .collect::<Vec<_>>()
            .join(",")
    }
}

// ============================================================================
// Option tables
// ============================================================================

const INTERNAL_MODES: &[Choice] = &[
    choice("auto", "Auto"),
    choice("hwseq", "Hardware sequencing"),
    choice("swseq", "Software sequencing"),
];

#[cfg(feature = "internal")]
const INTERNAL_OPTIONS: &[OptionSpec] = &[OptionSpec {
    key: "ich_spi_mode",
    label: "SPI mode",
    help: "Chipset sequencing mode (also accepted as `mode`)",
    kind: OptionKind::Choice(INTERNAL_MODES),
    default: Some("auto"),
    scope: Scope::Any,
}];

// ============================================================================
// Validation wrappers
// ============================================================================

/// Accept no options at all (programmers without configurable behavior).
fn validate_no_options(options: &[(&str, &str)]) -> Result<(), String> {
    match options.first() {
        None => Ok(()),
        Some((key, _)) => Err(format!("unknown option: {key}")),
    }
}

#[cfg(feature = "internal")]
fn validate_internal(options: &[(&str, &str)]) -> Result<(), String> {
    rflasher_internal::InternalOptions::from_options(options)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ============================================================================
// Programmer catalog
// ============================================================================

/// Get information about all available programmers (enabled at compile time).
#[allow(unused_mut, clippy::vec_init_then_push)]
pub fn available_programmers() -> Vec<ProgrammerInfo> {
    let mut programmers = Vec::new();

    #[cfg(feature = "dummy")]
    programmers.push(ProgrammerInfo {
        name: "dummy",
        aliases: &[],
        description: "In-memory flash emulator for testing",
        transport: Transport::None,
        options: &[],
        validate: validate_no_options,
    });

    #[cfg(feature = "ch341a")]
    programmers.push(ProgrammerInfo {
        name: "ch341a",
        aliases: &["ch341a_spi"],
        description: "CH341A USB SPI programmer (VID:1a86 PID:5512)",
        transport: Transport::Usb,
        options: &[],
        validate: validate_no_options,
    });

    #[cfg(feature = "ch347")]
    programmers.push(ProgrammerInfo {
        name: "ch347",
        aliases: &["ch347_spi"],
        description: "CH347 USB SPI programmer (VID:1a86 PID:55db/55de)",
        transport: Transport::Usb,
        options: crate::ch347::schema::OPTIONS,
        validate: crate::ch347::schema::validate_options,
    });

    #[cfg(feature = "dediprog")]
    programmers.push(ProgrammerInfo {
        name: "dediprog",
        aliases: &["dediprog_spi"],
        description: "Dediprog SF100/SF200/SF600/SF700 USB SPI",
        transport: Transport::Usb,
        options: crate::dediprog::schema::OPTIONS,
        validate: crate::dediprog::schema::validate_options,
    });

    #[cfg(feature = "serprog")]
    programmers.push(ProgrammerInfo {
        name: "serprog",
        aliases: &[],
        description: "Serial Flasher Protocol over serial/network",
        transport: Transport::SerialOrTcp,
        options: crate::serprog::schema::OPTIONS,
        validate: crate::serprog::schema::validate_options,
    });

    #[cfg(any(feature = "ftdi", feature = "ftdi-wasm"))]
    programmers.push(ProgrammerInfo {
        name: "ftdi",
        aliases: &["ft2232_spi", "ft4232_spi"],
        description: "FTDI MPSSE programmer (FT2232H/FT4232H/FT232H)",
        transport: Transport::Usb,
        options: crate::ftdi::schema::OPTIONS,
        validate: crate::ftdi::schema::validate_options,
    });

    #[cfg(feature = "ft4222")]
    programmers.push(ProgrammerInfo {
        name: "ft4222",
        aliases: &["ft4222_spi"],
        description: "FTDI FT4222H USB SPI programmer",
        transport: Transport::Usb,
        options: crate::ft4222::schema::OPTIONS,
        validate: crate::ft4222::schema::validate_options,
    });

    #[cfg(all(feature = "linux-spi", target_os = "linux"))]
    programmers.push(ProgrammerInfo {
        name: "linux_spi",
        aliases: &["linux-spi", "spidev"],
        description: "Linux SPI device via spidev interface",
        transport: Transport::LinuxDevice,
        options: crate::linux_spi::schema::OPTIONS,
        validate: crate::linux_spi::schema::validate_options,
    });

    #[cfg(all(feature = "linux-mtd", target_os = "linux"))]
    programmers.push(ProgrammerInfo {
        name: "linux_mtd",
        aliases: &["linux-mtd", "mtd"],
        description: "Linux MTD (Memory Technology Device) for NOR flash",
        transport: Transport::LinuxDevice,
        options: crate::linux_mtd::schema::OPTIONS,
        validate: crate::linux_mtd::schema::validate_options,
    });

    #[cfg(all(feature = "linux-gpio", target_os = "linux"))]
    programmers.push(ProgrammerInfo {
        name: "linux_gpio_spi",
        aliases: &["linux-gpio-spi", "linux_gpio", "linux-gpio"],
        description: "Linux GPIO bitbang SPI",
        transport: Transport::LinuxDevice,
        options: crate::linux_gpio::schema::OPTIONS,
        validate: crate::linux_gpio::schema::validate_options,
    });

    #[cfg(feature = "internal")]
    programmers.push(ProgrammerInfo {
        name: "internal",
        aliases: &[],
        description: "Intel/AMD internal SPI controller",
        transport: Transport::Internal,
        options: INTERNAL_OPTIONS,
        validate: validate_internal,
    });

    #[cfg(feature = "raiden")]
    programmers.push(ProgrammerInfo {
        name: "raiden_debug_spi",
        aliases: &["raiden", "raiden_spi"],
        description: "Chrome OS EC USB SPI",
        transport: Transport::Usb,
        options: crate::raiden::schema::OPTIONS,
        validate: crate::raiden::schema::validate_options,
    });

    #[cfg(feature = "sunxi-fel")]
    programmers.push(ProgrammerInfo {
        name: "sunxi_fel",
        aliases: &["sunxi-fel", "fel"],
        description: "Allwinner sunxi FEL USB SPI NOR programmer (VID:1F3A PID:EFE8)",
        transport: Transport::Usb,
        options: &[],
        validate: validate_no_options,
    });

    programmers
}

/// Generate a short list of programmer names for CLI help.
pub fn programmer_names_short() -> String {
    let programmers = available_programmers();
    if programmers.is_empty() {
        return "none (recompile with features)".to_string();
    }
    let names: Vec<&str> = programmers.iter().map(|p| p.name).collect();
    names.join(", ")
}

// ============================================================================
// Parameter parsing
// ============================================================================

/// Parse a speed value in kHz from a string with optional suffix.
///
/// Supports formats like:
/// - `"1000"` - plain number in kHz
/// - `"1000k"` or `"1000K"` - kHz (same as plain)
/// - `"30m"` or `"30M"` - MHz (multiplied by 1000)
/// - `"1g"` or `"1G"` - GHz (multiplied by 1_000_000)
///
/// Returns `None` if the string cannot be parsed.
pub fn parse_speed_khz(s: &str) -> Option<u32> {
    const MAX_KHZ: u32 = u32::MAX / 1000;

    /// Convert a parsed value with the given unit multiplier to kHz,
    /// rejecting non-finite, non-positive, and out-of-range results.
    fn khz_from(val: f64, multiplier: f64) -> Option<u32> {
        let khz = (val * multiplier).round();
        if !khz.is_finite() || !(1.0..=MAX_KHZ as f64).contains(&khz) {
            return None;
        }
        Some(khz as u32)
    }

    let s = s.trim().to_lowercase();

    // Try with GHz suffix
    if let Some(num) = s.strip_suffix('g') {
        let val: f64 = num.trim().parse().ok()?;
        return khz_from(val, 1_000_000.0);
    }
    // Try with MHz suffix
    if let Some(num) = s.strip_suffix('m') {
        let val: f64 = num.trim().parse().ok()?;
        return khz_from(val, 1_000.0);
    }
    // Try with kHz suffix
    if let Some(num) = s.strip_suffix('k') {
        let val: f64 = num.trim().parse().ok()?;
        return khz_from(val, 1.0);
    }
    // Plain kHz
    let val: f64 = s.parse().ok()?;
    khz_from(val, 1.0)
}

/// Parsed programmer parameters.
///
/// Options are kept in first-seen order with last-value-wins semantics.
#[derive(Debug, Clone)]
pub struct ProgrammerParams {
    /// Canonical programmer name.
    pub name: String,
    /// Ordered `(key, value)` options.
    options: Vec<(String, String)>,
}

impl ProgrammerParams {
    /// Look up an option value by key.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Iterate over options as `(&str, &str)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.options.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Convert parameters to a `Vec` of pairs for passing to `parse_options`.
    pub fn as_option_pairs(&self) -> Vec<(&str, &str)> {
        self.iter().collect()
    }
}

/// Parse a programmer string into name and parameters.
///
/// Format: `"name"` or `"name:key1=value1,key2=value2"`.
///
/// Options are kept in first-seen order with last-value-wins semantics. Order
/// matters to some parsers (FTDI applies `type` before derived defaults), so the
/// pairs are handed to backends verbatim rather than through a hash map.
///
/// # Example
/// ```ignore
/// let params = parse_programmer_params("ch347:spispeed=7500,cs=1")?;
/// assert_eq!(params.name, "ch347");
/// assert_eq!(params.get("cs"), Some("1"));
/// ```
pub fn parse_programmer_params(s: &str) -> Result<ProgrammerParams, Box<dyn std::error::Error>> {
    let (name, opts_str) = s.split_once(':').unwrap_or((s, ""));

    let mut options: Vec<(String, String)> = Vec::new();
    if !opts_str.is_empty() {
        for opt in opts_str.split(',') {
            if let Some((key, value)) = opt.split_once('=') {
                match options.iter_mut().find(|(k, _)| k == key) {
                    Some(slot) => slot.1 = value.to_string(),
                    None => options.push((key.to_string(), value.to_string())),
                }
            } else {
                return Err(
                    format!("Invalid parameter format: '{opt}' (expected key=value)").into(),
                );
            }
        }
    }

    Ok(ProgrammerParams {
        name: name.to_string(),
        options,
    })
}

#[cfg(test)]
mod speed_tests {
    use super::parse_speed_khz;

    #[test]
    fn parses_supported_speed_units() {
        assert_eq!(parse_speed_khz("1000"), Some(1000));
        assert_eq!(parse_speed_khz("1000K"), Some(1000));
        assert_eq!(parse_speed_khz("30m"), Some(30_000));
        assert_eq!(parse_speed_khz("1.5g"), Some(1_500_000));
    }

    #[test]
    fn rejects_speeds_that_cannot_be_converted_to_hz() {
        assert_eq!(parse_speed_khz("0"), None);
        assert_eq!(parse_speed_khz("-5m"), None);
        assert_eq!(parse_speed_khz("nan"), None);
        assert_eq!(parse_speed_khz("4.294968g"), None);
        assert_eq!(parse_speed_khz("4294968k"), None);
        assert_eq!(parse_speed_khz("4294968"), None);
    }
}

#[cfg(test)]
mod param_tests {
    use super::*;

    #[test]
    fn options_keep_declaration_order_for_order_sensitive_parsers() {
        let params = parse_programmer_params("ftdi:divisor=6,type=232h").unwrap();
        assert_eq!(
            params.as_option_pairs(),
            vec![("divisor", "6"), ("type", "232h")]
        );
    }

    #[test]
    fn repeated_keys_keep_their_position_and_the_last_value_wins() {
        let params = parse_programmer_params("ch347:cs=1,spispeed=30000,cs=0").unwrap();
        assert_eq!(params.get("cs"), Some("0"));
        assert_eq!(params.as_option_pairs().len(), 2);
        assert_eq!(
            params.as_option_pairs(),
            vec![("cs", "0"), ("spispeed", "30000")]
        );
    }

    #[test]
    fn malformed_option_without_equals_is_rejected() {
        assert!(parse_programmer_params("ch347:cs").is_err());
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;

    /// Required options for programmers whose parser rejects an otherwise empty
    /// config. Keep this in sync with the backend parsers; it is the only place
    /// the schema test needs backend-specific knowledge.
    fn required_base(name: &str) -> Vec<(&'static str, &'static str)> {
        match name {
            "linux_gpio_spi" => vec![
                ("dev", "/dev/gpiochip0"),
                ("cs", "25"),
                ("sck", "11"),
                ("mosi", "10"),
                ("miso", "9"),
            ],
            "linux_mtd" => vec![("dev", "0")],
            "linux_spi" => vec![("dev", "/dev/spidev0.0")],
            _ => Vec::new(),
        }
    }

    /// Required options adjusted for options that are mutually exclusive with, or
    /// co-required by, another option.
    ///
    /// - `linux_gpio` accepts either `dev` or `gpiochip`, not both.
    /// - `linux_gpio` requires `io2` and `io3` to be given together.
    fn base_for(name: &str, key: &str) -> Vec<(&'static str, &'static str)> {
        let mut base = required_base(name);
        if name == "linux_gpio_spi" {
            match key {
                "gpiochip" => base.retain(|(k, _)| *k != "dev"),
                "io2" => base.push(("io3", "3")),
                "io3" => base.push(("io2", "2")),
                _ => {}
            }
        }
        base
    }

    /// A representative accepted value for an option domain.
    fn sample_value(kind: &OptionKind) -> String {
        match kind {
            OptionKind::Int { min, .. } => min.to_string(),
            OptionKind::Choice(choices) => choices
                .first()
                .expect("choice list must not be empty")
                .value
                .to_string(),
            OptionKind::SpeedKhz { presets } => presets
                .first()
                .map(u32::to_string)
                .unwrap_or_else(|| "1m".to_string()),
            OptionKind::Text => "sample".to_string(),
            OptionKind::Path => "/dev/null".to_string(),
        }
    }

    #[test]
    fn option_keys_are_unique_and_nonempty() {
        for info in available_programmers() {
            let mut seen = Vec::new();
            for spec in info.options {
                assert!(
                    !spec.key.is_empty(),
                    "{} has an empty option key",
                    info.name
                );
                assert!(
                    !seen.contains(&spec.key),
                    "{} declares '{}' twice",
                    info.name,
                    spec.key
                );
                seen.push(spec.key);
            }
        }
    }

    #[test]
    fn choice_lists_are_nonempty_and_unique() {
        for info in available_programmers() {
            for spec in info.options {
                if let OptionKind::Choice(choices) = spec.kind {
                    assert!(
                        !choices.is_empty(),
                        "{}:{} has no choices",
                        info.name,
                        spec.key
                    );
                    let mut seen = Vec::new();
                    for c in choices {
                        assert!(!c.value.is_empty());
                        assert!(
                            !seen.contains(&c.value),
                            "{}:{} repeats choice '{}'",
                            info.name,
                            spec.key,
                            c.value
                        );
                        seen.push(c.value);
                    }
                }
            }
        }
    }

    /// Every declared key and every declared value must be accepted by the
    /// programmer's real parser. This is the forward half of the schema/parser
    /// guard.
    #[test]
    fn every_declared_option_value_is_accepted_by_its_parser() {
        for info in available_programmers() {
            for spec in info.options {
                let base = base_for(info.name, spec.key);
                let mut values: Vec<String> = Vec::new();
                if let Some(default) = spec.default {
                    values.push(default.to_string());
                }
                match spec.kind {
                    OptionKind::Choice(choices) => {
                        values.extend(choices.iter().map(|c| c.value.to_string()));
                    }
                    OptionKind::SpeedKhz { presets } => {
                        values.extend(presets.iter().map(u32::to_string));
                        // Also exercise the documented k/m/g suffix contract.
                        values.push("1m".to_string());
                    }
                    _ => values.push(sample_value(&spec.kind)),
                }

                for value in values {
                    let mut pairs = base.clone();
                    pairs.push((spec.key, value.as_str()));
                    if let Err(e) = (info.validate)(&pairs) {
                        panic!("{} rejected {}={:?}: {}", info.name, spec.key, value, e);
                    }
                }
            }
        }
    }

    /// Programmers with options must reject unknown keys, so a GUI that sends a
    /// stale or misspelled key fails loudly instead of silently ignoring it.
    #[test]
    fn unknown_options_are_rejected() {
        for info in available_programmers() {
            let mut pairs = required_base(info.name);
            pairs.push(("definitely_not_a_real_option", "1"));
            assert!(
                (info.validate)(&pairs).is_err(),
                "{} accepted an unknown option",
                info.name
            );
        }
    }

    /// Defaults are what the web form pre-fills, so they must parse.
    #[test]
    fn declared_defaults_are_accepted() {
        for info in available_programmers() {
            let mut pairs = required_base(info.name);
            for spec in info.options {
                if let Some(default) = spec.default {
                    pairs.push((spec.key, default));
                }
            }
            if let Err(e) = (info.validate)(&pairs) {
                panic!("{} rejected its own defaults: {}", info.name, e);
            }
        }
    }

    #[test]
    fn programmers_without_options_reject_everything() {
        for info in available_programmers() {
            if info.options.is_empty() {
                assert!(
                    (info.validate)(&[("anything", "1")]).is_err(),
                    "{} accepts options but declares none",
                    info.name
                );
            }
        }
    }

    /// Parser-accepted key aliases that are intentionally not separate UI fields.
    /// Feeding them through `validate` keeps the aliases working even though the
    /// schema only lists canonical keys.
    #[test]
    fn documented_option_aliases_are_accepted() {
        let cases: &[(&str, &[(&str, &str)])] = &[
            ("ftdi", &[("type", "4232h"), ("channel", "B")]),
            ("dediprog", &[("index", "0")]),
            ("internal", &[("mode", "hwseq")]),
            (
                "linux_gpio_spi",
                &[
                    ("dev", "/dev/gpiochip0"),
                    ("cs", "25"),
                    ("sck", "11"),
                    ("io0", "10"),
                    ("io1", "9"),
                ],
            ),
        ];

        for (name, pairs) in cases {
            let Some(info) = available_programmers()
                .into_iter()
                .find(|p| p.name == *name)
            else {
                continue;
            };
            (info.validate)(pairs).unwrap_or_else(|e| panic!("{name} rejected alias keys: {e}"));
        }
    }

    #[test]
    fn option_syntax_renders_all_keys() {
        for info in available_programmers() {
            let syntax = info.options_syntax();
            for spec in info.options {
                assert!(
                    syntax.contains(spec.key),
                    "{} help is missing {}",
                    info.name,
                    spec.key
                );
            }
        }
    }
}

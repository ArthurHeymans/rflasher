//! `rflasher_programmers::serprog` - Serial Flasher Protocol support
//!
//! This crate implements the serprog protocol for communication with
//! microcontroller-based flash programmers.
//!
//! The API is async on every target.
//!
//! # Protocol Overview
//!
//! The Serial Flasher Protocol (serprog) is a simple protocol for communicating
//! with flash programmers over serial ports or TCP sockets. It supports various
//! commands for SPI operations, bus type selection, and programmer configuration.
//!
//! # Supported Transports
//!
//! - Serial port: `/dev/ttyUSB0`, `/dev/ttyACM0`, `COM1`, etc. (sync mode only)
//! - TCP socket: `host:port` (sync mode only)
//! - Custom transports: Implement the `Transport` trait for WebSerial, etc.
//!
//! # Example
//!
//! ```ignore
//! use rflasher_programmers::serprog::{Serprog, SerialTransport};
//! use rflasher_core::programmer::SpiMaster;
//! use rflasher_core::spi::{SpiCommand, opcodes};
//!
//! // Open a serial connection
//! let transport = SerialTransport::open("/dev/ttyUSB0", Some(115200))?;
//! let mut serprog = Serprog::new(transport)?;
//!
//! // Optionally set SPI speed
//! serprog.set_spi_speed(2_000_000)?;
//!
//! // Read JEDEC ID
//! let mut id = [0u8; 3];
//! let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut id);
//! serprog.execute(&mut cmd)?;
//! println!("JEDEC ID: {:02X} {:02X} {:02X}", id[0], id[1], id[2]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

// Allow async fn in traits; dynamic dispatch is provided by dedicated
// object-erasure adapters where needed.

#[cfg(not(feature = "std"))]
extern crate alloc;

pub mod error;
pub mod protocol;

// Device and transport are available in std mode
#[cfg(feature = "std")]
pub mod device;
#[cfg(feature = "std")]
pub mod transport;

// Re-exports
pub use error::{Result, SerprogError};
pub use protocol::{CommandMap, ProgrammerInfo, bus};

#[cfg(feature = "std")]
pub use device::Serprog;
#[cfg(feature = "std")]
pub use transport::Transport;

// Serial and TCP transports only available in sync mode with std
#[cfg(feature = "serprog-native")]
pub use transport::SerialTransport;
#[cfg(feature = "serprog-native")]
pub use transport::TcpTransport;

/// Parsed serprog options shared by the CLI and the web frontend.
///
/// Connection parameters are optional here: [`SerprogConfig::connection`]
/// enforces that `dev` or `ip` is present when actually opening a device, so a
/// frontend can validate a partially filled form without inventing a device.
#[derive(Debug, Clone, Default)]
pub struct SerprogConfig {
    /// Serial device path.
    pub dev: Option<String>,
    /// TCP endpoint (`host:port`).
    pub ip: Option<String>,
    /// Serial baud rate, overriding any baud embedded in `dev`.
    pub baud: Option<u32>,
    /// SPI clock in kHz.
    pub spispeed_khz: Option<u32>,
    /// Chip select to assert.
    pub cs: Option<u8>,
}

impl SerprogConfig {
    /// Build the native connection description for this configuration.
    #[cfg(feature = "serprog-native")]
    pub fn connection(&self) -> std::result::Result<SerprogConnection, String> {
        if self.ip.is_some() && self.baud.is_some() {
            return Err("baud= only applies to serial connections (dev=)".to_string());
        }
        let mut conn = match (self.dev.as_deref(), self.ip.as_deref()) {
            (Some(_), Some(_)) => {
                return Err("serprog accepts only one of dev= or ip=".to_string());
            }
            (Some(dev), None) => SerprogConnection::parse(&format!("dev={dev}"))?,
            (None, Some(ip)) => SerprogConnection::parse(&format!("ip={ip}"))?,
            (None, None) => {
                return Err("serprog requires connection parameters.\n\
                     Usage: serprog:dev=/dev/ttyUSB0[:baud] or serprog:ip=host:port"
                    .to_string());
            }
        };
        if let Some(baud) = self.baud
            && let SerprogConnection::Serial { baud: slot, .. } = &mut conn
        {
            *slot = Some(baud);
        }
        Ok(conn)
    }
}

/// Baud rates accepted by the serprog `baud=` option.
///
/// Shared by the parser and the option schema so the published range cannot
/// drift from what [`parse_options`] accepts.
pub const SERPROG_BAUD_MIN: u32 = 1;
pub const SERPROG_BAUD_MAX: u32 = 4_000_000;

/// Parse serprog options from `key=value` pairs.
///
/// Accepted keys: `dev`, `ip`, `baud`, `spispeed`, `cs`. Unknown keys are
/// rejected so frontends cannot silently drop configuration.
pub fn parse_options(options: &[(&str, &str)]) -> std::result::Result<SerprogConfig, String> {
    let mut config = SerprogConfig::default();
    for (key, value) in options {
        match *key {
            "dev" => config.dev = Some(value.to_string()),
            "ip" => config.ip = Some(value.to_string()),
            "baud" => {
                let baud: u32 = value
                    .parse()
                    .map_err(|_| format!("Invalid baud value: {value}"))?;
                if !(SERPROG_BAUD_MIN..=SERPROG_BAUD_MAX).contains(&baud) {
                    return Err(format!(
                        "Invalid baud value: {value} (must be {SERPROG_BAUD_MIN}-{SERPROG_BAUD_MAX})"
                    ));
                }
                config.baud = Some(baud);
            }
            "spispeed" => {
                config.spispeed_khz = Some(
                    crate::catalog::parse_speed_khz(value)
                        .ok_or_else(|| format!("Invalid spispeed value: {value}"))?,
                );
            }
            "cs" => {
                config.cs = Some(
                    value
                        .parse()
                        .map_err(|_| format!("Invalid cs value: {value}"))?,
                );
            }
            _ => return Err(format!("unknown option: {key}")),
        }
    }
    Ok(config)
}

/// Connection options for serprog
#[cfg(feature = "serprog-native")]
#[derive(Debug, Clone)]
pub enum SerprogConnection {
    /// Serial port connection
    Serial {
        /// Device path (e.g., "/dev/ttyUSB0" or "COM1")
        device: String,
        /// Baud rate (None for hardware default)
        baud: Option<u32>,
    },
    /// TCP socket connection
    Tcp {
        /// Hostname or IP address
        host: String,
        /// Port number
        port: u16,
    },
}

#[cfg(feature = "serprog-native")]
impl SerprogConnection {
    /// Parse a connection string
    ///
    /// Formats:
    /// - `dev=/dev/ttyUSB0` - Serial with default baud
    /// - `dev=/dev/ttyUSB0:115200` - Serial with specified baud
    /// - `ip=host:port` - TCP connection
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        if let Some(dev) = s.strip_prefix("dev=") {
            // Serial connection
            if let Some((device, baud_str)) = dev.rsplit_once(':') {
                let baud = baud_str
                    .parse()
                    .map_err(|_| format!("Invalid baud rate: {}", baud_str))?;
                Ok(SerprogConnection::Serial {
                    device: device.to_string(),
                    baud: Some(baud),
                })
            } else {
                Ok(SerprogConnection::Serial {
                    device: dev.to_string(),
                    baud: None,
                })
            }
        } else if let Some(ip) = s.strip_prefix("ip=") {
            // TCP connection
            let (host, port_str) = ip
                .rsplit_once(':')
                .ok_or_else(|| "Missing port in ip= parameter".to_string())?;
            let port = port_str
                .parse()
                .map_err(|_| format!("Invalid port: {}", port_str))?;
            Ok(SerprogConnection::Tcp {
                host: host.to_string(),
                port,
            })
        } else {
            Err(format!(
                "Invalid serprog connection string: {}. Use dev=... or ip=...",
                s
            ))
        }
    }
}

/// Open a serprog connection via serial port
#[cfg(feature = "serprog-native")]
pub async fn open_serial(device: &str, baud: Option<u32>) -> Result<Serprog<SerialTransport>> {
    let transport = SerialTransport::open(device, baud)?;
    Serprog::new(transport).await
}

/// Open a serprog connection via TCP
#[cfg(feature = "serprog-native")]
pub async fn open_tcp(host: &str, port: u16) -> Result<Serprog<TcpTransport>> {
    let transport = TcpTransport::connect(host, port)?;
    Serprog::new(transport).await
}

#[cfg(all(test, feature = "serprog-native"))]
mod connection_tests {
    use super::*;

    fn serial_parts(config: &SerprogConfig) -> (String, Option<u32>) {
        match config.connection().unwrap() {
            SerprogConnection::Serial { device, baud } => (device, baud),
            other => panic!("expected serial connection, got {other:?}"),
        }
    }

    #[test]
    fn dev_with_embedded_baud_is_split() {
        let config = parse_options(&[("dev", "/dev/ttyUSB0:115200")]).unwrap();
        assert_eq!(
            serial_parts(&config),
            ("/dev/ttyUSB0".to_string(), Some(115200))
        );
    }

    #[test]
    fn explicit_baud_is_used_and_overrides_an_embedded_one() {
        let explicit = parse_options(&[("dev", "/dev/ttyUSB0"), ("baud", "9600")]).unwrap();
        assert_eq!(
            serial_parts(&explicit),
            ("/dev/ttyUSB0".to_string(), Some(9600))
        );

        let override_embedded =
            parse_options(&[("dev", "/dev/ttyUSB0:115200"), ("baud", "9600")]).unwrap();
        assert_eq!(
            serial_parts(&override_embedded),
            ("/dev/ttyUSB0".to_string(), Some(9600))
        );
    }

    #[test]
    fn ip_builds_a_tcp_connection() {
        let config = parse_options(&[("ip", "example.test:1234")]).unwrap();
        match config.connection().unwrap() {
            SerprogConnection::Tcp { host, port } => {
                assert_eq!(host, "example.test");
                assert_eq!(port, 1234);
            }
            other => panic!("expected tcp connection, got {other:?}"),
        }
    }

    #[test]
    fn conflicting_or_missing_connections_are_rejected() {
        let both = parse_options(&[("dev", "/dev/ttyUSB0"), ("ip", "h:1")]).unwrap();
        assert!(both.connection().is_err());

        let baud_with_ip = parse_options(&[("ip", "h:1"), ("baud", "9600")]).unwrap();
        assert!(baud_with_ip.connection().is_err());

        assert!(SerprogConfig::default().connection().is_err());
    }

    #[test]
    fn spispeed_accepts_suffixes_and_bad_values_are_rejected() {
        let config =
            parse_options(&[("dev", "/dev/ttyUSB0"), ("spispeed", "30m"), ("cs", "1")]).unwrap();
        assert_eq!(config.spispeed_khz, Some(30_000));
        assert_eq!(config.cs, Some(1));

        assert!(parse_options(&[("spispeed", "nope")]).is_err());
        assert!(parse_options(&[("cs", "nope")]).is_err());
        assert!(parse_options(&[("unknown", "1")]).is_err());
    }

    #[test]
    fn baud_must_stay_within_the_declared_range() {
        assert_eq!(
            parse_options(&[("baud", "115200")]).unwrap().baud,
            Some(115200)
        );
        assert_eq!(
            parse_options(&[("baud", &SERPROG_BAUD_MAX.to_string())])
                .unwrap()
                .baud,
            Some(SERPROG_BAUD_MAX)
        );
        assert!(parse_options(&[("baud", "0")]).is_err());
        assert!(parse_options(&[("baud", &(SERPROG_BAUD_MAX + 1).to_string())]).is_err());
        assert!(parse_options(&[("baud", "nope")]).is_err());
    }
}

// ---------------------------------------------------------------------------
// Option schema shared by the CLI and the web frontend.
// The table sits next to the parser it describes; `crate::catalog` only
// aggregates these modules for listing and validation.
// ---------------------------------------------------------------------------

pub mod schema {
    #[allow(unused_imports)]
    use crate::catalog::{Choice, OptionKind, OptionSpec, Scope, choice};

    pub const OPTIONS: &[OptionSpec] = &[
        OptionSpec {
            key: "dev",
            label: "Serial port",
            help: "Serial device (e.g. /dev/ttyUSB0); native frontends use the port picker",
            kind: OptionKind::Path,
            default: None,
            scope: Scope::NativeOnly,
        },
        OptionSpec {
            key: "ip",
            label: "Network address",
            help: "TCP endpoint host:port (native only)",
            kind: OptionKind::Text,
            default: None,
            scope: Scope::NativeOnly,
        },
        OptionSpec {
            key: "baud",
            label: "Baud rate",
            help: "Serial baud rate",
            kind: OptionKind::Int {
                min: super::SERPROG_BAUD_MIN as i64,
                max: super::SERPROG_BAUD_MAX as i64,
            },
            default: None,
            scope: Scope::Any,
        },
        OptionSpec {
            key: "spispeed",
            label: "SPI speed",
            help: "SPI clock in kHz (accepts k/m/g suffixes)",
            kind: OptionKind::SpeedKhz { presets: &[] },
            default: None,
            scope: Scope::Any,
        },
        OptionSpec {
            key: "cs",
            label: "Chip select",
            help: "Chip select to assert, if the programmer supports it",
            kind: OptionKind::Int { min: 0, max: 255 },
            default: None,
            scope: Scope::Any,
        },
    ];

    pub fn validate_options(options: &[(&str, &str)]) -> std::result::Result<(), String> {
        crate::serprog::parse_options(options).map(|_| ())
    }
}

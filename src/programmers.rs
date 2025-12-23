//! Programmer registration and dispatch
//!
//! This module provides a centralized registry for all programmers, with support
//! for feature-gated inclusion and dynamic help text generation.

use rflasher_core::programmer::SpiMaster;

/// Information about a programmer
pub struct ProgrammerInfo {
    /// Primary name (used for matching)
    pub name: &'static str,
    /// Alternative names/aliases
    pub aliases: &'static [&'static str],
    /// Short description
    pub description: &'static str,
    /// Whether this programmer is currently implemented
    pub implemented: bool,
}

/// Get information about all available programmers (enabled at compile time)
#[allow(unused_mut)]
pub fn available_programmers() -> Vec<ProgrammerInfo> {
    let mut programmers = Vec::new();

    #[cfg(feature = "dummy")]
    programmers.push(ProgrammerInfo {
        name: "dummy",
        aliases: &[],
        description: "In-memory flash emulator for testing",
        implemented: true,
    });

    #[cfg(feature = "ch341a")]
    programmers.push(ProgrammerInfo {
        name: "ch341a",
        aliases: &["ch341a_spi"],
        description: "CH341A USB SPI programmer (VID:1a86 PID:5512)",
        implemented: true,
    });

    #[cfg(feature = "serprog")]
    programmers.push(ProgrammerInfo {
        name: "serprog",
        aliases: &[],
        description: "Serial Flasher Protocol over serial/network (dev=<port> or ip=<host:port>)",
        implemented: true,
    });

    #[cfg(feature = "ftdi")]
    programmers.push(ProgrammerInfo {
        name: "ftdi",
        aliases: &["ft2232_spi", "ft4232_spi"],
        description: "FTDI MPSSE programmer (FT2232H/FT4232H/FT232H) (type=<dev>,port=<A-D>)",
        implemented: true,
    });

    #[cfg(feature = "linux-spi")]
    programmers.push(ProgrammerInfo {
        name: "linux_spi",
        aliases: &["linux-spi", "spidev"],
        description: "Linux spidev interface",
        implemented: false,
    });

    #[cfg(feature = "internal")]
    programmers.push(ProgrammerInfo {
        name: "internal",
        aliases: &[],
        description: "Intel chipset internal flash (ICH/PCH)",
        implemented: false,
    });

    programmers
}

/// Generate help text listing all available programmers
pub fn programmer_help() -> String {
    let programmers = available_programmers();

    if programmers.is_empty() {
        return "No programmers available (recompile with programmer features enabled)".to_string();
    }

    let mut help = String::from("Available programmers:\n");

    for p in &programmers {
        let status = if p.implemented {
            ""
        } else {
            " [not yet implemented]"
        };
        help.push_str(&format!("  {:12} - {}{}\n", p.name, p.description, status));
    }

    help
}

/// Generate a short list of programmer names for CLI help
pub fn programmer_names_short() -> String {
    let programmers = available_programmers();
    let names: Vec<&str> = programmers.iter().map(|p| p.name).collect();
    names.join(", ")
}

/// Check if a programmer name matches any available programmer
#[allow(unused_variables)]
pub fn find_programmer(name: &str) -> Option<&'static str> {
    // This is a bit tricky since we can't return references to local data
    // We'll match against known names directly

    #[cfg(feature = "dummy")]
    if name == "dummy" {
        return Some("dummy");
    }

    #[cfg(feature = "ch341a")]
    if name == "ch341a" || name == "ch341a_spi" {
        return Some("ch341a");
    }

    #[cfg(feature = "serprog")]
    if name == "serprog" {
        return Some("serprog");
    }

    #[cfg(feature = "ftdi")]
    if name == "ftdi" || name == "ft2232_spi" || name == "ft4232_spi" {
        return Some("ftdi");
    }

    #[cfg(feature = "linux-spi")]
    if name == "linux_spi" || name == "linux-spi" || name == "spidev" {
        return Some("linux_spi");
    }

    #[cfg(feature = "internal")]
    if name == "internal" {
        return Some("internal");
    }

    None
}

/// Execute a function with the specified programmer
///
/// The programmer string can be just the name (e.g., "ch341a") or include
/// parameters (e.g., "ch341a:index=1").
#[allow(unused_variables)]
pub fn with_programmer<F>(programmer: &str, f: F) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnOnce(&mut dyn SpiMaster) -> Result<(), Box<dyn std::error::Error>>,
{
    // Parse programmer name and options
    let (name, _options) = parse_programmer_string(programmer);

    // First check if the programmer is available at all
    let canonical_name = match find_programmer(name) {
        Some(n) => n,
        None => {
            return Err(unknown_programmer_error(name));
        }
    };

    // Dispatch to the appropriate programmer
    match canonical_name {
        #[cfg(feature = "dummy")]
        "dummy" => {
            let mut master = rflasher_dummy::DummyFlash::new_default();
            f(&mut master)
        }

        #[cfg(feature = "ch341a")]
        "ch341a" => {
            log::info!("Opening CH341A programmer...");
            let mut master = rflasher_ch341a::Ch341a::open().map_err(|e| {
                format!(
                    "Failed to open CH341A: {}\nMake sure the device is connected and you have permissions.",
                    e
                )
            })?;
            f(&mut master)
        }

        #[cfg(feature = "serprog")]
        "serprog" => {
            use rflasher_serprog::SerprogConnection;

            // Parse options
            let (_, options) = parse_programmer_string(programmer);

            // Build connection string from options (only dev= or ip=)
            let conn_str = options
                .iter()
                .filter(|(k, _)| *k == "dev" || *k == "ip")
                .map(|(k, v)| format!("{}={}", k, v))
                .collect::<Vec<_>>()
                .join(",");

            if conn_str.is_empty() {
                return Err("serprog requires connection parameters.\n\
                    Usage: serprog:dev=/dev/ttyUSB0[:baud] or serprog:ip=host:port\n\
                    Optional: spispeed=<hz>, cs=<num>"
                    .into());
            }

            // Parse connection
            let conn = SerprogConnection::parse(&conn_str)
                .map_err(|e| format!("Invalid serprog parameters: {}", e))?;

            // Extract optional parameters
            let spispeed: Option<u32> = options
                .iter()
                .find(|(k, _)| *k == "spispeed")
                .and_then(|(_, v)| v.parse().ok());
            let cs: Option<u8> = options
                .iter()
                .find(|(k, _)| *k == "cs")
                .and_then(|(_, v)| v.parse().ok());

            log::info!("Opening serprog programmer...");

            match conn {
                SerprogConnection::Serial { device, baud } => {
                    let transport = rflasher_serprog::SerialTransport::open(&device, baud)
                        .map_err(|e| format!("Failed to open serial port {}: {}", device, e))?;
                    let mut serprog = rflasher_serprog::Serprog::new(transport)
                        .map_err(|e| format!("Failed to initialize serprog: {}", e))?;

                    // Apply optional settings
                    if let Some(speed) = spispeed {
                        if let Err(e) = serprog.set_spi_speed(speed) {
                            log::warn!("Failed to set SPI speed: {}", e);
                        }
                    }
                    if let Some(chip_select) = cs {
                        serprog
                            .set_spi_cs(chip_select)
                            .map_err(|e| format!("Failed to set chip select: {}", e))?;
                    }

                    f(&mut serprog)
                }
                SerprogConnection::Tcp { host, port } => {
                    let transport = rflasher_serprog::TcpTransport::connect(&host, port)
                        .map_err(|e| format!("Failed to connect to {}:{}: {}", host, port, e))?;
                    let mut serprog = rflasher_serprog::Serprog::new(transport)
                        .map_err(|e| format!("Failed to initialize serprog: {}", e))?;

                    // Apply optional settings
                    if let Some(speed) = spispeed {
                        if let Err(e) = serprog.set_spi_speed(speed) {
                            log::warn!("Failed to set SPI speed: {}", e);
                        }
                    }
                    if let Some(chip_select) = cs {
                        serprog
                            .set_spi_cs(chip_select)
                            .map_err(|e| format!("Failed to set chip select: {}", e))?;
                    }

                    f(&mut serprog)
                }
            }
        }

        #[cfg(feature = "ftdi")]
        "ftdi" => {
            use rflasher_ftdi::{parse_options, Ftdi};

            // Parse options
            let (_, options) = parse_programmer_string(programmer);

            log::info!("Opening FTDI programmer...");

            // Parse configuration from options
            let config =
                parse_options(&options).map_err(|e| format!("Invalid FTDI parameters: {}", e))?;

            let mut master = Ftdi::open(&config).map_err(|e| {
                format!(
                    "Failed to open FTDI device: {}\n\
                     Make sure the device is connected and you have permissions.\n\
                     You may need to unbind the kernel ftdi_sio driver:\n\
                     echo -n '<bus>-<port>' | sudo tee /sys/bus/usb/drivers/ftdi_sio/unbind",
                    e
                )
            })?;

            f(&mut master)
        }

        #[cfg(feature = "linux-spi")]
        "linux_spi" => Err("linux_spi programmer is not yet implemented".into()),

        #[cfg(feature = "internal")]
        "internal" => Err("internal programmer is not yet implemented".into()),

        _ => Err(unknown_programmer_error(name)),
    }
}

/// Parse a programmer string into name and options
///
/// Format: "name" or "name:option1=value1,option2=value2"
pub fn parse_programmer_string(s: &str) -> (&str, Vec<(&str, &str)>) {
    if let Some((name, opts)) = s.split_once(':') {
        let options: Vec<_> = opts
            .split(',')
            .filter_map(|opt| opt.split_once('='))
            .collect();
        (name, options)
    } else {
        (s, Vec::new())
    }
}

fn unknown_programmer_error(name: &str) -> Box<dyn std::error::Error> {
    let mut msg = format!("Unknown programmer: {}\n\n", name);
    msg.push_str(&programmer_help());
    msg.push_str("\nUse 'rflasher list-programmers' for more details");
    msg.into()
}

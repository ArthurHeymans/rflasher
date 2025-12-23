//! Programmer registry and initialization
//!
//! This module handles opening programmers by name and creating FlashHandles.
//! It completely hides SpiMaster and OpaqueMaster from the public API.

use crate::handle::{ChipInfo, FlashHandle};
use rflasher_core::chip::ChipDatabase;
#[allow(unused_imports)] // Used in feature-gated code
use rflasher_core::flash::FlashDevice;
use rflasher_core::flash::{OpaqueFlashDevice, SpiFlashDevice};
use rflasher_core::layout::parse_ifd;
use rflasher_core::programmer::OpaqueMaster;
use std::collections::HashMap;

/// Parsed programmer parameters
pub struct ProgrammerParams {
    /// Programmer name (canonical)
    pub name: String,
    /// Key-value parameters
    pub params: HashMap<String, String>,
}

/// Parse a programmer string into name and parameters
///
/// Format: "name" or "name:key1=value1,key2=value2"
///
/// # Example
/// ```ignore
/// let params = parse_programmer_params("ch341a:index=1")?;
/// assert_eq!(params.name, "ch341a");
/// assert_eq!(params.params.get("index"), Some(&"1".to_string()));
/// ```
pub fn parse_programmer_params(s: &str) -> Result<ProgrammerParams, Box<dyn std::error::Error>> {
    let (name, opts_str) = s.split_once(':').unwrap_or((s, ""));

    let mut params = HashMap::new();
    if !opts_str.is_empty() {
        for opt in opts_str.split(',') {
            if let Some((key, value)) = opt.split_once('=') {
                params.insert(key.to_string(), value.to_string());
            } else {
                return Err(
                    format!("Invalid parameter format: '{}' (expected key=value)", opt).into(),
                );
            }
        }
    }

    Ok(ProgrammerParams {
        name: name.to_string(),
        params,
    })
}

/// Open a flash programmer and create a FlashHandle
///
/// This is the main entry point for the CLI. It handles:
/// 1. Parsing the programmer string
/// 2. Opening the appropriate programmer
/// 3. Probing the chip (for SPI) or determining flash size (for opaque)
/// 4. Creating a unified FlashHandle
///
/// # Arguments
/// * `programmer` - Programmer specification (e.g., "ch341a" or "serprog:dev=/dev/ttyUSB0")
/// * `db` - Chip database for JEDEC ID lookup
///
/// # Returns
/// A FlashHandle that abstracts over the programmer type
///
/// # Example
/// ```ignore
/// let db = ChipDatabase::new();
/// let mut handle = open_flash("ch341a", &db)?;
///
/// // Use the handle - works the same for all programmer types
/// let size = handle.size();
/// println!("Flash size: {} bytes", size);
/// ```
pub fn open_flash(
    programmer: &str,
    db: &ChipDatabase,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    let params = parse_programmer_params(programmer)?;

    match params.name.as_str() {
        #[cfg(feature = "dummy")]
        "dummy" => open_dummy(db),

        #[cfg(feature = "ch341a")]
        "ch341a" | "ch341a_spi" => open_ch341a(&params, db),

        #[cfg(feature = "serprog")]
        "serprog" => open_serprog(&params, db),

        #[cfg(feature = "ftdi")]
        "ftdi" | "ft2232_spi" | "ft4232_spi" => open_ftdi(&params, db),

        #[cfg(feature = "linux-spi")]
        "linux_spi" | "linux-spi" | "spidev" => open_linux_spi(&params, db),

        #[cfg(feature = "internal")]
        "internal" => open_internal(&params, db),

        _ => Err(format!("Unknown programmer: {}", params.name).into()),
    }
}

// Helper to get flash size from IFD for opaque programmers
fn get_flash_size_from_ifd(
    master: &mut dyn OpaqueMaster,
) -> Result<u32, Box<dyn std::error::Error>> {
    let mut header = [0u8; 4096];
    master.read(0, &mut header)?;

    if let Ok(layout) = parse_ifd(&header) {
        let size = layout.regions.iter().map(|r| r.end + 1).max().unwrap_or(0);
        if size > 0 {
            return Ok(size);
        }
    }

    let size = master.size();
    if size > 0 {
        return Ok(size as u32);
    }

    Err("Cannot determine flash size".into())
}

// Programmer-specific open functions
// These handle the details of each programmer type and return a FlashHandle

#[cfg(feature = "dummy")]
fn open_dummy(db: &ChipDatabase) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    let mut master = rflasher_dummy::DummyFlash::new_default();
    let ctx = rflasher_core::flash::probe(&mut master, db)?;

    log::info!(
        "Found: {} {} ({} bytes)",
        ctx.chip.vendor,
        ctx.chip.name,
        ctx.chip.total_size
    );

    let chip_info = ChipInfo::from(&ctx);
    let device = SpiFlashDevice::new_owned(master, ctx);
    Ok(FlashHandle::with_chip_info(Box::new(device), chip_info))
}

#[cfg(feature = "ch341a")]
fn open_ch341a(
    _params: &ProgrammerParams,
    db: &ChipDatabase,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    log::info!("Opening CH341A programmer...");

    let mut master = rflasher_ch341a::Ch341a::open().map_err(|e| {
        format!(
            "Failed to open CH341A: {}\nMake sure the device is connected and you have permissions.",
            e
        )
    })?;

    let ctx = rflasher_core::flash::probe(&mut master, db)?;
    log::info!(
        "Found: {} {} ({} bytes)",
        ctx.chip.vendor,
        ctx.chip.name,
        ctx.chip.total_size
    );

    let chip_info = ChipInfo::from(&ctx);
    let device = SpiFlashDevice::new_owned(master, ctx);
    Ok(FlashHandle::with_chip_info(Box::new(device), chip_info))
}

#[cfg(feature = "serprog")]
fn open_serprog(
    params: &ProgrammerParams,
    db: &ChipDatabase,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    use rflasher_serprog::SerprogConnection;

    log::info!("Opening serprog programmer...");

    // Build connection string from dev= or ip= parameters
    let conn_str = params
        .params
        .iter()
        .filter(|(k, _)| *k == "dev" || *k == "ip")
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join(",");

    if conn_str.is_empty() {
        return Err("serprog requires connection parameters.\n\
             Usage: serprog:dev=/dev/ttyUSB0[:baud] or serprog:ip=host:port"
            .into());
    }

    let conn = SerprogConnection::parse(&conn_str)
        .map_err(|e| format!("Invalid serprog parameters: {}", e))?;

    // Parse optional parameters
    let spispeed: Option<u32> = params.params.get("spispeed").and_then(|v| v.parse().ok());
    let cs: Option<u8> = params.params.get("cs").and_then(|v| v.parse().ok());

    // Open connection and create device with concrete type
    match conn {
        SerprogConnection::Serial { device, baud } => {
            let transport = rflasher_serprog::SerialTransport::open(&device, baud)
                .map_err(|e| format!("Failed to open serial port {}: {}", device, e))?;
            let mut serprog = rflasher_serprog::Serprog::new(transport)
                .map_err(|e| format!("Failed to initialize serprog: {}", e))?;

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

            let ctx = rflasher_core::flash::probe(&mut serprog, db)?;
            log::info!(
                "Found: {} {} ({} bytes)",
                ctx.chip.vendor,
                ctx.chip.name,
                ctx.chip.total_size
            );

            let chip_info = ChipInfo::from(&ctx);
            let device = SpiFlashDevice::new_owned(serprog, ctx);
            Ok(FlashHandle::with_chip_info(Box::new(device), chip_info))
        }
        SerprogConnection::Tcp { host, port } => {
            let transport = rflasher_serprog::TcpTransport::connect(&host, port)
                .map_err(|e| format!("Failed to connect to {}:{}: {}", host, port, e))?;
            let mut serprog = rflasher_serprog::Serprog::new(transport)
                .map_err(|e| format!("Failed to initialize serprog: {}", e))?;

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

            let ctx = rflasher_core::flash::probe(&mut serprog, db)?;
            log::info!(
                "Found: {} {} ({} bytes)",
                ctx.chip.vendor,
                ctx.chip.name,
                ctx.chip.total_size
            );

            let chip_info = ChipInfo::from(&ctx);
            let device = SpiFlashDevice::new_owned(serprog, ctx);
            Ok(FlashHandle::with_chip_info(Box::new(device), chip_info))
        }
    }
}

#[cfg(feature = "ftdi")]
fn open_ftdi(
    params: &ProgrammerParams,
    db: &ChipDatabase,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    use rflasher_ftdi::{parse_options, Ftdi};

    log::info!("Opening FTDI programmer...");

    // Convert HashMap to Vec<(&str, &str)> for parse_options
    let options: Vec<(&str, &str)> = params
        .params
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let config = parse_options(&options).map_err(|e| format!("Invalid FTDI parameters: {}", e))?;

    let mut master = Ftdi::open(&config).map_err(|e| {
        format!(
            "Failed to open FTDI device: {}\n\
             Make sure the device is connected and you have permissions.\n\
             You may need to unbind the kernel ftdi_sio driver:\n\
             echo -n '<bus>-<port>' | sudo tee /sys/bus/usb/drivers/ftdi_sio/unbind",
            e
        )
    })?;

    let ctx = rflasher_core::flash::probe(&mut master, db)?;
    log::info!(
        "Found: {} {} ({} bytes)",
        ctx.chip.vendor,
        ctx.chip.name,
        ctx.chip.total_size
    );

    let chip_info = ChipInfo::from(&ctx);
    let device = SpiFlashDevice::new_owned(master, ctx);
    Ok(FlashHandle::with_chip_info(Box::new(device), chip_info))
}

#[cfg(feature = "linux-spi")]
fn open_linux_spi(
    params: &ProgrammerParams,
    db: &ChipDatabase,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    use rflasher_linux_spi::{parse_options, LinuxSpi};

    log::info!("Opening Linux SPI programmer...");

    let options: Vec<(&str, &str)> = params
        .params
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let config =
        parse_options(&options).map_err(|e| format!("Invalid linux_spi parameters: {}", e))?;

    let mut master = LinuxSpi::open(&config).map_err(|e| {
        format!(
            "Failed to open Linux SPI device: {}\n\
             Make sure the device exists and you have read/write permissions.\n\
             You may need to: sudo usermod -aG spi $USER",
            e
        )
    })?;

    let ctx = rflasher_core::flash::probe(&mut master, db)?;
    log::info!(
        "Found: {} {} ({} bytes)",
        ctx.chip.vendor,
        ctx.chip.name,
        ctx.chip.total_size
    );

    let chip_info = ChipInfo::from(&ctx);
    let device = SpiFlashDevice::new_owned(master, ctx);
    Ok(FlashHandle::with_chip_info(Box::new(device), chip_info))
}

#[cfg(feature = "internal")]
fn open_internal(
    params: &ProgrammerParams,
    db: &ChipDatabase,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    use rflasher_internal::{InternalOptions, InternalProgrammer, SpiMode};

    log::info!("Opening internal programmer...");

    let options: Vec<(&str, &str)> = params
        .params
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let internal_opts = InternalOptions::from_options(&options)
        .map_err(|e| format!("Invalid internal programmer options: {}", e))?;

    if internal_opts.mode != SpiMode::Auto {
        log::info!("Using ich_spi_mode={}", internal_opts.mode);
    }

    let mut programmer = InternalProgrammer::with_options(internal_opts).map_err(|e| {
        format!(
            "Failed to initialize internal programmer: {}\n\
             Make sure you have root privileges and a supported Intel or AMD chipset.\n\
             For Intel ICH7, only swseq is supported.\n\
             For Intel PCH100+, swseq may be locked (use ich_spi_mode=hwseq).",
            e
        )
    })?;

    // Software sequencing: can probe chip via SPI
    // Hardware sequencing: opaque operations only
    if programmer.mode() == SpiMode::SoftwareSequencing {
        log::info!("Using SPI mode (swseq allows chip probing)");
        let ctx = rflasher_core::flash::probe(&mut programmer, db)?;
        log::info!(
            "Found: {} {} ({} bytes)",
            ctx.chip.vendor,
            ctx.chip.name,
            ctx.chip.total_size
        );

        let chip_info = ChipInfo::from(&ctx);
        let device = SpiFlashDevice::new(&mut programmer, ctx);
        Ok(FlashHandle::with_chip_info(Box::new(device), chip_info))
    } else {
        log::info!("Using opaque mode (hwseq - no chip probing available)");
        let flash_size = get_flash_size_from_ifd(&mut programmer)?;
        log::info!("Flash size: {} bytes (from IFD)", flash_size);

        let device = OpaqueFlashDevice::new_owned(programmer, flash_size);
        Ok(FlashHandle::without_chip_info(Box::new(device)))
    }
}

// Programmer information and listing
/// Information about a programmer
pub struct ProgrammerInfo {
    /// Primary name (used for matching)
    pub name: &'static str,
    /// Alternative names/aliases
    pub aliases: &'static [&'static str],
    /// Short description
    pub description: &'static str,
}

/// Get information about all available programmers (enabled at compile time)
#[allow(unused_mut, clippy::vec_init_then_push)]
pub fn available_programmers() -> Vec<ProgrammerInfo> {
    let mut programmers = Vec::new();

    #[cfg(feature = "dummy")]
    programmers.push(ProgrammerInfo {
        name: "dummy",
        aliases: &[],
        description: "In-memory flash emulator for testing",
    });

    #[cfg(feature = "ch341a")]
    programmers.push(ProgrammerInfo {
        name: "ch341a",
        aliases: &["ch341a_spi"],
        description: "CH341A USB SPI programmer (VID:1a86 PID:5512)",
    });

    #[cfg(feature = "serprog")]
    programmers.push(ProgrammerInfo {
        name: "serprog",
        aliases: &[],
        description: "Serial Flasher Protocol over serial/network (dev=<port> or ip=<host:port>)",
    });

    #[cfg(feature = "ftdi")]
    programmers.push(ProgrammerInfo {
        name: "ftdi",
        aliases: &["ft2232_spi", "ft4232_spi"],
        description: "FTDI MPSSE programmer (FT2232H/FT4232H/FT232H) (type=<dev>,port=<A-D>)",
    });

    #[cfg(feature = "linux-spi")]
    programmers.push(ProgrammerInfo {
        name: "linux_spi",
        aliases: &["linux-spi", "spidev"],
        description: "Linux SPI device via spidev interface (dev=/dev/spidevX.Y)",
    });

    #[cfg(feature = "internal")]
    programmers.push(ProgrammerInfo {
        name: "internal",
        aliases: &[],
        description: "Intel PCH internal SPI/FWH controller (ich_spi_mode=<auto|swseq|hwseq>)",
    });

    programmers
}

/// Generate a short list of programmer names for CLI help
pub fn programmer_names_short() -> String {
    let programmers = available_programmers();
    if programmers.is_empty() {
        return "none (recompile with features)".to_string();
    }
    let names: Vec<&str> = programmers.iter().map(|p| p.name).collect();
    names.join(", ")
}

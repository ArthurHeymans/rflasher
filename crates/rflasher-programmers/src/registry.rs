//! Programmer registry and initialization
//!
//! This module handles opening programmers by name and creating FlashHandles.
//! It completely hides SpiMaster and OpaqueMaster from the public API.

#[allow(unused_imports)] // Only used when at least one programmer backend is enabled
use crate::catalog::{ProgrammerParams, parse_programmer_params};
use crate::erased::{ErasedFlashDevice, ErasedSpiMaster};
use crate::handle::{ChipInfo, FlashHandle};
use rflasher_core::chip::ChipProvider;
#[allow(unused_imports)] // Used in feature-gated code
use rflasher_core::flash::FlashDevice;
#[cfg(any(feature = "dediprog", feature = "sunxi-fel"))]
use rflasher_core::flash::HybridFlashDevice;
#[cfg(any(feature = "linux-mtd", feature = "internal"))]
use rflasher_core::flash::OpaqueFlashDevice;
use rflasher_core::flash::{ProbeResult, SpiFlashDevice, probe_detailed};
#[cfg(feature = "internal")]
use rflasher_core::layout::parse_ifd;
#[cfg(feature = "internal")]
use rflasher_core::programmer::OpaqueMaster;
use rflasher_core::sfdp::SfdpMismatch;

/// Log any SFDP mismatches as warnings
fn log_sfdp_mismatches(mismatches: &[SfdpMismatch], chip_name: &str) {
    if mismatches.is_empty() {
        return;
    }

    log::warn!(
        "SFDP data for {} differs from database ({} mismatch{}):",
        chip_name,
        mismatches.len(),
        if mismatches.len() == 1 { "" } else { "es" }
    );

    for mismatch in mismatches {
        // Critical mismatches (size, page size) get ERROR level
        match mismatch {
            SfdpMismatch::TotalSize { sfdp, database } => {
                log::error!("  CRITICAL: {}", mismatch);
                log::error!(
                    "    This may cause data corruption! SFDP says {} bytes, DB says {} bytes",
                    sfdp,
                    database
                );
            }
            SfdpMismatch::PageSize { sfdp, database } => {
                log::error!("  CRITICAL: {}", mismatch);
                log::error!(
                    "    This may cause write failures! SFDP says {} bytes, DB says {} bytes",
                    sfdp,
                    database
                );
            }
            _ => {
                log::warn!("  {}", mismatch);
            }
        }
    }
}

/// Log info about chip detection source
fn log_probe_result(result: &ProbeResult) {
    if result.from_database {
        log::info!(
            "Found: {} {} ({} bytes) [from database]",
            result.chip.vendor,
            result.chip.name,
            result.chip.total_size
        );
        if result.sfdp.is_some() {
            log::debug!("SFDP data also available for verification");
        }
    } else {
        log::info!(
            "Found: JEDEC {:02X}:{:04X} ({} bytes) [from SFDP - not in database]",
            result.jedec_manufacturer,
            result.jedec_device,
            result.chip.total_size
        );
        log::warn!(
            "Chip not in database, using SFDP parameters. Consider adding to chip database."
        );
    }

    log_sfdp_mismatches(&result.mismatches, &result.chip.name);
}

/// Common probe and create handle logic for SPI programmers
async fn probe_and_create_handle<M>(
    master: M,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>>
where
    M: rflasher_core::programmer::SpiMaster + Send + 'static,
{
    let mut master = master;
    let result = probe_detailed(&mut master, db).await?;

    log_probe_result(&result);

    let chip_info = ChipInfo::from(result);
    let ctx = rflasher_core::flash::FlashContext::new(chip_info.chip.clone());
    let device = SpiFlashDevice::new(master, ctx);
    Ok(FlashHandle::with_chip_info(
        ErasedFlashDevice::new(device),
        chip_info,
    ))
}

/// A type-erased SPI master for use with the REPL
pub type BoxedSpiMaster = ErasedSpiMaster;

/// Open a raw SPI programmer without the FlashDevice wrapper
///
/// This is used by the REPL to get direct access to SPI commands.
/// Only SPI-based programmers are supported (not opaque ones like internal in hwseq mode).
///
/// # Arguments
/// * `programmer` - Programmer specification (e.g., "ch341a" or "serprog:dev=/dev/ttyUSB0")
///
/// # Returns
/// A boxed SpiMaster that can execute raw SPI commands
pub async fn open_spi_programmer(
    programmer: &str,
) -> Result<BoxedSpiMaster, Box<dyn std::error::Error>> {
    let params = parse_programmer_params(programmer)?;

    match params.name.as_str() {
        #[cfg(feature = "dummy")]
        "dummy" => {
            Ok(ErasedSpiMaster::new(open_dummy_master(&params).await?))
        }

        #[cfg(feature = "ch341a")]
        "ch341a" | "ch341a_spi" => {
            Ok(ErasedSpiMaster::new(open_ch341a_master(&params).await?))
        }

        #[cfg(feature = "ch347")]
        "ch347" | "ch347_spi" => {
            Ok(ErasedSpiMaster::new(open_ch347_master(&params).await?))
        }

        #[cfg(feature = "dediprog")]
        "dediprog" | "dediprog_spi" => {
            Ok(ErasedSpiMaster::new(open_dediprog_master(&params).await?))
        }

        #[cfg(feature = "serprog-native")]
        "serprog" => {
            let config = parse_serprog_config(&params)?;
            match config.connection()? {
                crate::serprog::SerprogConnection::Serial { device, baud } => {
                    Ok(ErasedSpiMaster::new(open_serprog_serial(&device, baud, &config).await?))
                }
                crate::serprog::SerprogConnection::Tcp { host, port } => {
                    Ok(ErasedSpiMaster::new(open_serprog_tcp(&host, port, &config).await?))
                }
            }
        }

        #[cfg(feature = "ftdi")]
        "ftdi" | "ft2232_spi" | "ft4232_spi" => {
            Ok(ErasedSpiMaster::new(open_ftdi_master(&params).await?))
        }

        #[cfg(feature = "ft4222")]
        "ft4222" | "ft4222_spi" => {
            Ok(ErasedSpiMaster::new(open_ft4222_master(&params).await?))
        }

        #[cfg(feature = "linux-spi")]
        "linux_spi" | "linux-spi" | "spidev" => {
            Ok(ErasedSpiMaster::new(open_linux_spi_master(&params).await?))
        }

        #[cfg(feature = "linux-gpio")]
        "linux_gpio_spi" | "linux-gpio-spi" | "linux_gpio" | "linux-gpio" => {
            Ok(ErasedSpiMaster::new(open_linux_gpio_spi_master(&params).await?))
        }

        #[cfg(feature = "raiden")]
        "raiden_debug_spi" | "raiden" | "raiden_spi" => {
            Ok(ErasedSpiMaster::new(open_raiden_master(&params).await?))
        }

        #[cfg(feature = "sunxi-fel")]
        "sunxi_fel" | "sunxi-fel" | "fel" => {
            Ok(ErasedSpiMaster::new(open_sunxi_fel_master(&params).await?))
        }

        // Internal and MTD are opaque-only or not SPI-based
        #[cfg(feature = "internal")]
        "internal" => {
            Err("The REPL is only supported for SPI-based programmers. \
                 The internal programmer in hardware sequencing mode doesn't support raw SPI commands. \
                 If you need raw SPI access, try internal:ich_spi_mode=swseq (if supported).".into())
        }

        #[cfg(feature = "linux-mtd")]
        "linux_mtd" | "linux-mtd" | "mtd" => {
            Err("The REPL is only supported for SPI-based programmers. \
                 The linux_mtd programmer uses the MTD subsystem and doesn't expose raw SPI.".into())
        }

        _ => Err(format!("Unknown programmer: {}", params.name).into()),
    }
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
pub async fn open_flash(
    programmer: &str,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    // Not every compiled programmer arm uses the database (e.g. linux-mtd);
    // in feature combinations where none does, this suppresses the unused
    // `db` parameter warning.
    let _ = db;
    let params = parse_programmer_params(programmer)?;

    match params.name.as_str() {
        #[cfg(feature = "dummy")]
        "dummy" => open_dummy(&params, db).await,

        #[cfg(feature = "ch341a")]
        "ch341a" | "ch341a_spi" => open_ch341a(&params, db).await,

        #[cfg(feature = "ch347")]
        "ch347" | "ch347_spi" => open_ch347(&params, db).await,

        #[cfg(feature = "dediprog")]
        "dediprog" | "dediprog_spi" => open_dediprog(&params, db).await,

        #[cfg(feature = "serprog-native")]
        "serprog" => open_serprog(&params, db).await,

        #[cfg(feature = "ftdi")]
        "ftdi" | "ft2232_spi" | "ft4232_spi" => open_ftdi(&params, db).await,

        #[cfg(feature = "ft4222")]
        "ft4222" | "ft4222_spi" => open_ft4222(&params, db).await,

        #[cfg(feature = "linux-spi")]
        "linux_spi" | "linux-spi" | "spidev" => open_linux_spi(&params, db).await,

        #[cfg(feature = "linux-mtd")]
        "linux_mtd" | "linux-mtd" | "mtd" => open_linux_mtd(&params).await,

        #[cfg(feature = "linux-gpio")]
        "linux_gpio_spi" | "linux-gpio-spi" | "linux_gpio" | "linux-gpio" => {
            open_linux_gpio_spi(&params, db).await
        }

        #[cfg(feature = "internal")]
        "internal" => open_internal(&params, db).await,

        #[cfg(feature = "raiden")]
        "raiden_debug_spi" | "raiden" | "raiden_spi" => open_raiden(&params, db).await,

        #[cfg(feature = "sunxi-fel")]
        "sunxi_fel" | "sunxi-fel" | "fel" => open_sunxi_fel(&params, db).await,

        _ => Err(format!("Unknown programmer: {}", params.name).into()),
    }
}

// Helper to get flash size from IFD for opaque programmers
#[cfg(feature = "internal")]
async fn get_flash_size_from_ifd<M: OpaqueMaster>(
    master: &mut M,
) -> Result<u32, Box<dyn std::error::Error>> {
    let mut header = [0u8; 4096];
    master.read(0, &mut header).await?;

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
async fn open_dummy_master(
    _params: &ProgrammerParams,
) -> Result<crate::dummy::DummyFlash, Box<dyn std::error::Error>> {
    let master = crate::dummy::DummyFlash::new_default();
    Ok(master)
}

#[cfg(feature = "dummy")]
async fn open_dummy(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    probe_and_create_handle(open_dummy_master(params).await?, db).await
}

#[cfg(feature = "ch341a")]
async fn open_ch341a_master(
    _params: &ProgrammerParams,
) -> Result<crate::ch341a::Ch341a, Box<dyn std::error::Error>> {
    log::info!("Opening CH341A programmer...");

    let master = crate::ch341a::Ch341a::open().await.map_err(|e| {
        format!(
            "Failed to open CH341A: {}\nMake sure the device is connected and you have permissions.",
            e
        )
    })?;

    Ok(master)
}

#[cfg(feature = "ch341a")]
async fn open_ch341a(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    probe_and_create_handle(open_ch341a_master(params).await?, db).await
}

#[cfg(feature = "ch347")]
async fn open_ch347_master(
    params: &ProgrammerParams,
) -> Result<crate::ch347::Ch347, Box<dyn std::error::Error>> {
    use crate::ch347::{Ch347, parse_options};

    log::info!("Opening CH347 programmer...");

    let options = params.as_option_pairs();

    let config = parse_options(&options).map_err(|e| format!("Invalid CH347 parameters: {}", e))?;

    let master = Ch347::open_with_config(config).await.map_err(|e| {
        format!(
            "Failed to open CH347: {}\nMake sure the device is connected and you have permissions.",
            e
        )
    })?;

    Ok(master)
}

#[cfg(feature = "ch347")]
async fn open_ch347(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    probe_and_create_handle(open_ch347_master(params).await?, db).await
}

#[cfg(feature = "dediprog")]
async fn open_dediprog_master(
    params: &ProgrammerParams,
) -> Result<crate::dediprog::Dediprog, Box<dyn std::error::Error>> {
    use crate::dediprog::{Dediprog, parse_options};

    log::info!("Opening Dediprog programmer...");

    let options = params.as_option_pairs();

    let config =
        parse_options(&options).map_err(|e| format!("Invalid Dediprog parameters: {}", e))?;

    let master = Dediprog::open_with_config(config).await.map_err(|e| {
        format!(
            "Failed to open Dediprog: {}\n\
             Make sure the device is connected and you have USB permissions.",
            e
        )
    })?;

    log::info!(
        "Dediprog {}: {}",
        master.device_type(),
        master.device_string()
    );

    Ok(master)
}

#[cfg(feature = "dediprog")]
async fn open_dediprog(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    let mut master = open_dediprog_master(params).await?;

    // Probe the flash chip via SpiMaster
    let result = probe_detailed(&mut master, db).await?;
    log_probe_result(&result);
    let chip_info = ChipInfo::from(result);
    let ctx = rflasher_core::flash::FlashContext::new(chip_info.chip.clone());

    // Set flash size so OpaqueMaster bulk read/write knows the bounds
    master.set_flash_size(ctx.total_size() as u32);

    // Use HybridFlashDevice: OpaqueMaster for fast bulk read/write (CMD_READ/CMD_WRITE),
    // SpiMaster for erase, status register access, and write protection
    let device = HybridFlashDevice::new(master, ctx);
    Ok(FlashHandle::with_chip_info(
        ErasedFlashDevice::new(device),
        chip_info,
    ))
}

#[cfg(feature = "serprog-native")]
fn parse_serprog_config(
    params: &ProgrammerParams,
) -> Result<crate::serprog::SerprogConfig, Box<dyn std::error::Error>> {
    crate::serprog::parse_options(&params.as_option_pairs())
        .map_err(|e| format!("Invalid serprog parameters: {}", e).into())
}

#[cfg(feature = "serprog-native")]
async fn configure_serprog<T: crate::serprog::Transport>(
    transport: T,
    config: &crate::serprog::SerprogConfig,
) -> Result<crate::serprog::Serprog<T>, Box<dyn std::error::Error>> {
    let mut serprog = crate::serprog::Serprog::new(transport)
        .await
        .map_err(|e| format!("Failed to initialize serprog: {}", e))?;
    if let Some(speed_khz) = config.spispeed_khz {
        serprog
            .set_spi_speed(speed_khz * 1000)
            .await
            .map_err(|e| format!("Failed to set requested SPI speed of {speed_khz} kHz: {e}"))?;
    }
    if let Some(chip_select) = config.cs {
        serprog
            .set_spi_cs(chip_select)
            .await
            .map_err(|e| format!("Failed to set chip select: {}", e))?;
    }
    Ok(serprog)
}

#[cfg(feature = "serprog-native")]
async fn open_serprog_serial(
    device: &str,
    baud: Option<u32>,
    config: &crate::serprog::SerprogConfig,
) -> Result<crate::serprog::Serprog<crate::serprog::SerialTransport>, Box<dyn std::error::Error>> {
    let transport = crate::serprog::SerialTransport::open(device, baud)
        .map_err(|e| format!("Failed to open serial port {}: {}", device, e))?;
    configure_serprog(transport, config).await
}

#[cfg(feature = "serprog-native")]
async fn open_serprog_tcp(
    host: &str,
    port: u16,
    config: &crate::serprog::SerprogConfig,
) -> Result<crate::serprog::Serprog<crate::serprog::TcpTransport>, Box<dyn std::error::Error>> {
    let transport = crate::serprog::TcpTransport::connect(host, port)
        .map_err(|e| format!("Failed to connect to {}:{}: {}", host, port, e))?;
    configure_serprog(transport, config).await
}

#[cfg(feature = "serprog-native")]
async fn open_serprog(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    use crate::serprog::SerprogConnection;

    log::info!("Opening serprog programmer...");
    let config = parse_serprog_config(params)?;
    match config.connection()? {
        SerprogConnection::Serial { device, baud } => {
            probe_and_create_handle(open_serprog_serial(&device, baud, &config).await?, db).await
        }
        SerprogConnection::Tcp { host, port } => {
            probe_and_create_handle(open_serprog_tcp(&host, port, &config).await?, db).await
        }
    }
}

#[cfg(feature = "ftdi")]
async fn open_ftdi_master(
    params: &ProgrammerParams,
) -> Result<crate::ftdi::Ftdi, Box<dyn std::error::Error>> {
    use crate::ftdi::{Ftdi, parse_options};

    log::info!("Opening FTDI programmer...");

    let options = params.as_option_pairs();

    let config = parse_options(&options).map_err(|e| format!("Invalid FTDI parameters: {}", e))?;

    let master = Ftdi::open(&config).await.map_err(|e| {
        format!(
            "Failed to open FTDI device: {}\n\
             Make sure the device is connected and you have permissions.\n\
             You may need to unbind the kernel ftdi_sio driver:\n\
             echo -n '<bus>-<port>' | sudo tee /sys/bus/usb/drivers/ftdi_sio/unbind",
            e
        )
    })?;

    Ok(master)
}

#[cfg(feature = "ftdi")]
async fn open_ftdi(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    probe_and_create_handle(open_ftdi_master(params).await?, db).await
}

#[cfg(feature = "ft4222")]
async fn open_ft4222_master(
    params: &ProgrammerParams,
) -> Result<crate::ft4222::Ft4222, Box<dyn std::error::Error>> {
    use crate::ft4222::{Ft4222, parse_options};

    log::info!("Opening FT4222H programmer...");

    let options = params.as_option_pairs();

    let config =
        parse_options(&options).map_err(|e| format!("Invalid FT4222 parameters: {}", e))?;

    let master = Ft4222::open_with_config(config).await.map_err(|e| {
        format!(
            "Failed to open FT4222H device: {}\n\
             Make sure the device is connected and you have USB permissions.",
            e
        )
    })?;

    log::info!(
        "FT4222H: actual SPI clock = {} kHz",
        master.actual_speed_khz()
    );

    Ok(master)
}

#[cfg(feature = "ft4222")]
async fn open_ft4222(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    probe_and_create_handle(open_ft4222_master(params).await?, db).await
}

#[cfg(feature = "linux-spi")]
async fn open_linux_spi_master(
    params: &ProgrammerParams,
) -> Result<crate::linux_spi::LinuxSpi, Box<dyn std::error::Error>> {
    use crate::linux_spi::{LinuxSpi, parse_options};

    log::info!("Opening Linux SPI programmer...");

    let options = params.as_option_pairs();

    let config =
        parse_options(&options).map_err(|e| format!("Invalid linux_spi parameters: {}", e))?;

    let master = LinuxSpi::open(&config).map_err(|e| {
        format!(
            "Failed to open Linux SPI device: {}\n\
             Make sure the device exists and you have read/write permissions.\n\
             You may need to: sudo usermod -aG spi $USER",
            e
        )
    })?;

    Ok(master)
}

#[cfg(feature = "linux-spi")]
async fn open_linux_spi(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    probe_and_create_handle(open_linux_spi_master(params).await?, db).await
}

#[cfg(feature = "linux-mtd")]
async fn open_linux_mtd(
    params: &ProgrammerParams,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    use crate::linux_mtd::{LinuxMtd, parse_options};

    log::info!("Opening Linux MTD programmer...");

    let options = params.as_option_pairs();

    let config =
        parse_options(&options).map_err(|e| format!("Invalid linux_mtd parameters: {}", e))?;

    let mtd = LinuxMtd::open(&config).map_err(|e| {
        format!(
            "Failed to open Linux MTD device: {}\n\
             Make sure the device exists and you have read/write permissions.\n\
             List available MTD devices with: cat /proc/mtd",
            e
        )
    })?;

    let flash_size = mtd.size() as u32;
    let erase_size = mtd.erase_size() as u32;

    log::info!(
        "MTD device: {} ({} bytes, erase size {} bytes)",
        mtd.info().name,
        flash_size,
        erase_size
    );

    let mut device = OpaqueFlashDevice::new(mtd, flash_size);
    device.set_erase_block_size(erase_size);
    Ok(FlashHandle::without_chip_info(ErasedFlashDevice::new(
        device,
    )))
}

#[cfg(feature = "linux-gpio")]
async fn open_linux_gpio_spi_master(
    params: &ProgrammerParams,
) -> Result<crate::linux_gpio::LinuxGpioSpi, Box<dyn std::error::Error>> {
    use crate::linux_gpio::{LinuxGpioSpi, parse_options};

    log::info!("Opening Linux GPIO SPI (bitbang) programmer...");

    let options = params.as_option_pairs();

    let config =
        parse_options(&options).map_err(|e| format!("Invalid linux_gpio_spi parameters: {}", e))?;

    let master = LinuxGpioSpi::open(&config).map_err(|e| {
        format!(
            "Failed to open Linux GPIO SPI device: {}\n\
             Make sure the GPIO chip exists and you have permissions.\n\
             You may need to run as root or add udev rules for /dev/gpiochipN",
            e
        )
    })?;

    Ok(master)
}

#[cfg(feature = "linux-gpio")]
async fn open_linux_gpio_spi(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    probe_and_create_handle(open_linux_gpio_spi_master(params).await?, db).await
}

#[cfg(feature = "internal")]
fn restricted_resource_hint(resource: &rflasher_internal::RestrictedResource) -> &'static str {
    use rflasher_internal::RestrictedResource;

    match resource {
        RestrictedResource::DevMem { .. } => {
            "Internal flashing requires privileged access to physical memory through /dev/mem.\n\
             Try running rflasher with sudo. If access is still blocked, boot Linux with the iomem=relaxed kernel parameter."
        }
    }
}

#[cfg(feature = "internal")]
fn format_internal_init_error(error: &rflasher_internal::InternalError) -> String {
    use rflasher_internal::InternalError;

    let hint = match error {
        InternalError::NoChipset | InternalError::UnsupportedChipset { .. } => {
            "This system does not have a supported Intel or AMD chipset."
        }
        InternalError::MultipleChipsets => {
            "Multiple supported chipsets were found; internal programmer selection is ambiguous."
        }
        InternalError::PermissionDenied { resource } => restricted_resource_hint(resource),
        InternalError::NotSupported("SPI100 not in use") => {
            "The AMD SPI100 controller is not configured or in use."
        }
        _ => return format!("Failed to initialize internal programmer: {error}"),
    };

    format!("Failed to initialize internal programmer: {error}\n{hint}")
}

#[cfg(all(test, feature = "internal"))]
mod internal_error_tests {
    use super::format_internal_init_error;
    use rflasher_internal::{InternalError, PciAccessError, RestrictedResource};

    #[test]
    fn permission_hint_is_not_used_without_a_permission_specific_cause() {
        let pci_error = InternalError::PciAccess(PciAccessError::InvalidAccess {
            bus: 0,
            device: 0,
            function: 0,
            register: 0x1000,
        });
        let memory_map_error = InternalError::MemoryMap {
            address: 0,
            size: 4096,
        };

        assert!(!format_internal_init_error(&pci_error).contains("sudo"));
        assert!(!format_internal_init_error(&memory_map_error).contains("sudo"));
    }

    #[test]
    fn generic_pci_config_failure_does_not_assume_a_permission_cause() {
        let error = InternalError::PciAccess(PciAccessError::ConfigRead {
            bus: 0,
            device: 0x1f,
            function: 0,
            register: 0xf0,
        });
        let message = format_internal_init_error(&error);

        assert!(message.contains("failed to read PCI config"));
        assert!(!message.contains("sudo"));
    }

    #[test]
    fn permission_denied_for_dev_mem_has_specific_guidance() {
        let error = InternalError::PermissionDenied {
            resource: RestrictedResource::DevMem {
                address: 0xfedc_0000,
                size: 8192,
            },
        };
        let message = format_internal_init_error(&error);

        assert!(message.contains("sudo"));
        assert!(message.contains("iomem=relaxed"));
        // The failing physical address must survive into user-facing output
        // so CONFIG_STRICT_DEVMEM range failures remain diagnosable.
        assert!(message.contains("0xfedc0000"));
        assert!(message.contains("size 8192"));
    }

    #[test]
    fn amd_spi100_not_in_use_has_specific_guidance() {
        let error = InternalError::NotSupported("SPI100 not in use");

        assert!(format_internal_init_error(&error).contains("AMD SPI100 controller"));
    }
}

#[cfg(feature = "internal")]
async fn open_internal(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    use rflasher_internal::{InternalOptions, InternalProgrammer, SpiMode};

    log::info!("Opening internal programmer...");

    let options = params.as_option_pairs();

    let internal_opts = InternalOptions::from_options(&options)
        .map_err(|e| format!("Invalid internal programmer options: {}", e))?;

    if internal_opts.mode != SpiMode::Auto {
        log::info!("Using ich_spi_mode={}", internal_opts.mode);
    }

    let mut programmer = InternalProgrammer::with_options(internal_opts)
        .map_err(|error| format_internal_init_error(&error))?;

    // Software sequencing: can probe chip via SPI
    // Hardware sequencing: opaque operations only
    if programmer.mode() == SpiMode::SoftwareSequencing {
        log::info!("Using SPI mode (swseq allows chip probing)");
        probe_and_create_handle(programmer, db).await
    } else {
        log::info!("Using opaque mode (hwseq - no chip probing available)");
        let flash_size = get_flash_size_from_ifd(&mut programmer).await?;
        log::info!("Flash size: {} bytes (from IFD)", flash_size);

        let device = OpaqueFlashDevice::new(programmer, flash_size);
        Ok(FlashHandle::without_chip_info(ErasedFlashDevice::new(
            device,
        )))
    }
}

#[cfg(feature = "raiden")]
async fn open_raiden_master(
    params: &ProgrammerParams,
) -> Result<crate::raiden::RaidenDebugSpi, Box<dyn std::error::Error>> {
    use crate::raiden::{RaidenDebugSpi, parse_options};

    log::info!("Opening Raiden Debug SPI programmer...");

    let options = params.as_option_pairs();

    let config =
        parse_options(&options).map_err(|e| format!("Invalid raiden parameters: {}", e))?;

    let master = RaidenDebugSpi::open_with_config(&config)
        .await
        .map_err(|e| {
            format!(
                "Failed to open Raiden Debug SPI device: {}\n\
             Make sure a Chrome OS debug device (SuzyQable, Servo, C2D2) is connected\n\
             and you have USB permissions.",
                e
            )
        })?;

    Ok(master)
}

#[cfg(feature = "raiden")]
async fn open_raiden(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    probe_and_create_handle(open_raiden_master(params).await?, db).await
}

#[cfg(feature = "sunxi-fel")]
async fn open_sunxi_fel_master(
    _params: &ProgrammerParams,
) -> Result<crate::sunxi_fel::SunxiFel, Box<dyn std::error::Error>> {
    log::info!("Opening sunxi FEL programmer...");

    let master = crate::sunxi_fel::SunxiFel::open().await.map_err(|e| {
        format!(
            "Failed to open sunxi FEL device: {}\n\
             Make sure the device is in FEL mode (hold FEL button while plugging in USB)\n\
             and you have USB permissions (VID:1F3A PID:EFE8).",
            e
        )
    })?;

    log::info!("Connected to: {}", master.soc_name());

    Ok(master)
}

#[cfg(feature = "sunxi-fel")]
async fn open_sunxi_fel(
    params: &ProgrammerParams,
    db: &dyn ChipProvider,
) -> Result<FlashHandle, Box<dyn std::error::Error>> {
    let mut master = open_sunxi_fel_master(params).await?;

    // Probe the flash chip via SpiMaster
    let result = probe_detailed(&mut master, db).await?;
    log_probe_result(&result);
    let chip_info = ChipInfo::from(result);
    let ctx = rflasher_core::flash::FlashContext::new(chip_info.chip.clone());

    // Configure OpaqueMaster with chip info discovered during probe
    master.set_use_4byte_addr(ctx.total_size() > 16 * 1024 * 1024);
    master.set_erase_blocks(ctx.chip.erase_blocks().to_vec());

    // Use HybridFlashDevice: OpaqueMaster for fast bulk read/write/erase
    // (batched SPI commands with on-SoC busy-wait), SpiMaster for WP and
    // status register access
    let device = HybridFlashDevice::new(master, ctx);
    Ok(FlashHandle::with_chip_info(
        ErasedFlashDevice::new(device),
        chip_info,
    ))
}

#[cfg(all(test, feature = "dummy"))]
mod dispatch_tests {
    use super::{open_flash, open_spi_programmer};
    use rflasher_core::chip::{ChipProvider, FlashChip};
    use rflasher_core::programmer::SpiMaster;
    use rflasher_core::spi::{SpiCommand, opcodes};

    struct NoChips;

    impl ChipProvider for NoChips {
        fn find_by_jedec_id(&self, _: u8, _: u16) -> Option<&FlashChip> {
            None
        }
    }

    #[test]
    fn dummy_raw_and_flash_dispatch_keep_distinct_results() {
        futures_lite::future::block_on(async {
            let mut master = open_spi_programmer("dummy").await.unwrap();
            let mut jedec = [0; 3];
            master
                .execute(&mut SpiCommand::read_reg(opcodes::RDID, &mut jedec))
                .await
                .unwrap();
            assert_eq!(jedec, [0xef, 0x40, 0x18]);

            // The flash path probes the chip rather than returning a raw master.
            let error = open_flash("dummy", &NoChips).await.err().unwrap();
            assert!(matches!(
                error.downcast_ref::<rflasher_core::error::Error>(),
                Some(rflasher_core::error::Error::ChipNotFound)
            ));
        });
    }
}

#[cfg(all(test, feature = "serprog-native"))]
mod serprog_open_tests {
    use super::parse_serprog_config;
    use crate::catalog::parse_programmer_params;
    use crate::serprog::SerprogConnection;

    #[test]
    fn registry_serprog_options_preserve_connection_and_configuration() {
        let serial = parse_serprog_config(
            &parse_programmer_params("serprog:dev=/dev/ttyUSB0:115200,baud=9600,spispeed=2m,cs=1")
                .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            serial.connection().unwrap(),
            SerprogConnection::Serial {
                baud: Some(9600),
                ..
            }
        ));
        assert_eq!((serial.spispeed_khz, serial.cs), (Some(2000), Some(1)));

        let tcp =
            parse_serprog_config(&parse_programmer_params("serprog:ip=localhost:1234").unwrap())
                .unwrap();
        assert!(matches!(
            tcp.connection().unwrap(),
            SerprogConnection::Tcp { port: 1234, .. }
        ));
        assert!(parse_serprog_config(&parse_programmer_params("serprog:bad=1").unwrap()).is_err());
    }
}

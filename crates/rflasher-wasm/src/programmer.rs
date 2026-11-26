//! Browser programmer dispatch and connection helpers.

use rflasher_core::error::Result as CoreResult;
use rflasher_core::flash::FlashContext;
use rflasher_core::programmer::{FlashAccess, SpiFeatures, SpiMaster};
use rflasher_core::spi::SpiCommand;
use rflasher_programmers::ch341a::Ch341a;
use rflasher_programmers::ch347::Ch347;
use rflasher_programmers::dediprog::Dediprog;
use rflasher_programmers::ezp_spi::EzpSpi;
use rflasher_programmers::ft4222::Ft4222;
use rflasher_programmers::ftdi::Ftdi;
use rflasher_programmers::raiden::RaidenDebugSpi;
use rflasher_programmers::serprog::Serprog;

use crate::transport::WebSerialTransport;

/// Baud rate used for serprog when the form leaves `baud=` unset.
const DEFAULT_SERPROG_BAUD: u32 = 115200;

/// Connected programmer - wraps a serprog, CH341A, CH347, FTDI, FT4222H,
/// Dediprog, or Raiden device
#[allow(clippy::large_enum_variant)]
pub(crate) enum Programmer {
    Serprog(Serprog<WebSerialTransport>),
    Ch341a(Ch341a),
    Ch347(Ch347),
    Ftdi(Ftdi),
    Ft4222(Ft4222),
    Dediprog(Dediprog),
    EzpSpi(EzpSpi),
    Raiden(RaidenDebugSpi),
}

/// Run `$body` with `$m` bound to the inner programmer of any variant.
macro_rules! dispatch {
    ($programmer:expr, $m:ident => $body:expr) => {
        match $programmer {
            Programmer::Serprog($m) => $body,
            Programmer::Ch341a($m) => $body,
            Programmer::Ch347($m) => $body,
            Programmer::Ftdi($m) => $body,
            Programmer::Ft4222($m) => $body,
            Programmer::Dediprog($m) => $body,
            Programmer::EzpSpi($m) => $body,
            Programmer::Raiden($m) => $body,
        }
    };
}

/// Forwards to the connected programmer, including its flash fast paths.
impl SpiMaster for Programmer {
    fn features(&self) -> SpiFeatures {
        dispatch!(self, m => SpiMaster::features(m))
    }

    fn max_read_len(&self) -> usize {
        dispatch!(self, m => SpiMaster::max_read_len(m))
    }

    fn max_write_len(&self) -> usize {
        dispatch!(self, m => SpiMaster::max_write_len(m))
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        dispatch!(self, m => SpiMaster::execute(m, cmd).await)
    }

    fn probe_opcode(&self, opcode: u8) -> bool {
        dispatch!(self, m => SpiMaster::probe_opcode(m, opcode))
    }

    fn supports_status_write(&self, enable_opcode: u8, len: usize) -> bool {
        dispatch!(self, m => SpiMaster::supports_status_write(m, enable_opcode, len))
    }

    fn chip_identified(&mut self, chip: &rflasher_core::chip::FlashChip) {
        dispatch!(self, m => SpiMaster::chip_identified(m, chip))
    }

    async fn delay_us(&mut self, us: u32) {
        dispatch!(self, m => SpiMaster::delay_us(m, us).await)
    }

    fn check_access(&self, access: FlashAccess, addr: u32, len: u32) -> CoreResult<()> {
        dispatch!(self, m => SpiMaster::check_access(m, access, addr, len))
    }

    async fn read_flash(
        &mut self,
        ctx: &FlashContext,
        addr: u32,
        buf: &mut [u8],
    ) -> CoreResult<()> {
        dispatch!(self, m => SpiMaster::read_flash(m, ctx, addr, buf).await)
    }

    async fn program_flash(
        &mut self,
        ctx: &FlashContext,
        addr: u32,
        data: &[u8],
    ) -> CoreResult<()> {
        dispatch!(self, m => SpiMaster::program_flash(m, ctx, addr, data).await)
    }
}

// =============================================================================
// Connecting
// =============================================================================

/// Ask the browser for a device and open programmer `name` with `options`.
///
/// `options` are the raw `key=value` pairs from the form; each backend's own
/// `parse_options` turns them into its typed config, exactly as the CLI does.
/// Returns the opened programmer and a display name for the status panel.
pub(crate) async fn open_programmer(
    name: &str,
    options: &[(&str, &str)],
) -> Result<(Programmer, String), String> {
    match name {
        "serprog" => {
            let config =
                rflasher_programmers::serprog::parse_options(options).map_err(|e| e.to_string())?;
            let baud = config.baud.unwrap_or(DEFAULT_SERPROG_BAUD);
            let transport = WebSerialTransport::request_and_open(baud)
                .await
                .map_err(|e| e.to_string())?;
            let mut serprog = Serprog::new(transport).await.map_err(|e| e.to_string())?;
            if let Some(khz) = config.spispeed_khz
                && let Err(e) = serprog.set_spi_speed(khz * 1000).await
            {
                log::warn!("Failed to set SPI speed: {e}");
            }
            if let Some(cs) = config.cs {
                serprog
                    .set_spi_cs(cs)
                    .await
                    .map_err(|e| format!("Failed to set chip select: {e}"))?;
            }
            let display = serprog.info().name_str().to_string();
            Ok((Programmer::Serprog(serprog), display))
        }
        "ch341a" => {
            let device_info = Ch341a::request_device().await.map_err(|e| e.to_string())?;
            let ch341a = Ch341a::open(device_info).await.map_err(|e| e.to_string())?;
            Ok((Programmer::Ch341a(ch341a), "CH341A".to_string()))
        }
        "ch347" => {
            let config =
                rflasher_programmers::ch347::parse_options(options).map_err(|e| e.to_string())?;
            let device_info = Ch347::request_device().await.map_err(|e| e.to_string())?;
            let ch347 = Ch347::open(device_info, config)
                .await
                .map_err(|e| e.to_string())?;
            let display = match ch347.variant() {
                rflasher_programmers::ch347::Ch347Variant::Ch347T => "CH347T",
                rflasher_programmers::ch347::Ch347Variant::Ch347F => "CH347F",
            };
            Ok((Programmer::Ch347(ch347), display.to_string()))
        }
        "ftdi" => {
            let config =
                rflasher_programmers::ftdi::parse_options(options).map_err(|e| e.to_string())?;
            let device = Ftdi::request_device().await.map_err(|e| e.to_string())?;
            let ftdi = Ftdi::open(device, &config)
                .await
                .map_err(|e| e.to_string())?;
            let display = format!(
                "{} ch {}",
                config.device_type.name(),
                config.interface.letter()
            );
            Ok((Programmer::Ftdi(ftdi), display))
        }
        "ft4222" => {
            let config =
                rflasher_programmers::ft4222::parse_options(options).map_err(|e| e.to_string())?;
            let device_info = Ft4222::request_device().await.map_err(|e| e.to_string())?;
            let ft4222 = Ft4222::open(device_info, config)
                .await
                .map_err(|e| e.to_string())?;
            let display = format!("FT4222H @ {} kHz", ft4222.actual_speed_khz());
            Ok((Programmer::Ft4222(ft4222), display))
        }
        "dediprog" => {
            let config = rflasher_programmers::dediprog::parse_options(options)
                .map_err(|e| e.to_string())?;
            let device_info = Dediprog::request_device()
                .await
                .map_err(|e| e.to_string())?;
            let dediprog = Dediprog::open(device_info, config)
                .await
                .map_err(|e| e.to_string())?;
            let display = format!("Dediprog {}", dediprog.device_string());
            Ok((Programmer::Dediprog(dediprog), display))
        }
        "ezp_spi" => {
            let device_info = EzpSpi::request_device().await.map_err(|e| e.to_string())?;
            let ezp = EzpSpi::open(device_info).await.map_err(|e| e.to_string())?;
            let display = format!("EZP2013 ({})", ezp.firmware_version());
            Ok((Programmer::EzpSpi(ezp), display))
        }
        "raiden_debug_spi" => {
            let config =
                rflasher_programmers::raiden::parse_options(options).map_err(|e| e.to_string())?;
            let device_info = RaidenDebugSpi::request_device()
                .await
                .map_err(|e| e.to_string())?;
            let raiden = RaidenDebugSpi::open(device_info, &config)
                .await
                .map_err(|e| e.to_string())?;
            let display = format!("Raiden ({})", config.target);
            Ok((Programmer::Raiden(raiden), display))
        }
        other => Err(format!("{other} cannot be opened from a browser")),
    }
}

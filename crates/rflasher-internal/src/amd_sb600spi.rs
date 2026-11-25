//! AMD SB600-family SPI controller driver (SB600, SB7x0/SP5100, SB8x0/SB9x0).
//!
//! Port of flashprog's `sb600spi.c` legacy 8-byte ring-buffer FIFO path.
//! Later FCHs (Hudson-2/3/4, Bolton, Yangtze, Promontory) are recognized so
//! discovery can report them accurately, but are refused as untested.

pub mod regs;

use crate::controller::Controller;
use crate::error::InternalError;
#[cfg(all(feature = "std", target_os = "linux"))]
use crate::host::LinuxHost;
use crate::host::{HostAccess, MmioAccess};
use regs::*;
use rflasher_core::chip::FlashChip;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::flash::{self, FlashContext};
use rflasher_core::programmer::{FlashAccess, SpiFeatures, SpiMaster};
use rflasher_core::spi::{AddressWidth, IoMode, SpiCommand, opcodes};

/// Size of the SB600/SB700/SB800 SPI ring-buffer FIFO in bytes.
pub const SB600_FIFO_SIZE: usize = 8;
const ADDRESS_BYTES: usize = 3;
/// Maximum payload bytes after the opcode in one command (8-byte FIFO).
pub const SB600_MAX_TX: usize = SB600_FIFO_SIZE;
/// Maximum received bytes in one command (8-byte ring buffer).
pub const SB600_MAX_RX: usize = SB600_FIFO_SIZE;
/// Maximum data bytes in one 3-byte-addressed write command (8 - 3 = 5).
pub const SB600_MAX_DATA_WRITE: usize = SB600_FIFO_SIZE - ADDRESS_BYTES;
/// Addressable limit of 3-byte SPI commands (16 MiB).
const LIMIT_3BYTE: u32 = 16 * 1024 * 1024;

const IDLE_POLL_LIMIT: u32 = 100_000;
const FIFO_RESET_POLL_LIMIT: u32 = 10_000;
const EXECUTE_POLL_LIMIT: u32 = 100_000;

/// AMD southbridge / FCH generations handled by `sb600spi.c`.
///
/// Ordered like flashprog's `enum amd_chipset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Sb600Generation {
    /// SB600 (LPC 1002:438d)
    Sb6xx,
    /// SB7x0 / SP5100 (LPC 1002:439d, SMBus rev 0x39..=0x3d)
    Sb7xx,
    /// SB8x0 / SB9x0 / Hudson-1 (LPC 1002:439d, SMBus rev 0x40..=0x42)
    Sb89xx,
    /// Hudson-2/3/4 (LPC 1022:780e, SMBus rev 0x11..=0x15). Refused as untested.
    Hudson234,
    /// Bolton (LPC 1022:780e, SMBus rev 0x16). Refused as untested.
    Bolton,
    /// Yangtze (Kabini/Temash, Merlin Falcon). Refused as untested.
    Yangtze,
    /// Promontory (Stoney Ridge). Refused as untested.
    Promontory,
}

impl Sb600Generation {
    /// Determine the generation from the LPC bridge device ID and SMBus revision.
    pub fn from_ids(lpc_device_id: u16, smbus_revision: u8) -> Option<Self> {
        match (lpc_device_id, smbus_revision) {
            (0x438d, _) => Some(Self::Sb6xx),
            (0x439d, 0x39..=0x3d) => Some(Self::Sb7xx),
            (0x439d, 0x40..=0x42) => Some(Self::Sb89xx),
            (0x780e, 0x11..=0x15) => Some(Self::Hudson234),
            (0x780e, 0x16) => Some(Self::Bolton),
            (0x780e, 0x39..=0x3a | 0x42) | (0x790e, 0x4a) => Some(Self::Yangtze),
            (0x790e, 0x4b) => Some(Self::Promontory),
            _ => None,
        }
    }

    /// Expected SMBus (D14F0) vendor/device ID paired with `lpc_device_id`.
    pub fn smbus_id(lpc_device_id: u16) -> Option<(u16, u16)> {
        match lpc_device_id {
            0x438d | 0x439d => Some((0x1002, 0x4385)),
            0x780e => Some((0x1022, 0x780b)),
            0x790e => Some((0x1022, 0x790b)),
            _ => None,
        }
    }

    /// Expected LPC bridge vendor ID for `lpc_device_id`.
    pub fn lpc_vendor(lpc_device_id: u16) -> Option<u16> {
        match lpc_device_id {
            0x438d | 0x439d => Some(0x1002),
            0x780e | 0x790e => Some(0x1022),
            _ => None,
        }
    }

    /// Human-readable generation name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Sb6xx => "SB6xx",
            Self::Sb7xx => "SB7xx/SP5100",
            Self::Sb89xx => "SB8xx/SB9xx",
            Self::Hudson234 => "Hudson-2/3/4",
            Self::Bolton => "Bolton",
            Self::Yangtze => "Yangtze",
            Self::Promontory => "Promontory",
        }
    }

    /// Whether this generation includes an integrated microcontroller (IMC).
    pub fn has_imc(self) -> bool {
        self != Self::Sb6xx
    }

    /// Whether this driver supports driving this generation.
    pub fn is_supported(self) -> bool {
        self <= Self::Sb89xx
    }

    fn controller_name(self) -> &'static str {
        match self {
            Self::Sb6xx => "AMD SB6xx SPI",
            Self::Sb7xx => "AMD SB7xx/SP5100 SPI",
            _ => "AMD SB8xx/SB9xx SPI",
        }
    }

    /// SB7xx and SB8xx also mirror RestrictedCmdWoAddr1/2 in SPI_Cntrl1[31:16].
    fn restricts_in_cntrl1(self) -> bool {
        matches!(self, Self::Sb7xx | Self::Sb89xx)
    }

    /// SpiBusy (SPI_Cntrl0[31]) exists on SB8xx and later.
    fn has_spi_busy(self) -> bool {
        self == Self::Sb89xx
    }
}

impl core::fmt::Display for Sb600Generation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

/// Command engine clock (`NormSpeed`, `SPI_Cntrl1[13:12]`).
///
/// 66 MHz (`00`, reserved on SB7xx/SP5100 and above the 33 MHz SPI ROM rating
/// in the SB810/SB850 databook) is not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sb600SpiSpeed {
    /// 33 MHz
    Mhz33,
    /// 22 MHz
    Mhz22,
    /// 16.5 MHz (flashprog default and hardware reset value)
    #[default]
    Mhz16_5,
}

impl Sb600SpiSpeed {
    /// All supported speeds.
    pub const ALL: [Self; 3] = [Self::Mhz33, Self::Mhz22, Self::Mhz16_5];

    /// `NormSpeed` field encoding.
    pub fn bits(self) -> u8 {
        match self {
            Self::Mhz33 => 1,
            Self::Mhz22 => 2,
            Self::Mhz16_5 => 3,
        }
    }

    /// Human-readable speed label.
    pub fn name(self) -> &'static str {
        match self {
            Self::Mhz33 => "33 MHz",
            Self::Mhz22 => "22 MHz",
            Self::Mhz16_5 => "16.5 MHz",
        }
    }

    /// CLI option token (`33mhz`, `22mhz`, `16.5mhz`).
    pub fn option_value(self) -> &'static str {
        match self {
            Self::Mhz33 => "33mhz",
            Self::Mhz22 => "22mhz",
            Self::Mhz16_5 => "16.5mhz",
        }
    }

    /// Parse a speed name, ignoring ASCII case and whitespace.
    pub fn parse(value: &str) -> Option<Self> {
        let mut normalized = [0u8; 16];
        let mut len = 0;
        for byte in value.bytes().filter(|b| !b.is_ascii_whitespace()) {
            *normalized.get_mut(len)? = byte.to_ascii_lowercase();
            len += 1;
        }
        Self::ALL
            .into_iter()
            .find(|speed| speed.option_value().as_bytes() == &normalized[..len])
    }
}

fn norm_speed_name(bits: u8) -> &'static str {
    match bits {
        0 => "66 MHz/reserved",
        1 => "33 MHz",
        2 => "22 MHz",
        _ => "16.5 MHz",
    }
}

/// One LPC Rom Protect register (`0x50..=0x5C`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RomProtect(u32);

impl RomProtect {
    /// An inactive register.
    pub const NONE: Self = Self(0);

    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    pub const fn write_protected(self) -> bool {
        self.0 & ROM_PROTECT_WRITE != 0
    }

    pub const fn read_protected(self) -> bool {
        self.0 & ROM_PROTECT_READ != 0
    }

    /// First protected host physical address (bits 31:11, 2 KiB aligned).
    pub const fn base(self) -> u32 {
        self.0 & ROM_PROTECT_BASE_MASK
    }

    /// Protected range length in bytes: `(RomOffset + 1) * 1024`.
    pub const fn size(self) -> u32 {
        (((self.0 >> ROM_PROTECT_OFFSET_SHIFT) & ROM_PROTECT_OFFSET_MASK) + 1) << 10
    }

    /// Conservatively check whether this range overlaps flash offsets `start..end`
    /// of a chip of `chip_size` bytes.
    ///
    /// Host addresses in the top 16 MiB map to `addr & (chip_size - 1)` on a
    /// power-of-two chip up to 16 MiB. Ranges outside the top 16 MiB or chips
    /// with non-power-of-two sizes conservatively protect the whole chip.
    pub fn protects(self, chip_size: u32, start: u32, end: u32) -> bool {
        const WINDOW: u64 = (1 << 32) - LIMIT_3BYTE as u64;
        if start >= end {
            return false;
        }
        let base = u64::from(self.base());
        let host_end = base + u64::from(self.size());
        if base < WINDOW
            || host_end > 1 << 32
            || !chip_size.is_power_of_two()
            || chip_size > LIMIT_3BYTE
            || self.size() >= chip_size
        {
            return true;
        }
        let first = self.base() & (chip_size - 1);
        let last = first + self.size();
        let overlaps = |a: u32, b: u32| a < end && start < b;
        overlaps(first, last.min(chip_size)) || (last > chip_size && overlaps(0, last - chip_size))
    }
}

/// Integrated microcontroller (IMC) state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sb600Imc {
    /// Southbridge has no IMC (SB600).
    NotPresent,
    /// IMC is strapped off and not reporting itself present.
    StrappedOff,
    /// IMC is strapped on or running (`IntegratedImcPresent`).
    Enabled,
    /// IMC strap could not be determined.
    Unknown,
}

impl Sb600Imc {
    fn allows_engine(self) -> bool {
        matches!(self, Self::NotPresent | Self::StrappedOff)
    }
}

/// Configuration required to open [`Sb600SpiController`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sb600SpiConfig {
    /// Controller generation.
    pub generation: Sb600Generation,
    /// Command engine clock while open; `None` leaves the current clock unchanged.
    pub speed: Option<Sb600SpiSpeed>,
    /// IMC state; must be [`Sb600Imc::NotPresent`] (SB600) or [`Sb600Imc::StrappedOff`].
    pub imc: Sb600Imc,
    /// Rom Protect 0..3 registers read from LPC `0x50..=0x5C`.
    pub rom_protect: [RomProtect; 4],
}

impl Sb600SpiConfig {
    /// Default configuration for `generation`: 16.5 MHz, no Rom Protect, and
    /// `NotPresent` on SB600 or `Unknown` elsewhere (requiring explicit verification).
    pub const fn new(generation: Sb600Generation) -> Self {
        Self {
            generation,
            speed: Some(Sb600SpiSpeed::Mhz16_5),
            imc: if matches!(generation, Sb600Generation::Sb6xx) {
                Sb600Imc::NotPresent
            } else {
                Sb600Imc::Unknown
            },
            rom_protect: [RomProtect::NONE; 4],
        }
    }
}

/// Controller register fields modified on open and restored on drop.
#[derive(Debug, Default, Clone, Copy)]
struct Restore {
    norm_speed: Option<u8>,
    fast_read: bool,
}

/// Timeout state recorded when polling does not complete in time.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Poison {
    #[default]
    None,
    /// `ExecuteOpCode` did not clear within the polling budget; safe to
    /// restore saved settings on drop once the engine returns to idle.
    ExecuteTimeout,
    /// FIFO pointer reset did not complete; do not touch registers on drop.
    FifoTimeout,
}

/// AMD SB600-family SPI controller.
pub struct Sb600SpiController<H: HostAccess> {
    host: H,
    spibar: H::MmioRegion,
    generation: Sb600Generation,
    /// Opcodes from SPI_RestrictedCmd / SPI_RestrictedCmd2 / SPI_Cntrl1[31:16] (0 = unused).
    restricted: [u8; 10],
    rom_protect: [RomProtect; 4],
    chip_size: Option<u32>,
    /// Addressed erase opcodes and their maximum block size from `chip_identified`.
    erase_ops: [(u8, u32); 8],
    erase_ops_len: usize,
    restore: Restore,
    poison: Poison,
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl Sb600SpiController<LinuxHost> {
    /// Open the controller at physical address `spibar_addr` on Linux.
    ///
    /// # Safety
    ///
    /// Same requirements as [`Self::new_with_host`]. Prefer
    /// [`crate::amd_enable::open_amd_sb600`].
    pub unsafe fn new(spibar_addr: u64, config: Sb600SpiConfig) -> Result<Self, InternalError> {
        // SAFETY: forwarded to caller's safety contract.
        unsafe { Self::new_with_host(LinuxHost::new(), spibar_addr, config) }
    }
}

impl<H: HostAccess> Sb600SpiController<H> {
    /// Open the controller at `spibar_addr` using caller-provided host access.
    ///
    /// # Safety
    ///
    /// - `spibar_addr` must be the valid 32-byte SPI register window of an
    ///   SB600-family controller matching `config.generation` for `host`.
    /// - No other agent may concurrently drive this SPI register window.
    pub unsafe fn new_with_host(
        host: H,
        spibar_addr: u64,
        config: Sb600SpiConfig,
    ) -> Result<Self, InternalError> {
        let generation = config.generation;
        if !generation.is_supported() {
            log::error!("{generation} SPI controller support is untested and not enabled");
            return Err(InternalError::NotSupported(
                "AMD FCH SB600-family SPI controller variant is untested",
            ));
        }
        if !config.imc.allows_engine() {
            log::error!(
                "The {generation} IMC is {:?}; refusing to drive the SPI engine",
                config.imc
            );
            return Err(InternalError::NotSupported(
                "SB600-family IMC is not known to be off",
            ));
        }
        if config.imc == Sb600Imc::NotPresent && generation.has_imc() {
            return Err(InternalError::NotSupported(
                "SB600-family IMC state must be established",
            ));
        }

        // SAFETY: caller guarantees `spibar_addr` names this controller's 32-byte MMIO window.
        let spibar = unsafe { host.map_mmio(spibar_addr, SPI_WINDOW)? };

        let mut controller = Self {
            host,
            spibar,
            generation,
            restricted: [0; 10],
            rom_protect: config.rom_protect,
            chip_size: None,
            erase_ops: [(0, 0); 8],
            erase_ops_len: 0,
            restore: Restore::default(),
            poison: Poison::None,
        };
        controller.print_config();
        let cntrl0 = controller.spibar.read32(SPI_CNTRL0);
        if cntrl0 & CNTRL0_SPI_ACCESS_MAC_ROM_EN == 0 || cntrl0 & CNTRL0_SPI_HOST_ACCESS_ROM_EN == 0
        {
            log::error!("State of SpiAccessMacRomEn or SpiHostAccessRomEn prohibits full access");
            return Err(InternalError::AccessDenied {
                region: "SB600 SPI controller (SpiAccessMacRomEn/SpiHostAccessRomEn cleared)",
            });
        }
        controller.wait_idle()?;
        controller.read_restricted();
        controller.log_rom_protect();
        // If configuring speed fails, Drop restores any earlier register change.
        controller.handle_speed(config.speed)?;
        Ok(controller)
    }

    pub fn generation(&self) -> Sb600Generation {
        self.generation
    }

    /// Opcodes present in the hardware restricted-command registers.
    pub fn restricted_opcodes(&self) -> impl Iterator<Item = u8> + '_ {
        self.restricted.iter().copied().filter(|&op| op != 0)
    }

    pub fn rom_protect(&self) -> [RomProtect; 4] {
        self.rom_protect
    }

    pub fn is_poisoned(&self) -> bool {
        self.poison != Poison::None
    }

    fn cmd_byte(&self) -> u8 {
        self.spibar.read8(SPI_CNTRL0_CMD) & !CMD_TRIGGERS
    }

    fn print_config(&self) {
        let cntrl0 = self.spibar.read32(SPI_CNTRL0);
        log::debug!(
            "SPI_Cntrl0={cntrl0:#010x} SpiArbEnable={} SpiAccessMacRomEn={} \
             SpiHostAccessRomEn={} ArbWaitCount={} SpiBridgeDisable={}",
            u32::from(cntrl0 & CNTRL0_SPI_ARB_ENABLE != 0),
            u32::from(cntrl0 & CNTRL0_SPI_ACCESS_MAC_ROM_EN != 0),
            u32::from(cntrl0 & CNTRL0_SPI_HOST_ACCESS_ROM_EN != 0),
            (cntrl0 & CNTRL0_ARB_WAIT_COUNT_MASK) >> CNTRL0_ARB_WAIT_COUNT_SHIFT,
            u32::from(cntrl0 & CNTRL0_SPI_BRIDGE_DISABLE != 0),
        );
        if self.generation >= Sb600Generation::Sb7xx {
            log::debug!(
                "  DropOneClkOnRd/SpiClkGate={} SpiBusy={}",
                u32::from(cntrl0 & CNTRL0_SPI_CLK_GATE != 0),
                u32::from(cntrl0 & CNTRL0_SPI_BUSY != 0),
            );
        }
        if self.generation >= Sb600Generation::Sb89xx {
            log::debug!("Using SPI_CS{}", self.spibar.read8(ALT_SPI_CS) & 0x03);
        }
    }

    fn log_rom_protect(&self) {
        for (index, protect) in self.rom_protect.iter().enumerate() {
            if protect.read_protected() || protect.write_protected() {
                log::warn!(
                    "Rom Protect {index}: {}{}protected {:#010x}..{:#010x}",
                    if protect.read_protected() {
                        "read "
                    } else {
                        ""
                    },
                    if protect.write_protected() {
                        "write "
                    } else {
                        ""
                    },
                    protect.base(),
                    u64::from(protect.base()) + u64::from(protect.size()),
                );
            }
        }
    }

    fn read_restricted(&mut self) {
        let cmd1 = self.spibar.read32(SPI_RESTRICTED_CMD).to_le_bytes();
        let cmd2 = self.spibar.read32(SPI_RESTRICTED_CMD2).to_le_bytes();
        self.restricted[0..4].copy_from_slice(&cmd1);
        self.restricted[4..8].copy_from_slice(&cmd2);
        if self.generation.restricts_in_cntrl1() {
            let cntrl1 = self.spibar.read32(SPI_CNTRL1).to_le_bytes();
            self.restricted[8] = cntrl1[2];
            self.restricted[9] = cntrl1[3];
        }
        for opcode in self.restricted_opcodes() {
            log::info!("Opcode {opcode:#04x} is restricted by the SPI controller");
        }
    }

    fn is_restricted(&self, opcode: u8) -> bool {
        self.restricted_opcodes().any(|op| op == opcode)
    }

    fn engine_idle(&self) -> bool {
        let cntrl0 = self.spibar.read32(SPI_CNTRL0);
        cntrl0 & CNTRL0_EXECUTE_OPCODE == 0
            && !(self.generation.has_spi_busy() && cntrl0 & CNTRL0_SPI_BUSY != 0)
    }

    fn wait_idle(&self) -> Result<(), InternalError> {
        for _ in 0..IDLE_POLL_LIMIT {
            if self.engine_idle() {
                return Ok(());
            }
            self.host.delay_us(1);
        }
        log::error!("The SPI command engine is busy; another agent may be using it");
        Err(InternalError::Io("SPI command engine busy"))
    }

    fn ensure_ready(&mut self) -> Result<(), InternalError> {
        self.wait_idle()?;
        if self.poison != Poison::None {
            log::info!("SPI command engine is idle again after a timeout");
            self.poison = Poison::None;
        }
        Ok(())
    }

    fn norm_speed(&self) -> u8 {
        (self.spibar.read8(SPI_CNTRL1_SPEED) & SPEED_NORM_MASK) >> SPEED_NORM_SHIFT
    }

    fn write_norm_speed(&self, bits: u8) {
        let reg = self.spibar.read8(SPI_CNTRL1_SPEED);
        let updated = (reg & !SPEED_NORM_MASK) | ((bits << SPEED_NORM_SHIFT) & SPEED_NORM_MASK);
        self.spibar.write8(SPI_CNTRL1_SPEED, updated);
    }

    fn handle_speed(&mut self, speed: Option<Sb600SpiSpeed>) -> Result<(), InternalError> {
        if self.generation == Sb600Generation::Sb89xx {
            let cmd = self.cmd_byte();
            let fast_read = cmd & CMD_FAST_READ_ENABLE != 0;
            log::debug!(
                "Fast Reads are {}abled",
                if fast_read { "en" } else { "dis" }
            );
            if fast_read {
                log::debug!("Disabling fast reads temporarily");
                self.spibar
                    .write8(SPI_CNTRL0_CMD, cmd & !CMD_FAST_READ_ENABLE);
                if self.spibar.read32(SPI_CNTRL0) & CNTRL0_FAST_READ_ENABLE != 0 {
                    log::error!("Clearing fastReadEnable did not stick");
                    return Err(InternalError::SpiInit("Clearing fastReadEnable failed"));
                }
                self.restore.fast_read = true;
            }
        }

        let current = self.norm_speed();
        log::debug!("NormSpeed is {}", norm_speed_name(current));
        let Some(speed) = speed else {
            return Ok(());
        };
        let bits = speed.bits();
        if current == bits {
            return Ok(());
        }
        log::debug!("Setting SPI clock to {}", speed.name());
        let before = self.spibar.read8(SPI_CNTRL1_SPEED);
        self.restore.norm_speed = Some(current);
        self.write_norm_speed(bits);
        let after = self.spibar.read8(SPI_CNTRL1_SPEED);
        if self.norm_speed() != bits || (after & SPEED_FAST_MASK) != (before & SPEED_FAST_MASK) {
            log::error!("Setting SPI clock failed");
            return Err(InternalError::SpiInit("Setting SPI clock failed"));
        }
        Ok(())
    }

    fn restore_settings(&mut self) {
        let restore = core::mem::take(&mut self.restore);
        if restore.norm_speed.is_none() && !restore.fast_read {
            return;
        }
        if self.poison == Poison::FifoTimeout || !self.engine_idle() {
            log::warn!("SPI command engine is not idle; leaving clock settings unchanged");
            return;
        }
        if let Some(bits) = restore.norm_speed {
            self.write_norm_speed(bits);
        }
        if restore.fast_read {
            self.spibar
                .write8(SPI_CNTRL0_CMD, self.cmd_byte() | CMD_FAST_READ_ENABLE);
        }
    }

    // -----------------------------------------------------------------------
    // Command engine (flashprog sb600_spi_send_command)
    // -----------------------------------------------------------------------

    fn fifo_pointer(&self) -> u8 {
        self.spibar.read8(SPI_CNTRL1_SPEED) & SPEED_FIFO_PTR_MASK
    }

    fn reset_fifo_pointer(&mut self) -> Result<(), InternalError> {
        self.spibar
            .write8(SPI_CNTRL0_CMD, self.cmd_byte() | CMD_FIFO_PTR_CLR);
        for _ in 0..FIFO_RESET_POLL_LIMIT {
            if self.fifo_pointer() == 0 {
                return Ok(());
            }
            self.host.delay_us(1);
        }
        log::error!("AMD SPI FIFO pointer reset timed out");
        self.poison = Poison::FifoTimeout;
        Err(InternalError::Io("SPI FIFO pointer reset timed out"))
    }

    fn check_fifo_pointer(&self, want: usize) -> Result<(), InternalError> {
        let have = self.fifo_pointer();
        let want = (want % SB600_FIFO_SIZE) as u8;
        if have == want {
            return Ok(());
        }
        log::error!(
            "AMD SPI FIFO pointer corruption: pointer is {have}, wanted {want}. \
             Another agent may be accessing the flash chip."
        );
        Err(InternalError::Io("SPI FIFO pointer corruption"))
    }

    fn execute_opcode(&mut self) -> Result<(), InternalError> {
        self.spibar
            .write8(SPI_CNTRL0_CMD, self.cmd_byte() | CMD_EXECUTE_OPCODE);
        for _ in 0..EXECUTE_POLL_LIMIT {
            if self.spibar.read32(SPI_CNTRL0) & CNTRL0_EXECUTE_OPCODE == 0 {
                return Ok(());
            }
            self.host.delay_us(1);
        }
        log::error!("SPI command timed out; poisoning controller until engine is idle");
        self.poison = Poison::ExecuteTimeout;
        Err(InternalError::Io("SPI transfer timeout"))
    }

    /// Send `writearr` (opcode first) and receive `readarr.len()` bytes via the
    /// 8-byte ring-buffer FIFO.
    pub fn send_command(
        &mut self,
        writearr: &[u8],
        readarr: &mut [u8],
    ) -> Result<(), InternalError> {
        self.send_command_with_size(writearr, readarr, None)
    }

    fn send_command_with_size(
        &mut self,
        writearr: &[u8],
        readarr: &mut [u8],
        fallback_size: Option<u32>,
    ) -> Result<(), InternalError> {
        let (&opcode, data) = writearr.split_first().ok_or(InternalError::Io(
            "SPI controller needs to send at least 1 byte",
        ))?;
        if self.is_restricted(opcode) {
            log::error!("Opcode {opcode:#04x} is restricted by the SPI controller");
            return Err(InternalError::AccessDenied {
                region: "SB600 restricted SPI opcode",
            });
        }
        if data.len() > SB600_MAX_TX || readarr.len() > SB600_MAX_RX {
            log::info!(
                "SPI controller cannot send {} or receive {} bytes (FIFO limit is {})",
                data.len(),
                readarr.len(),
                SB600_FIFO_SIZE
            );
            return Err(InternalError::Io("SPI controller FIFO is 8 bytes"));
        }
        self.check_command_rom_protect(opcode, data, readarr.len(), fallback_size)?;

        log::trace!(
            "sb600 cmd={opcode:#04x}, writecnt={}, readcnt={}",
            data.len(),
            readarr.len()
        );
        self.ensure_ready()?;
        self.spibar.write8(SPI_CNTRL0, opcode);

        // Workaround for SB600 controller bug: when sending only an opcode
        // (writecnt == 0), the controller drops the last received byte unless
        // RxByteCount is incremented by one.
        let readoffby1 = usize::from(data.is_empty());
        let counts = (((readarr.len() + readoffby1) as u8) << 4) | (data.len() as u8);
        self.spibar.write8(SPI_CNTRL0_COUNTS, counts);

        self.reset_fifo_pointer()?;
        for &byte in data {
            self.spibar.write8(SPI_FIFO, byte);
        }
        self.check_fifo_pointer(data.len())?;

        self.reset_fifo_pointer()?;
        self.execute_opcode()?;
        self.check_fifo_pointer(data.len() + readarr.len())?;

        // The FIFO is an 8-byte ring buffer that retains the transmitted bytes
        // before the received response; reset and skip over `data`.
        self.reset_fifo_pointer()?;
        for _ in data {
            let _ = self.spibar.read8(SPI_FIFO);
        }
        self.check_fifo_pointer(data.len())?;
        for byte in readarr.iter_mut() {
            *byte = self.spibar.read8(SPI_FIFO);
        }
        self.check_fifo_pointer(data.len() + readarr.len())?;

        if self.spibar.read8(SPI_CNTRL0_COUNTS) != counts || self.spibar.read8(SPI_CNTRL0) != opcode
        {
            log::error!(
                "Unexpected change in AMD SPI opcode or read/write count; \
                 another agent may be accessing the flash chip."
            );
            return Err(InternalError::Io("SPI opcode or read/write count changed"));
        }
        Ok(())
    }

    fn has_rom_protect(&self) -> bool {
        self.rom_protect
            .iter()
            .any(|p| p.read_protected() || p.write_protected())
    }

    fn has_write_protect(&self) -> bool {
        self.rom_protect.iter().any(|p| p.write_protected())
    }

    fn erase_op_size(&self, opcode: u8) -> Option<u32> {
        self.erase_ops[..self.erase_ops_len]
            .iter()
            .find(|&&(op, _)| op == opcode)
            .map(|&(_, size)| size)
    }

    /// Enforce `RomProtect 0..3` on raw commands whenever any slot is active.
    ///
    /// When no `RomProtect` slot is active, any non-restricted opcode is passed
    /// through unchanged. When protection is active, known non-array commands
    /// are allowed, array reads/programs/erases are checked against protected
    /// ranges, chip erase is refused if any write protection is active, and
    /// commands with unknown array footprints fail closed.
    fn check_command_rom_protect(
        &self,
        opcode: u8,
        data: &[u8],
        read_len: usize,
        fallback_size: Option<u32>,
    ) -> Result<(), InternalError> {
        if !self.has_rom_protect() {
            return Ok(());
        }
        let denied = || InternalError::AccessDenied {
            region: "SB600 Rom Protect",
        };
        let check = |access, addr, len| {
            self.check_rom_protect(access, addr, len, fallback_size)
                .map_err(|_| denied())
        };
        let addr24 = |bytes: &[u8]| u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]);

        match opcode {
            // Identification, SFDP, status, write-enable and protection-register commands.
            opcodes::RDID
            | opcodes::REMS
            | opcodes::RES
            | opcodes::RDUID
            | 0x5A // RDSFDP
            | opcodes::RDSR
            | opcodes::RDSR2
            | opcodes::RDSR3
            | opcodes::WRSR
            | opcodes::WRSR2
            | opcodes::WRSR3
            | opcodes::WREN
            | opcodes::WRDI
            | opcodes::EWSR
            | opcodes::ULBPR
            | 0x70 // Micron RFSR
            | 0x30 // Spansion/Cypress CLSR
            | 0x2B // Macronix RDSCUR
            => Ok(()),
            opcodes::READ if data.len() == 3 => {
                check(FlashAccess::Read, addr24(data), read_len as u32)
            }
            opcodes::FAST_READ if data.len() == 4 => {
                check(FlashAccess::Read, addr24(data), read_len as u32)
            }
            opcodes::PP if data.len() >= 4 => {
                check(FlashAccess::Modify, addr24(data), (data.len() - 3) as u32)
            }
            opcodes::CE_60 | opcodes::CE_C7 | 0x62 => {
                if self.has_write_protect() {
                    Err(denied())
                } else {
                    Ok(())
                }
            }
            _ => {
                if let Some(size) = self.erase_op_size(opcode)
                    && data.len() == 3
                    && size > 0
                {
                    let addr = addr24(data);
                    let start = if size.is_power_of_two() {
                        addr & !(size - 1)
                    } else {
                        addr
                    };
                    return check(FlashAccess::Modify, start, size);
                }
                log::error!(
                    "Refusing command {opcode:#04x} with unknown array footprint while Rom Protect is active"
                );
                Err(denied())
            }
        }
    }

    fn check_rom_protect(
        &self,
        access: FlashAccess,
        addr: u32,
        len: u32,
        fallback_size: Option<u32>,
    ) -> CoreResult<()> {
        let end = addr.checked_add(len).ok_or(CoreError::AddressOutOfBounds)?;
        let active = |p: &RomProtect| match access {
            FlashAccess::Read => p.read_protected(),
            FlashAccess::Modify => p.write_protected(),
        };
        if !self.rom_protect.iter().any(active) {
            return Ok(());
        }
        let Some(size) = self.chip_size.or(fallback_size) else {
            log::error!("Rom Protect is active; chip size must be known before access");
            return Err(CoreError::RegionProtected);
        };
        if end > size {
            log::error!(
                "Access {addr:#x}..{end:#x} exceeds chip size {size:#x} while Rom Protect is active"
            );
            return Err(CoreError::AddressOutOfBounds);
        }
        if let Some(protect) = self
            .rom_protect
            .iter()
            .find(|p| active(p) && p.protects(size, addr, end))
        {
            log::error!(
                "Access {addr:#x}..{end:#x} overlaps Rom Protect {:#010x}",
                protect.raw()
            );
            return Err(CoreError::RegionProtected);
        }
        Ok(())
    }

    fn execute_spi_command(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        if cmd.io_mode != IoMode::Single {
            return Err(CoreError::IoModeNotSupported);
        }
        if !cmd.dummy_cycles.is_multiple_of(8)
            || !matches!(
                cmd.address_width,
                AddressWidth::None | AddressWidth::ThreeByte
            )
            || cmd.address.is_some() != (cmd.address_width == AddressWidth::ThreeByte)
            || self.is_restricted(cmd.opcode)
        {
            return Err(CoreError::OpcodeNotSupported);
        }
        if let Some(addr) = cmd.address {
            let span = u32::try_from(cmd.write_data.len().max(cmd.read_buf.len()))
                .map_err(|_| CoreError::AddressOutOfBounds)?;
            if addr >= LIMIT_3BYTE || addr.checked_add(span).is_none_or(|end| end > LIMIT_3BYTE) {
                return Err(CoreError::AddressOutOfBounds);
            }
        }
        let write_len = cmd.header_len() + cmd.write_data.len();
        if write_len - 1 > SB600_MAX_TX || cmd.read_buf.len() > SB600_MAX_RX {
            return Err(CoreError::OpcodeNotSupported);
        }
        let mut writearr = [0u8; SB600_MAX_TX + 1];
        let header_len = cmd.encode_header(&mut writearr);
        writearr[header_len..write_len].copy_from_slice(cmd.write_data);
        self.send_command(&writearr[..write_len], cmd.read_buf)
            .map_err(InternalError::to_core_error)
    }
}

impl<H: HostAccess> Drop for Sb600SpiController<H> {
    fn drop(&mut self) {
        self.restore_settings();
    }
}

impl<H: HostAccess> Controller for Sb600SpiController<H> {
    fn is_locked(&self) -> bool {
        false
    }

    fn writes_enabled(&self) -> bool {
        true
    }

    fn enable_bios_write(&mut self) -> Result<(), InternalError> {
        Ok(())
    }

    fn controller_read(&mut self, addr: u32, buf: &mut [u8], chip_size: usize) -> CoreResult<()> {
        let len = u32::try_from(buf.len()).map_err(|_| CoreError::AddressOutOfBounds)?;
        let end = addr.checked_add(len).ok_or(CoreError::AddressOutOfBounds)?;
        if end > LIMIT_3BYTE {
            return Err(CoreError::AddressOutOfBounds);
        }
        let fallback_size = u32::try_from(chip_size).ok().filter(|&s| s > 0);
        self.check_rom_protect(FlashAccess::Read, addr, len, fallback_size)?;
        for (index, chunk) in buf.chunks_mut(SB600_MAX_RX).enumerate() {
            let current = addr + (index * SB600_MAX_RX) as u32;
            let [_, a2, a1, a0] = current.to_be_bytes();
            self.send_command_with_size(&[opcodes::READ, a2, a1, a0], chunk, fallback_size)
                .map_err(InternalError::to_core_error)?;
        }
        Ok(())
    }

    fn controller_write(&mut self, _addr: u32, _data: &[u8]) -> CoreResult<()> {
        Err(CoreError::OpcodeNotSupported)
    }

    fn controller_erase(&mut self, _addr: u32, _len: u32) -> CoreResult<()> {
        Err(CoreError::OpcodeNotSupported)
    }

    fn controller_name(&self) -> &'static str {
        self.generation.controller_name()
    }

    fn features(&self) -> SpiFeatures {
        // 3-byte addressing only; do not enter 4BA or EAR modes on an internal flash.
        SpiFeatures::NO_4BA_MODES
    }

    fn max_read_len(&self) -> usize {
        SB600_MAX_RX
    }

    fn max_write_len(&self) -> usize {
        SB600_MAX_DATA_WRITE
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        self.execute_spi_command(cmd)
    }

    fn check_access(&self, access: FlashAccess, addr: u32, len: u32) -> CoreResult<()> {
        self.check_rom_protect(access, addr, len, None)
    }

    fn probe_opcode(&self, opcode: u8) -> bool {
        !self.is_restricted(opcode)
    }

    fn chip_identified(&mut self, chip: &FlashChip) {
        self.chip_size = (chip.total_size <= LIMIT_3BYTE).then_some(chip.total_size);
        self.erase_ops = [(0, 0); 8];
        self.erase_ops_len = 0;
        for block in chip.erase_blocks.iter() {
            if block.is_chip_erase() {
                continue;
            }
            let size = block.max_block_size();
            if size == 0 {
                continue;
            }
            if let Some(existing) = self.erase_ops[..self.erase_ops_len]
                .iter_mut()
                .find(|(op, _)| *op == block.opcode)
            {
                existing.1 = existing.1.max(size);
            } else if self.erase_ops_len < self.erase_ops.len() {
                self.erase_ops[self.erase_ops_len] = (block.opcode, size);
                self.erase_ops_len += 1;
            }
        }
    }
}

impl<H: HostAccess> SpiMaster for Sb600SpiController<H> {
    fn features(&self) -> SpiFeatures {
        Controller::features(self)
    }

    fn max_read_len(&self) -> usize {
        Controller::max_read_len(self)
    }

    fn max_write_len(&self) -> usize {
        Controller::max_write_len(self)
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        self.execute_spi_command(cmd)
    }

    fn probe_opcode(&self, opcode: u8) -> bool {
        Controller::probe_opcode(self, opcode)
    }

    fn chip_identified(&mut self, chip: &FlashChip) {
        Controller::chip_identified(self, chip);
    }

    async fn delay_us(&mut self, us: u32) {
        self.host.delay_us(us);
    }

    fn check_access(&self, access: FlashAccess, addr: u32, len: u32) -> CoreResult<()> {
        Controller::check_access(self, access, addr, len)
    }

    async fn read_flash(
        &mut self,
        ctx: &FlashContext,
        addr: u32,
        buf: &mut [u8],
    ) -> CoreResult<()> {
        let len = u32::try_from(buf.len()).map_err(|_| CoreError::AddressOutOfBounds)?;
        self.check_rom_protect(FlashAccess::Read, addr, len, None)?;
        flash::spi_read(self, ctx, addr, buf).await
    }
}

#[cfg(test)]
pub(crate) mod tests;

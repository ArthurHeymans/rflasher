//! AMD SB600-family chipset discovery (read-only admission checks).
//!
//! Discovery only reads PCI configuration space and (on SB8xx/SB9xx) ACPI MMIO.
//! Rom Protect 0..3 (LPC `0x50..=0x5C`, write-once after reset on SB8xx) are
//! never modified; their values are passed to [`Sb600SpiController`] to enforce.

use super::AMD_LPC_FUNCTION;
use crate::amd_pci::AmdChipsetEnable;
#[cfg(all(feature = "std", target_os = "linux"))]
use crate::amd_sb600spi::Sb600SpiSpeed;
use crate::amd_sb600spi::regs::*;
use crate::amd_sb600spi::{
    RomProtect, Sb600Generation, Sb600Imc, Sb600SpiConfig, Sb600SpiController,
};
use crate::error::InternalError;
#[cfg(all(feature = "std", target_os = "linux"))]
use crate::host::LinuxHost;
use crate::host::{HostAccess, MmioAccess, PciAddress, PciConfigAccess};

/// Board parameters that cannot be read from PCI configuration space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sb600BoardConfig {
    /// SB8xx/SB9xx `AcpiMMioAddr` (`PM_Reg 24h`). Discovery maps 4 KiB read-only
    /// here and verifies `PM_Reg 24h` decodes at this address.
    pub acpi_mmio_base: u64,
    /// SB7xx/SP5100 only: operator assertion that the IMC is strapped off
    /// (`PM_Reg B0h[2]` is only reachable via PM index I/O after writing `ReadStrap`).
    pub sb7xx_imc_strapped_off: bool,
}

impl Default for Sb600BoardConfig {
    fn default() -> Self {
        Self {
            acpi_mmio_base: SB800_ACPI_MMIO_DEFAULT,
            sb7xx_imc_strapped_off: false,
        }
    }
}

/// SB8xx/SB9xx strap and pin-mux registers read from ACPI MMIO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sb600Straps {
    /// `AcpiMmioEn`, `PM_Reg 24h`
    pub acpi_mmio_en: u32,
    /// `Misc`, `PM_Reg C8h` (`UseBypassRom`, `BypassRomSel`)
    pub pm_misc: u32,
    /// `StrapStatus`, `Misc_Reg 80h`
    pub strap_status: u32,
    /// `StrapOverride`, `Misc_Reg 84h`
    pub strap_override: u32,
    /// `IoMux` bytes for GPIO162..=165 (`SPI_CLK`, `SPI_DO`, `SPI_DI`, `SPI_CS#`)
    pub iomux: [u8; 4],
}

/// Information about a discovered AMD SB600-family SPI controller.
#[derive(Debug, Clone)]
pub struct AmdSb600Info {
    /// Matched chipset database entry.
    pub enable: &'static AmdChipsetEnable,
    /// LPC bridge (`D14F3`) holding `SPI_BASE_ADDR`.
    pub lpc: PciAddress,
    /// SMBus controller (`D14F0`) revision ID.
    pub smbus_revision: u8,
    /// Physical base address of the 32-byte SPI register window.
    pub spibar_addr: u64,
    /// Raw `SPI_BASE_ADDR` register (`LPC 0xA0`).
    pub spi_base_addr: u32,
    /// SB8xx/SB9xx strap and pin-mux snapshot.
    pub straps: Option<Sb600Straps>,
    /// Controller configuration derived from discovery.
    pub config: Sb600SpiConfig,
}

impl AmdSb600Info {
    pub fn generation(&self) -> Sb600Generation {
        self.config.generation
    }

    /// Open the controller on Linux.
    ///
    /// # Safety
    ///
    /// `self.spibar_addr` must be the unmodified SPI register window returned by
    /// [`enable_amd_sb600`]. Prefer [`open_amd_sb600`].
    #[cfg(all(feature = "std", target_os = "linux"))]
    pub unsafe fn create_controller(&self) -> Result<Sb600SpiController<LinuxHost>, InternalError> {
        // SAFETY: forwarded to caller's safety contract.
        unsafe { Sb600SpiController::new(self.spibar_addr, self.config) }
    }

    /// Open the controller using caller-provided host access.
    ///
    /// # Safety
    ///
    /// `self.spibar_addr` must be the unmodified SPI register window returned by
    /// [`enable_amd_sb600_with_host`] for `host`.
    pub unsafe fn create_controller_with_host<H: HostAccess>(
        &self,
        host: H,
    ) -> Result<Sb600SpiController<H>, InternalError> {
        // SAFETY: forwarded to caller's safety contract.
        unsafe { Sb600SpiController::new_with_host(host, self.spibar_addr, self.config) }
    }
}

/// Check SB600/SB7xx SMBus GPIO_Enable bits (`flashprog/sb600spi.c`):
/// a set bit means the pin is configured as GPIO rather than SPI.
fn check_smbus_spi_pins<H: PciConfigAccess<Error = InternalError>>(
    host: &H,
    smbus: PciAddress,
) -> Result<(), InternalError> {
    let gpio_12_11 = host.read8(smbus, SMBUS_GPIO_12_11)?;
    let gpio_32_31 = host.read8(smbus, SMBUS_GPIO_32_31)?;
    let gpio_47 = host.read8(smbus, SMBUS_GPIO_47)?;
    let pin = |set| if set { "GPIO" } else { "SPI" };
    log::debug!(
        "GPIO11(SPI_DO)={}, GPIO12(SPI_DI)={}, GPIO31(SPI_HOLD)={}, GPIO32(SPI_CS)={}, GPIO47(SPI_CLK)={}",
        pin(gpio_12_11 & SMBUS_GPIO11_SPI_DO != 0),
        pin(gpio_12_11 & SMBUS_GPIO12_SPI_DI != 0),
        pin(gpio_32_31 & SMBUS_GPIO31_SPI_HOLD != 0),
        pin(gpio_32_31 & SMBUS_GPIO32_SPI_CS != 0),
        pin(gpio_47 & SMBUS_GPIO47_SPI_CLK != 0),
    );
    // SPI_HOLD is not used on all boards, so only DO, DI, CS and CLK are required.
    if gpio_12_11 & (SMBUS_GPIO11_SPI_DO | SMBUS_GPIO12_SPI_DI) != 0
        || gpio_32_31 & SMBUS_GPIO32_SPI_CS != 0
        || gpio_47 & SMBUS_GPIO47_SPI_CLK != 0
    {
        log::info!("SPI pins are configured as GPIOs; not enabling SPI");
        return Err(InternalError::NotSupported(
            "SB600 SPI pins are used as GPIOs",
        ));
    }
    Ok(())
}

/// Read SB8xx/SB9xx strap and pin-mux registers through ACPI MMIO.
///
/// # Safety
///
/// `base` must be the chipset's `AcpiMMioAddr` for `host`.
unsafe fn read_sb8xx_straps<H: HostAccess>(
    host: &H,
    base: u64,
) -> Result<Sb600Straps, InternalError> {
    // SAFETY: forwarded to caller; window is only read.
    let acpi = unsafe { host.map_mmio(base, ACPI_MMIO_SIZE)? };
    let straps = Sb600Straps {
        acpi_mmio_en: acpi.read32(PM_ACPI_MMIO_EN),
        pm_misc: acpi.read32(PM_MISC),
        strap_status: acpi.read32(MISC_STRAP_STATUS),
        strap_override: acpi.read32(MISC_STRAP_OVERRIDE),
        iomux: IOMUX_SPI_PINS.map(|(offset, _)| acpi.read8(offset)),
    };
    log::debug!("SB8xx straps: {straps:x?}");
    let en = straps.acpi_mmio_en;
    if en & ACPI_MMIO_DECODE_EN == 0
        || en & ACPI_MMIO_IO_SEL != 0
        || u64::from(en & ACPI_MMIO_ADDR_MASK) != base
    {
        log::error!(
            "PM_Reg 24h reads {en:#010x} through {base:#x}; ACPI MMIO is not decoded there"
        );
        return Err(InternalError::ChipsetEnable(
            "SB8xx ACPI MMIO is not decoded at the expected base",
        ));
    }
    Ok(straps)
}

/// Verify SB8xx/SB9xx SPI pin muxing and active ROM strap selection.
fn check_sb8xx_spi(straps: &Sb600Straps) -> Result<(), InternalError> {
    for ((_, pin), mux) in IOMUX_SPI_PINS.iter().zip(straps.iomux) {
        if mux & 0x03 != 0 {
            log::info!("{pin} is muxed to function {}, not SPI", mux & 0x03);
            return Err(InternalError::NotSupported(
                "SB600 SPI pins are used as GPIOs",
            ));
        }
    }
    let rom_is_spi = if straps.pm_misc & PM_MISC_USE_BYPASS_ROM != 0 {
        ((straps.pm_misc & PM_MISC_BYPASS_ROM_SEL_MASK) >> PM_MISC_BYPASS_ROM_SEL_SHIFT) == 0b11
    } else if straps.strap_override & STRAP_OVERRIDE_EN != 0 {
        straps.strap_override & STRAP_ROM_SPI_MASK == STRAP_ROM_SPI_MASK
    } else {
        straps.strap_status & STRAP_ROM_SPI_MASK == STRAP_ROM_SPI_MASK
    };
    if !rom_is_spi {
        log::info!(
            "Not strapped to SPI ROM (StrapStatus={:#010x}, StrapOverride={:#010x}, PM_Reg C8h={:#010x})",
            straps.strap_status,
            straps.strap_override,
            straps.pm_misc
        );
        return Err(InternalError::NotSupported(
            "SB600 southbridge is not strapped to SPI ROM",
        ));
    }
    Ok(())
}

/// Discover an AMD SB600-family SPI controller using caller-provided host access.
///
/// # Safety
///
/// `host` must reach the chipset at `device`, and on SB8xx/SB9xx
/// `board.acpi_mmio_base` must be its `AcpiMMioAddr`.
pub unsafe fn enable_amd_sb600_with_host<H: HostAccess>(
    host: &H,
    enable: &'static AmdChipsetEnable,
    device: PciAddress,
    board: &Sb600BoardConfig,
) -> Result<AmdSb600Info, InternalError> {
    if !enable.chipset.uses_sb600_spi() {
        return Err(InternalError::NotSupported(
            "AMD chipset does not use the SB600-family SPI controller",
        ));
    }
    let lpc = PciAddress::new(
        device.segment(),
        device.bus(),
        device.device(),
        AMD_LPC_FUNCTION,
    );
    let smbus = PciAddress::new(device.segment(), device.bus(), device.device(), 0);
    log::info!(
        "Enabling AMD {} SPI controller at {lpc}",
        enable.device_name
    );

    let lpc_id = host.read32(lpc, LPC_ID)?;
    let (lpc_vendor, lpc_device) = (lpc_id as u16, (lpc_id >> 16) as u16);
    let (smbus_vendor, smbus_device) = Sb600Generation::smbus_id(lpc_device)
        .filter(|_| Sb600Generation::lpc_vendor(lpc_device) == Some(lpc_vendor))
        .ok_or_else(|| {
            log::error!(
                "{lpc} is {lpc_vendor:04x}:{lpc_device:04x}, not an SB600-family LPC bridge"
            );
            InternalError::ChipsetEnable("Not an SB600-family LPC bridge")
        })?;
    let smbus_id = host.read32(smbus, SMBUS_ID)?;
    if smbus_id != (u32::from(smbus_device) << 16 | u32::from(smbus_vendor)) {
        log::error!("SMBus device {smbus_vendor:04x}:{smbus_device:04x} not found at {smbus}");
        return Err(InternalError::ChipsetEnable(
            "SMBus device not found, not enabling SPI",
        ));
    }
    let smbus_revision = host.read8(smbus, SMBUS_REVISION_ID)?;
    if !enable.matches(lpc_vendor, lpc_device, smbus_revision)
        && !enable.matches(smbus_vendor, smbus_device, smbus_revision)
    {
        log::error!(
            "The {} entry does not describe {lpc_vendor:04x}:{lpc_device:04x} \
             with SMBus revision {smbus_revision:#04x}",
            enable.device_name
        );
        return Err(InternalError::ChipsetEnable(
            "Chipset entry does not match the hardware",
        ));
    }
    let generation = Sb600Generation::from_ids(lpc_device, smbus_revision).ok_or_else(|| {
        log::error!(
            "SMBus revision {smbus_revision:#04x} of LPC {lpc_device:04x} is not a known \
             SB600-family revision"
        );
        InternalError::ChipsetEnable("Unknown SB600-family SMBus revision")
    })?;
    log::debug!("{generation} detected (SMBus revision {smbus_revision:#04x})");
    if !generation.is_supported() {
        log::error!(
            "{generation} uses an untested SB600-family SPI controller variant; not enabling it"
        );
        return Err(InternalError::NotSupported(
            "AMD FCH SB600-family SPI controller variant is untested",
        ));
    }

    let spi_base_addr = host.read32(lpc, LPC_SPI_BASE_ADDR)?;
    if spi_base_addr == 0xffff_ffff {
        return Err(InternalError::ChipsetEnable(
            "SPI_BASE_ADDR reads all 0xff, aborting",
        ));
    }
    let spibar_addr = u64::from(spi_base_addr & SPI_BASE_ADDR_MASK);
    let spi_rom_enable = spi_base_addr & SPI_BASE_ROM_ENABLE != 0;
    log::debug!(
        "SPI_BASE_ADDR={spi_base_addr:#010x}: base={spibar_addr:#x}, SpiRomEnable={}",
        u8::from(spi_rom_enable)
    );
    if spibar_addr == 0 {
        return Err(InternalError::NotSupported("SB600 SPI not in use"));
    }
    if generation >= Sb600Generation::Sb7xx && !spi_rom_enable {
        log::info!("SpiRomEnable is clear: the SPI ROM is disabled");
        return Err(InternalError::NotSupported("SB600 SPI not in use"));
    }

    let mut rom_protect = [RomProtect::NONE; 4];
    for (index, slot) in rom_protect.iter_mut().enumerate() {
        *slot = RomProtect::from_raw(host.read32(lpc, LPC_ROM_PROTECT0 + 4 * index as u16)?);
    }

    let straps = match generation {
        Sb600Generation::Sb89xx => {
            // SAFETY: LPC and SMBus were verified as SB8xx/SB9xx above; caller
            // guarantees `board.acpi_mmio_base` is its ACPI MMIO base.
            let straps = unsafe { read_sb8xx_straps(host, board.acpi_mmio_base)? };
            check_sb8xx_spi(&straps)?;
            Some(straps)
        }
        _ => {
            check_smbus_spi_pins(host, smbus)?;
            None
        }
    };

    if generation == Sb600Generation::Sb7xx {
        let host_control = host.read8(lpc, LPC_HOST_CONTROL)?;
        log::debug!(
            "SpiOpEnInLpcMode={} PrefetchEnSPIFromHost={}",
            u8::from(host_control & HOST_CONTROL_SPI_OP_IN_LPC != 0),
            u8::from(host_control & HOST_CONTROL_PREFETCH_EN != 0),
        );
    }

    let imc = if generation.has_imc() {
        let running = host.read8(lpc, LPC_PCI_CONTROL)? & PCI_CONTROL_IMC_PRESENT != 0;
        let strapped = match straps {
            Some(straps) => Some(
                straps.strap_status & STRAP_IMC_ENABLE != 0
                    || (straps.strap_override & STRAP_OVERRIDE_EN != 0
                        && straps.strap_override & STRAP_IMC_ENABLE != 0),
            ),
            None if board.sb7xx_imc_strapped_off => Some(false),
            None => None,
        };
        match (running, strapped) {
            (true, _) | (_, Some(true)) => Sb600Imc::Enabled,
            (false, Some(false)) => Sb600Imc::StrappedOff,
            (false, None) => Sb600Imc::Unknown,
        }
    } else {
        Sb600Imc::NotPresent
    };
    log::debug!("IMC: {imc:?}");
    match imc {
        Sb600Imc::Enabled => {
            log::error!(
                "The IMC is strapped on or running and may use the flash; \
                 rflasher does not put it to sleep"
            );
            return Err(InternalError::NotSupported("SB600-family IMC is enabled"));
        }
        Sb600Imc::Unknown => {
            log::error!(
                "The SB7xx IMC strap (PM_Reg B0h[2]) cannot be read through PCI configuration \
                 space; pass sb7xx_imc=off if the IMC is strapped off"
            );
            return Err(InternalError::NotSupported("SB7xx IMC strap unknown"));
        }
        Sb600Imc::NotPresent | Sb600Imc::StrappedOff => {}
    }

    Ok(AmdSb600Info {
        enable,
        lpc,
        smbus_revision,
        spibar_addr,
        spi_base_addr,
        straps,
        config: Sb600SpiConfig {
            imc,
            rom_protect,
            ..Sb600SpiConfig::new(generation)
        },
    })
}

/// Discover an AMD SB600-family SPI controller from Linux userspace.
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn enable_amd_sb600(
    enable: &'static AmdChipsetEnable,
    device: PciAddress,
    sb7xx_imc_strapped_off: bool,
) -> Result<AmdSb600Info, InternalError> {
    let board = Sb600BoardConfig {
        sb7xx_imc_strapped_off,
        ..Sb600BoardConfig::default()
    };
    // SAFETY: Linux PCI config goes through sysfs; ACPI MMIO is only mapped on
    // verified SB8xx/SB9xx at the documented default and validated via PM_Reg 24h.
    unsafe { enable_amd_sb600_with_host(&LinuxHost::new(), enable, device, &board) }
}

/// Discover and open an AMD SB600-family SPI controller from Linux userspace.
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn open_amd_sb600(
    enable: &'static AmdChipsetEnable,
    device: PciAddress,
    speed: Option<Sb600SpiSpeed>,
    sb7xx_imc_strapped_off: bool,
) -> Result<Sb600SpiController<LinuxHost>, InternalError> {
    let mut info = enable_amd_sb600(enable, device, sb7xx_imc_strapped_off)?;
    if speed.is_some() {
        info.config.speed = speed;
    }
    // SAFETY: `info.spibar_addr` was just discovered from this host's LPC bridge.
    unsafe { info.create_controller() }
}

#[cfg(test)]
mod tests;

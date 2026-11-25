//! AMD chipset enable and initialization
//!
//! This module contains the chipset enable functions for AMD platforms,
//! particularly the SPI100 controller initialization.

use crate::amd_pci::AmdChipsetEnable;
use crate::amd_spi100::Spi100Controller;
use crate::error::InternalError;
#[cfg(all(feature = "std", target_os = "linux"))]
use crate::host::DefaultPciAccess;
#[cfg(all(feature = "std", target_os = "linux"))]
use crate::host::LinuxHost;
use crate::host::{HostAccess, PciAddress, PciConfigAccess};

/// PCI register offset for SPI BAR in AMD FCH LPC device
const AMD_SPI_BAR_OFFSET: u8 = 0xa0;

/// PCI register offset for ROM Range 2 in AMD FCH LPC device
const AMD_ROM_RANGE2_OFFSET: u8 = 0x6c;

/// LPC function number in AMD FCH
const AMD_LPC_FUNCTION: u8 = 3;

/// Information about a detected AMD chipset with SPI100
#[derive(Debug)]
pub struct AmdSpi100Info {
    /// The chipset enable entry
    pub enable: &'static AmdChipsetEnable,
    /// PCI segment/domain
    pub domain: u16,
    /// PCI bus number
    pub bus: u8,
    /// PCI device number
    pub device: u8,
    /// PCI function number
    pub function: u8,
    /// PCI revision ID
    pub revision_id: u8,
    /// Physical address of SPI BAR
    pub spibar_addr: u64,
    /// Physical address of memory-mapped ROM (if enabled)
    pub memory_addr: Option<u64>,
    /// Size of memory-mapped ROM region
    pub mapped_len: usize,
    /// Whether SPI ROM is enabled
    pub spirom_enabled: bool,
}

impl AmdSpi100Info {
    /// Create a new SPI100 controller instance from this info using Linux host access.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `self.spibar_addr`, `self.memory_addr`, and
    /// `self.mapped_len` describe valid MMIO/ROM windows on the current Linux host
    /// (for example, returned unmodified by [`enable_amd_spi100_with_host`] using
    /// [`DefaultPciAccess`]).
    #[cfg(all(feature = "std", target_os = "linux"))]
    pub unsafe fn create_controller(&self) -> Result<Spi100Controller<LinuxHost>, InternalError> {
        // SAFETY: forwarded to the caller's safety contract.
        unsafe { Spi100Controller::new(self.spibar_addr, self.memory_addr, self.mapped_len) }
    }

    /// Create a new SPI100 controller instance from this info using caller-provided host access.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `self.spibar_addr`, `self.memory_addr`, and
    /// `self.mapped_len` describe valid MMIO/ROM windows for `host` (for example,
    /// returned unmodified by [`enable_amd_spi100_with_host`] on the same host).
    #[cfg(any(all(feature = "std", target_os = "linux"), not(feature = "std")))]
    pub unsafe fn create_controller_with_host<H: HostAccess>(
        &self,
        host: H,
    ) -> Result<Spi100Controller<H>, InternalError> {
        // SAFETY: forwarded to the caller's safety contract.
        unsafe {
            Spi100Controller::new_with_host(
                host,
                self.spibar_addr,
                self.memory_addr,
                self.mapped_len,
            )
        }
    }
}

/// Decode AMD LPC ROM Range 2 register into `(start, end_inclusive, length)`.
///
/// Bits 15:0 hold `start[31:16]`; bits 31:16 hold `end[31:16]` (inclusive,
/// with low 16 bits fixed to `0xffff`). Arithmetic is done in `u64` so a full
/// 4 GiB range (`0xffff_0000`) does not overflow `u32`.
fn decode_rom_range2(rom_range2: u32) -> (u64, u64, u64) {
    let start = u64::from(rom_range2 & 0xffff) << 16;
    let end = u64::from(rom_range2 | 0xffff);
    let len = if end >= start { end - start + 1 } else { 0 };
    (start, end, len)
}

/// Enables AMD SPI100 using host-provided PCI config access.
///
/// This helper contains the platform-independent BAR and ROM range discovery
/// logic. Linux userspace and embedded firmware can use the same code with
/// different [`PciConfigAccess`] implementations.
pub fn enable_amd_spi100_with_host<H: PciConfigAccess<Error = InternalError>>(
    host: &H,
    enable: &'static AmdChipsetEnable,
    smbus_bdf: PciAddress,
    revision_id: u8,
) -> Result<AmdSpi100Info, InternalError> {
    if !enable.chipset.uses_spi100() {
        return Err(InternalError::NotSupported(
            "AMD chipset does not use an SPI100 controller",
        ));
    }

    log::info!(
        "Enabling AMD {} SPI100 controller at {:02x}:{:02x}.0",
        enable.device_name,
        smbus_bdf.bus(),
        smbus_bdf.device()
    );

    let lpc_bdf = PciAddress::new(
        smbus_bdf.segment(),
        smbus_bdf.bus(),
        smbus_bdf.device(),
        AMD_LPC_FUNCTION,
    );
    let spibar = host.read32(lpc_bdf, AMD_SPI_BAR_OFFSET as u16)?;

    if spibar == 0xffff_ffff {
        return Err(InternalError::ChipsetEnable(
            "SPI100 BAR reads all 0xff, aborting",
        ));
    }

    log::debug!(
        "SPI BAR config: AltSpiCSEnable={} SpiRomEnable={} AbortEnable={} \
         RouteTpm2Spi={} PspSpiMmioSel={}",
        spibar & 1,
        (spibar >> 1) & 1,
        (spibar >> 2) & 1,
        (spibar >> 3) & 1,
        (spibar >> 4) & 1,
    );

    let spirom_enabled = (spibar & (1 << 1)) != 0;
    let phys_spibar = (spibar & !0xff) as u64;

    if phys_spibar == 0 {
        if spirom_enabled {
            log::error!("SPI ROM is enabled but SPI BAR is unconfigured");
            return Err(InternalError::ChipsetEnable(
                "SPI BAR unconfigured but SPI ROM enabled",
            ));
        }

        log::debug!("SPI100 not used");
        return Err(InternalError::NotSupported("SPI100 not in use"));
    }

    log::debug!("SPI100 BAR at {:#010x}", phys_spibar);

    let rom_range2 = host.read32(lpc_bdf, AMD_ROM_RANGE2_OFFSET as u16)?;
    let (rom_range_start, rom_range_end, rom_range_len) = decode_rom_range2(rom_range2);
    let mapped_len = usize::try_from(rom_range_len).unwrap_or(0);

    log::debug!(
        "ROM Range 2: {:#010x}..{:#010x} ({} KB)",
        rom_range_start,
        rom_range_end,
        rom_range_len / 1024
    );

    let memory_addr = if spirom_enabled && mapped_len > 0 && rom_range_start != 0 {
        Some(rom_range_start)
    } else {
        None
    };

    if memory_addr.is_some() {
        log::info!(
            "Memory-mapped flash access enabled at {:#010x}",
            rom_range_start
        );
    }

    Ok(AmdSpi100Info {
        enable,
        domain: smbus_bdf.segment(),
        bus: smbus_bdf.bus(),
        device: smbus_bdf.device(),
        function: 0,
        revision_id,
        spibar_addr: phys_spibar,
        memory_addr,
        mapped_len,
        spirom_enabled,
    })
}

/// Enable AMD SPI100 controller
///
/// This function initializes the AMD SPI100 controller by:
/// 1. Reading the SPI BAR from the LPC device (function 3)
/// 2. Reading the ROM range configuration
/// 3. Setting up memory-mapped flash access if enabled
/// 4. Creating the SPI100 controller instance
///
/// # Arguments
///
/// * `enable` - The chipset enable entry
/// * `bus` - PCI bus number of the SMBus device
/// * `device` - PCI device number of the SMBus device
/// * `revision_id` - PCI revision ID
///
/// # Returns
///
/// Information needed to create the SPI100 controller
#[cfg(all(feature = "std", target_os = "linux"))]
pub fn enable_amd_spi100(
    enable: &'static AmdChipsetEnable,
    bus: u8,
    device: u8,
    revision_id: u8,
) -> Result<AmdSpi100Info, InternalError> {
    enable_amd_spi100_with_host(
        &DefaultPciAccess,
        enable,
        PciAddress::new(0, bus, device, 0),
        revision_id,
    )
}

/// Stub for unsupported non-Linux platforms. Embedded firmware should call
/// [`enable_amd_spi100_with_host`] with its own [`PciConfigAccess`] backend.
#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn enable_amd_spi100(
    _enable: &'static AmdChipsetEnable,
    _bus: u8,
    _device: u8,
    _revision_id: u8,
) -> Result<AmdSpi100Info, InternalError> {
    Err(InternalError::NotSupported(
        "AMD chipset enable not available",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amd_pci::AMD_CHIPSETS;
    use crate::host::tests::FakeHost;

    fn spi100_enable() -> &'static AmdChipsetEnable {
        AMD_CHIPSETS
            .iter()
            .find(|chipset| chipset.chipset.uses_spi100())
            .expect("AMD SPI100 chipset table entry")
    }

    fn non_spi100_enable() -> &'static AmdChipsetEnable {
        AMD_CHIPSETS
            .iter()
            .find(|chipset| !chipset.chipset.uses_spi100())
            .expect("non-SPI100 AMD chipset table entry")
    }

    #[test]
    fn test_rom_range_calculation() {
        assert_eq!(
            decode_rom_range2(0xff00_0000),
            (0x0000_0000, 0xff00_ffff, 0xff01_0000)
        );
        // Full 4 GiB range must not overflow u32 arithmetic.
        assert_eq!(
            decode_rom_range2(0xffff_0000),
            (0x0000_0000, 0xffff_ffff, 0x1_0000_0000)
        );
        // Standard 16 MiB window at top of 4 GiB.
        assert_eq!(
            decode_rom_range2(0xffff_ff00),
            (0xff00_0000, 0xffff_ffff, 0x0100_0000)
        );
        // Inverted range (start > end) is disabled.
        assert_eq!(
            decode_rom_range2(0xff00_ffff),
            (0xffff_0000, 0xff00_ffff, 0)
        );
    }

    #[test]
    fn test_enable_amd_spi100_rejects_non_spi100_before_pci_access() {
        let mut host = FakeHost::default();
        host.deny_reads();
        host.deny_writes();

        let err = enable_amd_spi100_with_host(
            &host,
            non_spi100_enable(),
            PciAddress::new(0, 0, 0x14, 0),
            0x49,
        )
        .unwrap_err();
        assert!(matches!(err, InternalError::NotSupported(_)));
        assert!(host.writes().is_empty());
    }

    #[test]
    fn test_enable_amd_spi100_with_host_extracts_bars() {
        let host = FakeHost::default();
        let lpc_bdf = PciAddress::new(0, 0, 0x14, AMD_LPC_FUNCTION);
        host.set_config32(lpc_bdf, AMD_SPI_BAR_OFFSET as u16, 0xfed8_0203);
        host.set_config32(lpc_bdf, AMD_ROM_RANGE2_OFFSET as u16, 0xff00_0000);

        let info = enable_amd_spi100_with_host(
            &host,
            spi100_enable(),
            PciAddress::new(0, 0, 0x14, 0),
            0x51,
        )
        .unwrap();

        assert_eq!(info.domain, 0);
        assert_eq!(info.spibar_addr, 0xfed8_0200);
        assert!(info.spirom_enabled);
        assert_eq!(info.memory_addr, None);
        assert_eq!(info.mapped_len, 0xff01_0000);
    }

    #[test]
    fn test_enable_amd_spi100_with_host_handles_full_4gib_rom_range2() {
        let host = FakeHost::default();
        let lpc_bdf = PciAddress::new(0, 0, 0x14, AMD_LPC_FUNCTION);
        host.set_config32(lpc_bdf, AMD_SPI_BAR_OFFSET as u16, 0xfed8_0203);
        host.set_config32(lpc_bdf, AMD_ROM_RANGE2_OFFSET as u16, 0xffff_0000);

        let info = enable_amd_spi100_with_host(
            &host,
            spi100_enable(),
            PciAddress::new(0, 0, 0x14, 0),
            0x51,
        )
        .unwrap();
        assert_eq!(info.memory_addr, None);
    }

    #[test]
    fn test_enable_amd_spi100_with_host_rejects_unconfigured_bar() {
        let host = FakeHost::default();
        let lpc_bdf = PciAddress::new(0, 0, 0x14, AMD_LPC_FUNCTION);
        host.set_config32(lpc_bdf, AMD_SPI_BAR_OFFSET as u16, 0xffff_ffff);

        let err = enable_amd_spi100_with_host(
            &host,
            spi100_enable(),
            PciAddress::new(0, 0, 0x14, 0),
            0x51,
        )
        .unwrap_err();
        assert!(matches!(err, InternalError::ChipsetEnable(_)));
    }
}

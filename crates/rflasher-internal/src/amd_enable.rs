//! AMD chipset enable and initialization
//!
//! This module contains the chipset enable functions for AMD platforms,
//! particularly the SPI100 controller initialization.

use crate::amd_pci::AmdChipsetEnable;
use crate::amd_spi100::Spi100Controller;
use crate::error::InternalError;
use crate::pci::pci_read_config32;

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
    /// Create a new SPI100 controller instance from this info
    pub fn create_controller(&self) -> Result<Spi100Controller, InternalError> {
        Spi100Controller::new(self.spibar_addr, self.memory_addr, self.mapped_len)
    }
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
    log::info!(
        "Enabling AMD {} SPI100 controller at {:02x}:{:02x}.0",
        enable.device_name,
        bus,
        device
    );

    // Read SPI BAR from LPC device (function 3)
    let spibar = pci_read_config32(bus, device, AMD_LPC_FUNCTION, AMD_SPI_BAR_OFFSET)?;

    if spibar == 0xffffffff {
        return Err(InternalError::ChipsetEnable(
            "SPI100 BAR reads all 0xff, aborting",
        ));
    }

    // Log SPI BAR configuration bits
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

    // Extract physical SPI BAR address (lower 8 bits are config/reserved)
    let phys_spibar = (spibar & !0xff) as u64;

    if phys_spibar == 0 {
        if spirom_enabled {
            log::error!("SPI ROM is enabled but SPI BAR is unconfigured");
            return Err(InternalError::ChipsetEnable(
                "SPI BAR unconfigured but SPI ROM enabled",
            ));
        } else {
            log::debug!("SPI100 not used");
            return Err(InternalError::NotSupported("SPI100 not in use"));
        }
    }

    log::debug!("SPI100 BAR at {:#010x}", phys_spibar);

    // Read ROM Range 2 configuration (used for memory mapping below 4GB)
    let rom_range2 = pci_read_config32(bus, device, AMD_LPC_FUNCTION, AMD_ROM_RANGE2_OFFSET)?;
    let rom_range_end = rom_range2 | 0xffff;
    let rom_range_start = (rom_range2 & 0xffff) << 16;
    let mapped_len = if rom_range_end > rom_range_start {
        (rom_range_end - rom_range_start + 1) as usize
    } else {
        0
    };

    log::debug!(
        "ROM Range 2: {:#010x}..{:#010x} ({} KB)",
        rom_range_start,
        rom_range_end,
        mapped_len / 1024
    );

    // Determine if we should use memory mapping
    let memory_addr = if spirom_enabled && mapped_len > 0 {
        Some(rom_range_start as u64)
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
        bus,
        device,
        function: 0, // SMBus is at function 0
        revision_id,
        spibar_addr: phys_spibar,
        memory_addr,
        mapped_len,
        spirom_enabled,
    })
}

/// Stub for non-Linux platforms
#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn enable_amd_spi100(
    _enable: &'static AmdChipsetEnable,
    _bus: u8,
    _device: u8,
    _revision_id: u8,
) -> Result<AmdSpi100Info, InternalError> {
    Err(InternalError::NotSupported(
        "AMD chipset enable only supported on Linux",
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_rom_range_calculation() {
        // ROM Range 2 = 0xff000000
        let rom_range2: u32 = 0xff00_0000;
        let rom_range_end = rom_range2 | 0xffff;
        let rom_range_start = (rom_range2 & 0xffff) << 16;
        let mapped_len = if rom_range_end > rom_range_start {
            (rom_range_end - rom_range_start + 1) as usize
        } else {
            0
        };

        assert_eq!(rom_range_start, 0x0000_0000);
        assert_eq!(rom_range_end, 0xff00_ffff);
        assert_eq!(mapped_len, 0xff01_0000);
    }
}

//! SPIBAR discovery and Intel PCI configuration function helpers.

use crate::DetectedChipset;
use crate::chipset::IchChipset;
use crate::error::InternalError;
use crate::host::{PciAddress, PciConfigAccess};
use crate::ich_regs::*;

pub(super) fn uses_silvermont_sbase(chipset: &DetectedChipset) -> bool {
    chipset.chipset_type() == IchChipset::BayTrail
        && matches!(chipset.enable.device_id, 0x0f1c..=0x0f1f | 0x1f38..=0x1f3b | 0x229c)
}

pub(crate) fn is_visible_spi_id(id: u16) -> bool {
    matches!(
        id,
        0x9d24
            | 0xa124
            | 0xa2a4
            | 0xa1a4
            | 0xa224
            | 0x9da4
            | 0xa324
            | 0x02a4
            | 0x06a4
            | 0x34a4
            | 0x4da4
            | 0xa3a4
            | 0xa0a4
            | 0x43a4
            | 0x51a4
            | 0x54a4
            | 0x7a24
            | 0x7aa4
            | 0x4b24
            | 0x5a96
            | 0x3196
            | 0x7e23
            | 0xa823
            | 0xae23
    )
}

/// PCI function holding the BIOS control register and, on PCH100+, SPI BAR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct IntelConfigFunction {
    pub(crate) address: PciAddress,
    /// Firmware hides this function's vendor/device IDs, so the OS does not
    /// enumerate it and its config space needs a raw access mechanism.
    pub(crate) hidden: bool,
}

/// PCI location of the BIOS control register and, on PCH100+, SPI BAR.
/// IDs and bridge mappings follow flashprog's chipset_enable.c. Do not infer
/// a location for other generations or IDs: an unrelated function may exist.
pub(crate) fn intel_config_function(
    chipset: &DetectedChipset,
) -> Result<IntelConfigFunction, InternalError> {
    let actual = IntelConfigFunction {
        address: PciAddress::new(
            chipset.domain,
            chipset.bus,
            chipset.device,
            chipset.function,
        ),
        hidden: false,
    };
    let id = chipset.enable.device_id;
    if is_visible_spi_id(id) && chipset.chipset_type().is_pch100_compatible() {
        return Ok(actual);
    }
    // These are the mappings flashprog accesses via PCI_ACCESS_I386_TYPE1
    // (enable_flash_pch100_or_c620) because the SPI function may be hidden.
    let location = match (chipset.chipset_type(), id) {
        (IchChipset::Series100SunrisePoint, _)
        | (IchChipset::C620Lewisburg, 0xa1c0..=0xa1cd | 0xa240..=0xa249)
        | (IchChipset::Series300CannonPoint, 0xa3c8 | 0xa3da) => (0x1f, 5),
        (IchChipset::ApolloLake, 0x5ae8) | (IchChipset::GeminiLake, 0x31e8 | 0x3197) => (0x0d, 2),
        (generation, _) if !generation.is_pch100_compatible() => return Ok(actual),
        _ => {
            return Err(InternalError::NotSupported(
                "No validated SPI PCI device mapping for this chipset",
            ));
        }
    };
    Ok(IntelConfigFunction {
        address: PciAddress::new(chipset.domain, chipset.bus, location.0, location.1),
        hidden: true,
    })
}

/// PCI address of [`intel_config_function`].
pub(crate) fn intel_config_bdf(chipset: &DetectedChipset) -> Result<PciAddress, InternalError> {
    intel_config_function(chipset).map(|function| function.address)
}

/// Gets the SPI BAR physical address using host-provided PCI config access.
/// Hidden SPI functions must be reachable through `host`; on Linux,
/// [`IchSpiController::new`] routes them to raw x86 config access.
pub fn get_spibar_address_with_host<H: PciConfigAccess<Error = InternalError>>(
    host: &H,
    chipset: &DetectedChipset,
) -> Result<u64, InternalError> {
    let generation = chipset.chipset_type();

    if generation.is_pch100_compatible() {
        let IntelConfigFunction {
            address: spi_bdf,
            hidden,
        } = intel_config_function(chipset)?;
        let read_config = |offset| {
            host.read32(spi_bdf, offset).map_err(|e| {
                log::warn!("Cannot read SPI PCI configuration at {spi_bdf}: {e}");
                InternalError::ChipsetEnable("SPI PCI config unavailable or access denied")
            })
        };
        let identity = read_config(0)?;
        let device = (identity >> 16) as u16;
        let ids_hidden = hidden && identity == u32::MAX;
        let valid = if ids_hidden {
            // Firmware hides the IDs of this function; like flashprog, rely
            // on the LPC/eSPI device mapping and the SPI BAR checks below.
            log::debug!("SPI PCI function {spi_bdf} has hidden IDs");
            true
        } else if is_visible_spi_id(chipset.enable.device_id) {
            device == chipset.enable.device_id
        } else {
            match generation {
                // SPT-LP, SPT-H (332691: D31:F5 DID A124h) and the 200 Series.
                IchChipset::Series100SunrisePoint => matches!(device, 0x9d24 | 0xa124 | 0xa2a4),
                IchChipset::C620Lewisburg => matches!(device, 0xa1a4 | 0xa224),
                IchChipset::Series300CannonPoint => device == 0xa3a4,
                IchChipset::ApolloLake => device == 0x5a96,
                IchChipset::GeminiLake => device == 0x3196,
                _ => false,
            }
        };
        if !valid || (!ids_hidden && identity as u16 != 0x8086) {
            return Err(InternalError::ChipsetEnable(
                "PCI function is not a validated Intel SPI controller",
            ));
        }
        let spibar_raw = read_config(PCI_REG_SPIBAR as u16)?;
        // SPI BAR is a 32-bit memory BAR; never treat I/O or other BAR
        // encodings as a physical MMIO address.
        if spibar_raw & 0x7 != 0 {
            return Err(InternalError::ChipsetEnable(
                "SPI BAR is not a 32-bit memory BAR",
            ));
        }
        let addr = spibar_raw & 0xffff_f000;

        if addr == 0 || addr == 0xffff_f000 {
            return Err(InternalError::ChipsetEnable(
                "SPIBAR is invalid - SPI controller may be hidden or disabled by firmware",
            ));
        }

        Ok(addr as u64)
    } else if uses_silvermont_sbase(chipset) {
        let lpc_bdf = PciAddress::new(
            chipset.domain,
            chipset.bus,
            chipset.device,
            chipset.function,
        );
        let sbase = host.read32(lpc_bdf, 0x54)? & 0xffff_fe00;
        if sbase == 0 || sbase == 0xffff_fe00 {
            return Err(InternalError::ChipsetEnable("SBASE is invalid"));
        }
        Ok(u64::from(sbase))
    } else if generation.is_ich9_compatible() || generation == IchChipset::Ich7 {
        let lpc_bdf = PciAddress::new(
            chipset.domain,
            chipset.bus,
            chipset.device,
            chipset.function,
        );
        let rcba = host.read32(lpc_bdf, PCI_REG_RCBA as u16)?;
        if rcba & 1 == 0 {
            return Err(InternalError::ChipsetEnable("RCBA not enabled"));
        }

        let rcba_base = (rcba & !0x3fff) as u64;
        let spi_offset = match generation {
            // flashprog: SPIBAR is at RCRB+0x3020 for ICH7 and ICH8,
            // and at RCRB+0x3800 for ICH9 and newer legacy-RCBA chipsets.
            IchChipset::Ich7 | IchChipset::Ich8 => RCBA_SPI_OFFSET_ICH7,
            _ => RCBA_SPI_OFFSET_ICH9,
        };

        Ok(rcba_base + spi_offset as u64)
    } else {
        Err(InternalError::NotSupported(
            "Unsupported chipset generation",
        ))
    }
}

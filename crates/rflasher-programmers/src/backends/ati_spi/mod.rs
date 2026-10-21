//! ATI/AMD Radeon GPU VBIOS SPI flash programmer.
//!
//! Accesses the SPI flash on discrete AMD/ATI GPUs (R600 through Polaris)
//! through the GPU ROM controller, reached via PCI MMIO. Ported from
//! flashrom `ati_spi.c`.
//!
//! This backend is Linux-only: it enumerates GPUs through sysfs and maps the
//! MMIO BAR with `mmap`. It is intentionally not available on `wasm` (or any
//! non-Linux target).

mod driver;
mod error;
mod ids;
mod options;
mod ownership;

pub use driver::AtiSpiController;
pub use error::AtiSpiError;
pub use ids::{ATI_SPI_DEVICES, ATI_VID, AtiSpiDevice, AtiSpiType, TestStatus};
pub use options::{AtiSpiOptions, parse_options, schema};

/// A supported ATI/AMD GPU discovered on the PCI bus.
#[derive(Debug, Clone)]
pub struct DetectedAtiGpu {
    /// Matched device-table entry.
    pub device: &'static AtiSpiDevice,
    /// PCI address of the GPU function.
    pub address: rflasher_pci::PciAddress,
    /// BAR index (0-5) holding the MMIO registers.
    pub bar: u8,
}

impl DetectedAtiGpu {
    /// Short device name from the ID table.
    pub fn name(&self) -> &'static str {
        self.device.device_name
    }

    /// GPU family name (e.g. `"Polaris"`, `"R600"`).
    pub fn family(&self) -> &'static str {
        self.device.spi_type.family_name()
    }

    /// Map the MMIO BAR and initialize the SPI controller.
    pub fn create_controller(&self) -> Result<AtiSpiController, AtiSpiError> {
        AtiSpiController::new(self.address)
    }
}

/// Scan the PCI bus for supported ATI/AMD GPUs.
pub fn scan_for_ati_gpus() -> Result<Vec<DetectedAtiGpu>, AtiSpiError> {
    let pci = rflasher_pci::SysfsPci::system();
    let mut found = Vec::new();
    for pci_dev in pci.enumerate()? {
        let Some(device) = ids::find_ati_spi_device(pci_dev.vendor_id, pci_dev.device_id) else {
            continue;
        };
        let bar = (device.spi_type.io_bar() - 0x10) / 4;
        found.push(DetectedAtiGpu {
            device,
            address: pci_dev.address,
            bar,
        });
    }
    Ok(found)
}

/// Return the sole matching GPU, refusing ambiguous selection.
pub fn find_ati_gpu() -> Result<Option<DetectedAtiGpu>, AtiSpiError> {
    select_gpu(scan_for_ati_gpus()?, None)
}

/// Select and open a GPU using the same options as the programmer catalog.
pub fn open(options: &[(&str, &str)]) -> Result<AtiSpiController, AtiSpiError> {
    let config = parse_options(options).map_err(AtiSpiError::Options)?;
    let gpu = select_gpu(scan_for_ati_gpus()?, config.pci)?
        .ok_or_else(|| AtiSpiError::Selection("No matching ATI/AMD GPU found".into()))?;
    log::info!(
        "Using GPU: {} ({}) at {}",
        gpu.name(),
        gpu.family(),
        gpu.address
    );
    gpu.create_controller()
}

fn select_gpu(
    gpus: Vec<DetectedAtiGpu>,
    address: Option<rflasher_pci::PciAddress>,
) -> Result<Option<DetectedAtiGpu>, AtiSpiError> {
    if let Some(address) = address {
        return gpus
            .into_iter()
            .find(|gpu| gpu.address == address)
            .map(Some)
            .ok_or_else(|| {
                AtiSpiError::Selection(format!("No matching ATI/AMD GPU at {address}"))
            });
    }
    if gpus.len() > 1 {
        let addresses = gpus
            .iter()
            .map(|gpu| gpu.address.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AtiSpiError::Selection(format!(
            "Multiple ATI/AMD GPUs found ({addresses}); select one with ati_spi:pci=DDDD:BB:DD.F"
        )));
    }
    Ok(gpus.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rflasher_pci::PciAddress;

    fn gpu(segment: u16, bus: u8) -> DetectedAtiGpu {
        DetectedAtiGpu {
            device: ids::find_ati_spi_device(ATI_VID, 0x67df).unwrap(),
            address: PciAddress::new(segment, bus, 0, 0),
            bar: 5,
        }
    }

    #[test]
    fn selection_is_unambiguous_and_matches_the_entire_bdf() {
        assert!(select_gpu(vec![], None).unwrap().is_none());
        assert_eq!(
            select_gpu(vec![gpu(0, 3)], None).unwrap().unwrap().address,
            gpu(0, 3).address
        );
        assert!(select_gpu(vec![gpu(0, 3), gpu(1, 3)], None).is_err());
        assert_eq!(
            select_gpu(vec![gpu(0, 3), gpu(1, 3)], Some(gpu(1, 3).address))
                .unwrap()
                .unwrap()
                .address,
            gpu(1, 3).address
        );
        assert!(select_gpu(vec![gpu(0, 3)], Some(gpu(1, 3).address)).is_err());
    }
}

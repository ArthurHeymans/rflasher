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

pub use driver::AtiSpiController;
pub use error::AtiSpiError;
pub use ids::{ATI_SPI_DEVICES, ATI_VID, AtiSpiDevice, AtiSpiType, TestStatus};

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
        AtiSpiController::new(
            self.device.vendor_id,
            self.device.device_id,
            self.address,
            self.bar,
        )
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

/// Return the first supported ATI/AMD GPU on the PCI bus, if any.
pub fn find_ati_gpu() -> Result<Option<DetectedAtiGpu>, AtiSpiError> {
    Ok(scan_for_ati_gpus()?.into_iter().next())
}

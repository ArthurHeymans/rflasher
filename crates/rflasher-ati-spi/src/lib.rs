//! ATI/AMD Radeon GPU SPI flash programmer.

#![cfg_attr(not(feature = "std"), no_std)]
extern crate alloc;

mod ati_pci;
mod ati_spi;
mod error;

pub use ati_pci::{
    ATI_SPI_DEVICES, ATI_VID, AtiSpiDevice, AtiSpiType, TestStatus, find_ati_spi_device,
};
pub use ati_spi::AtiSpiController;
pub use error::AtiSpiError;

#[derive(Debug, Clone)]
pub struct DetectedAtiGpu {
    pub device: &'static AtiSpiDevice,
    pub address: rflasher_pci::PciAddress,
    pub bar: u8,
}

impl DetectedAtiGpu {
    pub fn name(&self) -> &'static str {
        self.device.device_name
    }
    pub fn family(&self) -> &'static str {
        self.device.spi_type.family_name()
    }
    pub fn create_controller(&self) -> Result<AtiSpiController, AtiSpiError> {
        AtiSpiController::new(
            self.device.vendor_id,
            self.device.device_id,
            self.address,
            self.bar,
        )
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
pub fn scan_for_ati_gpus() -> Result<alloc::vec::Vec<DetectedAtiGpu>, AtiSpiError> {
    use rflasher_pci::SysfsPci;
    let pci = SysfsPci::system();
    let mut found = alloc::vec::Vec::new();
    for pci_dev in pci.enumerate()? {
        let Some(device) = find_ati_spi_device(pci_dev.vendor_id, pci_dev.device_id) else {
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

#[cfg(all(feature = "std", target_os = "linux"))]
pub fn find_ati_gpu() -> Result<Option<DetectedAtiGpu>, AtiSpiError> {
    Ok(scan_for_ati_gpus()?.into_iter().next())
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn scan_for_ati_gpus() -> Result<alloc::vec::Vec<DetectedAtiGpu>, AtiSpiError> {
    Err(AtiSpiError::NotSupported(
        "PCI scanning only supported on Linux",
    ))
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub fn find_ati_gpu() -> Result<Option<DetectedAtiGpu>, AtiSpiError> {
    Err(AtiSpiError::NotSupported(
        "PCI scanning only supported on Linux",
    ))
}

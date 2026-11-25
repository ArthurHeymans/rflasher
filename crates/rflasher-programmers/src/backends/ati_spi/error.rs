use thiserror::Error;

#[derive(Debug, Error)]
pub enum AtiSpiError {
    #[error("GPU {vendor_id:04x}:{device_id:04x} ({name}) is not supported")]
    UnsupportedDevice {
        vendor_id: u16,
        device_id: u16,
        name: &'static str,
    },
    #[error("PCI access error: {0}")]
    Pci(#[from] rflasher_pci::PciError),
    #[error("invalid ATI SPI options: {0}")]
    Options(String),
    #[error("{0}")]
    Selection(String),
    #[error("GPU is not available for exclusive access: {0}")]
    Ownership(String),
    #[error("unsupported SPI command: {0}")]
    UnsupportedCommand(&'static str),
    #[error("SPI controller init failed: {0}")]
    SpiInit(&'static str),
    #[error("I/O error: {0}")]
    Io(&'static str),
}

use core::fmt;

#[derive(Debug)]
pub enum AtiSpiError {
    UnsupportedDevice {
        vendor_id: u16,
        device_id: u16,
        name: &'static str,
    },
    Pci(rflasher_pci::PciError),
    SpiInit(&'static str),
    NotSupported(&'static str),
    Io(&'static str),
}

impl From<rflasher_pci::PciError> for AtiSpiError {
    fn from(error: rflasher_pci::PciError) -> Self {
        Self::Pci(error)
    }
}

impl fmt::Display for AtiSpiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedDevice {
                vendor_id,
                device_id,
                name,
            } => write!(
                f,
                "GPU {vendor_id:04x}:{device_id:04x} ({name}) is not supported"
            ),
            Self::Pci(error) => write!(f, "PCI access error: {error}"),
            Self::SpiInit(message) => write!(f, "SPI controller init failed: {message}"),
            Self::NotSupported(message) => write!(f, "not supported: {message}"),
            Self::Io(message) => write!(f, "I/O error: {message}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for AtiSpiError {}

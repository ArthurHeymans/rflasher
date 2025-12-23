//! Error types for the internal programmer

use core::fmt;

/// Error type for the internal programmer
#[derive(Debug)]
pub enum InternalError {
    /// No supported chipset found
    NoChipset,
    /// Chipset is not supported
    UnsupportedChipset {
        vendor_id: u16,
        device_id: u16,
        name: &'static str,
    },
    /// Multiple chipsets found (ambiguous)
    MultipleChipsets,
    /// Failed to access PCI device
    PciAccess(PciAccessError),
    /// Failed to map memory
    MemoryMap { address: u64, size: usize },
    /// Chipset enable failed
    ChipsetEnable(&'static str),
    /// SPI controller initialization failed
    SpiInit(&'static str),
    /// Flash access denied by hardware
    AccessDenied { region: &'static str },
    /// Intel Flash Descriptor (IFD) not found or invalid
    InvalidDescriptor,
    /// Operation not supported by this chipset
    NotSupported(&'static str),
    /// I/O error
    Io(&'static str),
}

/// PCI access error details
#[derive(Debug)]
pub enum PciAccessError {
    /// Failed to initialize PCI access
    Init,
    /// Failed to scan PCI bus
    Scan,
    /// Failed to read PCI config space
    ConfigRead {
        bus: u8,
        device: u8,
        function: u8,
        register: u8,
    },
    /// Failed to write PCI config space
    ConfigWrite {
        bus: u8,
        device: u8,
        function: u8,
        register: u8,
    },
    /// BAR not available or invalid
    InvalidBar(u8),
}

impl fmt::Display for InternalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoChipset => write!(f, "no supported chipset found"),
            Self::UnsupportedChipset {
                vendor_id,
                device_id,
                name,
            } => {
                write!(
                    f,
                    "chipset {:04x}:{:04x} ({}) is not supported",
                    vendor_id, device_id, name
                )
            }
            Self::MultipleChipsets => write!(f, "multiple supported chipsets found"),
            Self::PciAccess(e) => write!(f, "PCI access error: {}", e),
            Self::MemoryMap { address, size } => {
                write!(f, "failed to map memory at {:#x} (size {})", address, size)
            }
            Self::ChipsetEnable(msg) => write!(f, "chipset enable failed: {}", msg),
            Self::SpiInit(msg) => write!(f, "SPI controller init failed: {}", msg),
            Self::AccessDenied { region } => {
                write!(f, "access denied to {} region", region)
            }
            Self::InvalidDescriptor => write!(f, "invalid Intel Flash Descriptor"),
            Self::NotSupported(msg) => write!(f, "not supported: {}", msg),
            Self::Io(msg) => write!(f, "I/O error: {}", msg),
        }
    }
}

impl fmt::Display for PciAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Init => write!(f, "failed to initialize PCI access"),
            Self::Scan => write!(f, "failed to scan PCI bus"),
            Self::ConfigRead {
                bus,
                device,
                function,
                register,
            } => write!(
                f,
                "failed to read PCI config at {:02x}:{:02x}.{:x} reg {:#x}",
                bus, device, function, register
            ),
            Self::ConfigWrite {
                bus,
                device,
                function,
                register,
            } => write!(
                f,
                "failed to write PCI config at {:02x}:{:02x}.{:x} reg {:#x}",
                bus, device, function, register
            ),
            Self::InvalidBar(bar) => write!(f, "BAR{} not available or invalid", bar),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for InternalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PciAccess(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PciAccessError {}

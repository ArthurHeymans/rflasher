//! Error types for the internal programmer

use core::fmt;

use rflasher_core::error::Error as CoreError;

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
    /// Permission was denied while accessing a required system resource.
    ///
    /// The `resource` variant carries context such as the physical address
    /// that could not be mapped, so failures remain diagnosable even though
    /// the OS error code itself is not preserved here.
    PermissionDenied { resource: RestrictedResource },
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

/// A system resource whose access was denied.
#[derive(Debug)]
pub enum RestrictedResource {
    /// Physical memory accessed through `/dev/mem` while mapping `address`.
    ///
    /// With `CONFIG_STRICT_DEVMEM`, `mmap()` fails with `EPERM` for ranges
    /// the kernel considers reserved, so the intended address matters when
    /// interpreting this failure.
    DevMem { address: u64 },
}

impl fmt::Display for RestrictedResource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DevMem { address } => {
                write!(
                    f,
                    "physical memory through /dev/mem (address {:#x})",
                    address
                )
            }
        }
    }
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
        register: u16,
    },
    /// Failed to write PCI config space
    ConfigWrite {
        bus: u8,
        device: u8,
        function: u8,
        register: u16,
    },
    /// Invalid PCI configuration-space address, offset, or width.
    InvalidAccess {
        bus: u8,
        device: u8,
        function: u8,
        register: u16,
    },
    /// BAR not available or invalid
    InvalidBar(u8),
}

impl From<rflasher_pci::PciError> for InternalError {
    fn from(error: rflasher_pci::PciError) -> Self {
        match error {
            rflasher_pci::PciError::NotSupported(message) => Self::NotSupported(message),
            rflasher_pci::PciError::Scan => Self::PciAccess(PciAccessError::Scan),
            rflasher_pci::PciError::ConfigRead { address, offset } => {
                Self::PciAccess(PciAccessError::ConfigRead {
                    bus: address.bus(),
                    device: address.device(),
                    function: address.function(),
                    register: offset,
                })
            }
            rflasher_pci::PciError::ConfigWrite { address, offset } => {
                Self::PciAccess(PciAccessError::ConfigWrite {
                    bus: address.bus(),
                    device: address.device(),
                    function: address.function(),
                    register: offset,
                })
            }
            rflasher_pci::PciError::InvalidAccess { address, offset } => {
                Self::PciAccess(PciAccessError::InvalidAccess {
                    bus: address.bus(),
                    device: address.device(),
                    function: address.function(),
                    register: offset,
                })
            }
        }
    }
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
            Self::PermissionDenied { resource } => {
                write!(f, "permission denied while accessing {resource}")
            }
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
            Self::InvalidAccess {
                bus,
                device,
                function,
                register,
            } => write!(
                f,
                "invalid PCI config access at {:02x}:{:02x}.{:x} reg {:#x}",
                bus, device, function, register
            ),
            Self::InvalidBar(bar) => write!(f, "BAR{} not available or invalid", bar),
        }
    }
}

impl InternalError {
    /// Classifies this internal error as a [`CoreError`].
    ///
    /// Centralizing the mapping keeps every conversion site in sync; new
    /// `InternalError` variants cannot silently diverge between call sites.
    pub fn to_core_error(self) -> CoreError {
        match self {
            Self::NoChipset | Self::UnsupportedChipset { .. } | Self::MultipleChipsets => {
                CoreError::ProgrammerNotReady
            }
            Self::PciAccess(_)
            | Self::PermissionDenied { .. }
            | Self::MemoryMap { .. }
            | Self::ChipsetEnable(_)
            | Self::SpiInit(_)
            | Self::InvalidDescriptor => CoreError::ProgrammerError,
            Self::AccessDenied { .. } => CoreError::RegionProtected,
            Self::Io(_) => CoreError::IoError,
            Self::NotSupported(_) => CoreError::OpcodeNotSupported,
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

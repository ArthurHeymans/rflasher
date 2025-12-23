//! Internal programmer wrapper implementing OpaqueMaster
//!
//! This module provides the high-level programmer interface that wraps
//! the low-level IchSpiController and implements the OpaqueMaster trait.

use crate::chipset::IchChipset;
use crate::error::InternalError;
use crate::ichspi::{IchSpiController, SpiMode};
use crate::DetectedChipset;

use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::OpaqueMaster;

/// Internal programmer for Intel ICH/PCH chipsets
#[cfg(all(feature = "std", target_os = "linux"))]
pub struct InternalProgrammer {
    /// The underlying SPI controller
    controller: IchSpiController,
    /// Flash size detected via hardware sequencing
    flash_size: usize,
    /// Whether BIOS writes are enabled
    writes_enabled: bool,
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl InternalProgrammer {
    /// Create a new internal programmer
    ///
    /// This will:
    /// 1. Detect the Intel chipset
    /// 2. Initialize the SPI controller
    /// 3. Enable BIOS writes if possible
    pub fn new() -> Result<Self, InternalError> {
        Self::with_options(SpiMode::Auto)
    }

    /// Create a new internal programmer with explicit mode
    pub fn with_options(mode: SpiMode) -> Result<Self, InternalError> {
        // Detect chipset
        let chipset = crate::detect_chipset()?
            .ok_or(InternalError::NoChipset)?;

        Self::from_chipset(&chipset, mode)
    }

    /// Create from a specific detected chipset
    pub fn from_chipset(chipset: &DetectedChipset, mode: SpiMode) -> Result<Self, InternalError> {
        let mut controller = IchSpiController::new(chipset, mode)?;

        // Try to enable BIOS writes
        let writes_enabled = match controller.enable_bios_write() {
            Ok(()) => true,
            Err(e) => {
                log::warn!("Could not enable BIOS writes: {}", e);
                false
            }
        };

        // For now, we can't detect flash size without reading the IFD
        // or probing the chip. Set to 0 and let caller determine size.
        let flash_size = 0;

        Ok(Self {
            controller,
            flash_size,
            writes_enabled,
        })
    }

    /// Set the flash size (should be called after probing)
    pub fn set_flash_size(&mut self, size: usize) {
        self.flash_size = size;
    }

    /// Get the chipset generation
    pub fn generation(&self) -> IchChipset {
        self.controller.generation()
    }

    /// Check if the SPI configuration is locked
    pub fn is_locked(&self) -> bool {
        self.controller.is_locked()
    }

    /// Check if writes are enabled
    pub fn writes_enabled(&self) -> bool {
        self.writes_enabled
    }

    /// Check if operating in hardware sequencing mode
    pub fn is_hwseq(&self) -> bool {
        self.controller.mode() == SpiMode::HardwareSequencing
    }

    /// Get the operating mode
    pub fn mode(&self) -> SpiMode {
        self.controller.mode()
    }

    /// Get a reference to the underlying controller
    pub fn controller(&self) -> &IchSpiController {
        &self.controller
    }

    /// Get a mutable reference to the underlying controller
    pub fn controller_mut(&mut self) -> &mut IchSpiController {
        &mut self.controller
    }

    /// Convert an internal error to a core error
    fn map_error(e: InternalError) -> CoreError {
        match e {
            InternalError::NoChipset
            | InternalError::UnsupportedChipset { .. }
            | InternalError::MultipleChipsets => CoreError::ProgrammerNotReady,
            InternalError::PciAccess(_) | InternalError::MemoryMap { .. } => {
                CoreError::ProgrammerError
            }
            InternalError::AccessDenied { .. } => CoreError::RegionProtected,
            InternalError::Io(_) => CoreError::IoError,
            InternalError::ChipsetEnable(_) | InternalError::SpiInit(_) => {
                CoreError::ProgrammerError
            }
            InternalError::InvalidDescriptor => CoreError::ProgrammerError,
            InternalError::NotSupported(_) => CoreError::OpcodeNotSupported,
        }
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl OpaqueMaster for InternalProgrammer {
    fn size(&self) -> usize {
        self.flash_size
    }

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> CoreResult<()> {
        if !self.is_hwseq() {
            // Software sequencing mode - not implemented yet
            return Err(CoreError::OpcodeNotSupported);
        }

        self.controller
            .hwseq_read(addr, buf)
            .map_err(Self::map_error)
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> CoreResult<()> {
        if !self.writes_enabled {
            return Err(CoreError::WriteProtected);
        }

        if !self.is_hwseq() {
            return Err(CoreError::OpcodeNotSupported);
        }

        self.controller
            .hwseq_write(addr, data)
            .map_err(Self::map_error)
    }

    fn erase(&mut self, addr: u32, len: u32) -> CoreResult<()> {
        if !self.writes_enabled {
            return Err(CoreError::WriteProtected);
        }

        if !self.is_hwseq() {
            return Err(CoreError::OpcodeNotSupported);
        }

        self.controller
            .hwseq_erase(addr, len)
            .map_err(Self::map_error)
    }
}

/// Programmer information
pub fn programmer_info() -> rflasher_core::programmer::ProgrammerInfo {
    rflasher_core::programmer::ProgrammerInfo {
        name: "internal",
        description: "Intel ICH/PCH internal flash programmer",
        requires_root: true,
    }
}

// Non-Linux stub
#[cfg(not(all(feature = "std", target_os = "linux")))]
pub struct InternalProgrammer {
    _private: (),
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
impl InternalProgrammer {
    pub fn new() -> Result<Self, InternalError> {
        Err(InternalError::NotSupported(
            "Internal programmer only supported on Linux",
        ))
    }

    pub fn with_options(_mode: SpiMode) -> Result<Self, InternalError> {
        Err(InternalError::NotSupported(
            "Internal programmer only supported on Linux",
        ))
    }
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
impl OpaqueMaster for InternalProgrammer {
    fn size(&self) -> usize {
        0
    }

    fn read(&mut self, _addr: u32, _buf: &mut [u8]) -> CoreResult<()> {
        Err(CoreError::ProgrammerNotReady)
    }

    fn write(&mut self, _addr: u32, _data: &[u8]) -> CoreResult<()> {
        Err(CoreError::ProgrammerNotReady)
    }

    fn erase(&mut self, _addr: u32, _len: u32) -> CoreResult<()> {
        Err(CoreError::ProgrammerNotReady)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_programmer_info() {
        let info = programmer_info();
        assert_eq!(info.name, "internal");
        assert!(info.requires_root);
    }
}

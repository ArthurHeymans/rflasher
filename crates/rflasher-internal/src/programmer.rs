//! Internal programmer wrapper implementing SpiMaster and OpaqueMaster
//!
//! This module provides the high-level programmer interface that wraps
//! the low-level controllers (Intel ICH/PCH or AMD SPI100) and implements
//! the appropriate trait (SpiMaster or OpaqueMaster).

use crate::amd_enable::enable_amd_spi100;
use crate::controller::Controller;
use crate::error::InternalError;
use crate::ichspi::{IchSpiController, SpiMode};
use crate::{AnyDetectedChipset, DetectedAmdChipset, DetectedChipset};

use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{OpaqueMaster, SpiFeatures, SpiMaster};
use rflasher_core::spi::SpiCommand;

/// Options for the internal programmer
#[derive(Debug, Clone, Default)]
pub struct InternalOptions {
    /// SPI sequencing mode (auto, hwseq, swseq)
    pub mode: SpiMode,
}

impl InternalOptions {
    /// Create options with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the SPI mode
    pub fn with_mode(mut self, mode: SpiMode) -> Self {
        self.mode = mode;
        self
    }

    /// Parse options from key-value pairs (from CLI)
    ///
    /// Supported options:
    /// - ich_spi_mode=auto|hwseq|swseq
    pub fn from_options(options: &[(&str, &str)]) -> Result<Self, InternalError> {
        let mut opts = Self::default();

        for (key, value) in options {
            match *key {
                "ich_spi_mode" | "mode" => {
                    opts.mode = SpiMode::parse(value).ok_or(InternalError::NotSupported(
                        "Invalid ich_spi_mode value (use: auto, hwseq, or swseq)",
                    ))?;
                }
                _ => {
                    log::warn!("Unknown internal programmer option: {}={}", key, value);
                }
            }
        }

        Ok(opts)
    }
}

/// Internal programmer for Intel ICH/PCH and AMD SPI100 chipsets
#[cfg(all(feature = "std", target_os = "linux"))]
pub struct InternalProgrammer {
    /// The underlying SPI controller (Intel or AMD)
    controller: Box<dyn Controller>,
    /// Flash size detected via hardware sequencing
    flash_size: usize,
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl InternalProgrammer {
    /// Create a new internal programmer with default options
    ///
    /// This will:
    /// 1. Detect the Intel chipset
    /// 2. Initialize the SPI controller
    /// 3. Enable BIOS writes if possible
    pub fn new() -> Result<Self, InternalError> {
        Self::with_options(InternalOptions::default())
    }

    /// Create a new internal programmer with explicit options
    pub fn with_options(options: InternalOptions) -> Result<Self, InternalError> {
        // Detect chipset
        let chipset = crate::detect_chipset()?.ok_or(InternalError::NoChipset)?;

        match chipset {
            AnyDetectedChipset::Intel(intel_chipset) => {
                Self::from_intel_chipset(&intel_chipset, options)
            }
            AnyDetectedChipset::Amd(amd_chipset) => Self::from_amd_chipset(&amd_chipset, options),
        }
    }

    /// Create from a specific detected Intel chipset
    pub fn from_intel_chipset(
        chipset: &DetectedChipset,
        options: InternalOptions,
    ) -> Result<Self, InternalError> {
        let mut controller = IchSpiController::new(chipset, options.mode)?;

        // Try to enable BIOS writes
        if let Err(e) = controller.enable_bios_write() {
            log::warn!("Could not enable BIOS writes: {}", e);
        }

        // For now, we can't detect flash size without reading the IFD
        // or probing the chip. Set to 0 and let caller determine size.
        let flash_size = 0;

        Ok(Self {
            controller: Box::new(controller),
            flash_size,
        })
    }

    /// Create from a specific detected AMD chipset
    pub fn from_amd_chipset(
        chipset: &DetectedAmdChipset,
        _options: InternalOptions,
    ) -> Result<Self, InternalError> {
        // Enable the AMD SPI100 controller
        let info = enable_amd_spi100(
            chipset.enable,
            chipset.bus,
            chipset.device,
            chipset.revision_id,
        )?;

        // Create the controller
        let controller = info.create_controller()?;

        // Flash size will be determined later by probing
        let flash_size = 0;

        Ok(Self {
            controller: Box::new(controller),
            flash_size,
        })
    }

    /// Old API for compatibility - only works with Intel chipsets
    #[deprecated(
        since = "0.1.0",
        note = "Use from_intel_chipset or from_amd_chipset instead"
    )]
    pub fn from_chipset(
        chipset: &DetectedChipset,
        options: InternalOptions,
    ) -> Result<Self, InternalError> {
        Self::from_intel_chipset(chipset, options)
    }

    /// Set the flash size (should be called after probing)
    pub fn set_flash_size(&mut self, size: usize) {
        self.flash_size = size;
    }

    /// Get the operating mode (Intel only)
    ///
    /// Returns SoftwareSequencing for AMD controllers
    pub fn mode(&self) -> SpiMode {
        self.intel_controller()
            .map(|c| c.mode())
            .unwrap_or(SpiMode::SoftwareSequencing)
    }

    /// Check if writes are enabled (internal helper)
    fn writes_enabled(&self) -> bool {
        self.controller.writes_enabled()
    }

    /// Check if this is an Intel controller (internal helper)
    fn is_intel(&self) -> bool {
        self.controller.controller_name() == "Intel ICH/PCH"
    }

    /// Get a reference to the underlying Intel controller (internal helper)
    ///
    /// Returns None for AMD controllers.
    fn intel_controller(&self) -> Option<&IchSpiController> {
        // SAFETY: We use controller_name to determine the type
        if self.is_intel() {
            // Downcast the trait object to the concrete type
            let controller_ptr = &*self.controller as *const dyn Controller;
            let intel_ptr = controller_ptr as *const IchSpiController;
            unsafe { Some(&*intel_ptr) }
        } else {
            None
        }
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl OpaqueMaster for InternalProgrammer {
    fn size(&self) -> usize {
        self.flash_size
    }

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> CoreResult<()> {
        self.controller.controller_read(addr, buf, self.flash_size)
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> CoreResult<()> {
        if !self.writes_enabled() {
            return Err(CoreError::WriteProtected);
        }

        self.controller.controller_write(addr, data)
    }

    fn erase(&mut self, addr: u32, len: u32) -> CoreResult<()> {
        if !self.writes_enabled() {
            return Err(CoreError::WriteProtected);
        }

        self.controller.controller_erase(addr, len)
    }
}

/// SpiMaster implementation for raw SPI command execution
///
/// This is only available in software sequencing mode. Hardware sequencing
/// mode does not allow arbitrary SPI commands - use OpaqueMaster instead.
///
/// # Limitations
///
/// - Only opcodes in the OPMENU table can be executed
/// - Maximum 64 bytes per transfer
/// - Only single I/O mode (no dual/quad)
/// - No 4-byte addressing support (24-bit address max)
/// - Dummy cycles are not supported by the Intel controller
#[cfg(all(feature = "std", target_os = "linux"))]
impl SpiMaster for InternalProgrammer {
    fn features(&self) -> SpiFeatures {
        self.controller.features()
    }

    fn max_read_len(&self) -> usize {
        self.controller.max_read_len()
    }

    fn max_write_len(&self) -> usize {
        self.controller.max_write_len()
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        self.controller.execute(cmd)
    }

    fn probe_opcode(&self, opcode: u8) -> bool {
        self.controller.probe_opcode(opcode)
    }

    fn delay_us(&mut self, us: u32) {
        std::thread::sleep(std::time::Duration::from_micros(us as u64));
    }
}

/// Programmer information
pub fn programmer_info() -> rflasher_core::programmer::ProgrammerInfo {
    rflasher_core::programmer::ProgrammerInfo {
        name: "internal",
        description: "Intel ICH/PCH and AMD SPI100 internal flash programmer",
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

    pub fn with_options(_options: InternalOptions) -> Result<Self, InternalError> {
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

#[cfg(not(all(feature = "std", target_os = "linux")))]
impl SpiMaster for InternalProgrammer {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::empty()
    }

    fn max_read_len(&self) -> usize {
        0
    }

    fn max_write_len(&self) -> usize {
        0
    }

    fn execute(&mut self, _cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        Err(CoreError::ProgrammerNotReady)
    }

    fn probe_opcode(&self, _opcode: u8) -> bool {
        false
    }

    fn delay_us(&mut self, _us: u32) {}
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

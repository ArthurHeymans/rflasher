//! Internal programmer wrapper implementing SpiMaster and OpaqueMaster
//!
//! This module provides the high-level programmer interface that wraps
//! the low-level controllers (Intel ICH/PCH or AMD SPI100) and implements
//! the appropriate trait (SpiMaster or OpaqueMaster).

use crate::amd_enable::enable_amd_spi100;
use crate::amd_spi100::Spi100Controller;
use crate::chipset::IchChipset;
use crate::error::InternalError;
use crate::ichspi::{IchSpiController, SpiMode};
use crate::{AnyDetectedChipset, DetectedAmdChipset, DetectedChipset};

use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{OpaqueMaster, SpiFeatures, SpiMaster};
use rflasher_core::spi::{AddressWidth, SpiCommand};

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

/// Controller type (Intel or AMD)
#[cfg(all(feature = "std", target_os = "linux"))]
enum Controller {
    Intel(IchSpiController),
    Amd(Spi100Controller),
}

/// Internal programmer for Intel ICH/PCH and AMD SPI100 chipsets
#[cfg(all(feature = "std", target_os = "linux"))]
pub struct InternalProgrammer {
    /// The underlying SPI controller
    controller: Controller,
    /// Flash size detected via hardware sequencing
    flash_size: usize,
    /// Whether BIOS writes are enabled
    writes_enabled: bool,
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
            controller: Controller::Intel(controller),
            flash_size,
            writes_enabled,
        })
    }

    /// Create from a specific detected AMD chipset
    pub fn from_amd_chipset(
        chipset: &DetectedAmdChipset,
        _options: InternalOptions,
    ) -> Result<Self, InternalError> {
        // Enable the AMD SPI100 controller
        let info = enable_amd_spi100(chipset.enable, chipset.bus, chipset.device, chipset.revision_id)?;

        // Create the controller
        let controller = info.create_controller()?;

        // AMD doesn't need BIOS_CNTL write enable - SPI100 is always writable
        let writes_enabled = true;

        // Flash size will be determined later by probing
        let flash_size = 0;

        Ok(Self {
            controller: Controller::Amd(controller),
            flash_size,
            writes_enabled,
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

    /// Get the chipset generation (Intel only)
    pub fn generation(&self) -> Option<IchChipset> {
        match &self.controller {
            Controller::Intel(c) => Some(c.generation()),
            Controller::Amd(_) => None,
        }
    }

    /// Check if the SPI configuration is locked
    pub fn is_locked(&self) -> bool {
        match &self.controller {
            Controller::Intel(c) => c.is_locked(),
            Controller::Amd(_) => false, // AMD SPI100 doesn't have a lock bit
        }
    }

    /// Check if writes are enabled
    pub fn writes_enabled(&self) -> bool {
        self.writes_enabled
    }

    /// Check if operating in hardware sequencing mode (Intel only)
    pub fn is_hwseq(&self) -> bool {
        match &self.controller {
            Controller::Intel(c) => c.mode() == SpiMode::HardwareSequencing,
            Controller::Amd(_) => false, // AMD always uses software sequencing
        }
    }

    /// Get the operating mode (Intel only)
    pub fn mode(&self) -> SpiMode {
        match &self.controller {
            Controller::Intel(c) => c.mode(),
            Controller::Amd(_) => SpiMode::SoftwareSequencing,
        }
    }

    /// Check if this is an Intel controller
    pub fn is_intel(&self) -> bool {
        matches!(&self.controller, Controller::Intel(_))
    }

    /// Check if this is an AMD controller
    pub fn is_amd(&self) -> bool {
        matches!(&self.controller, Controller::Amd(_))
    }

    /// Get a reference to the underlying Intel controller
    pub fn intel_controller(&self) -> Option<&IchSpiController> {
        match &self.controller {
            Controller::Intel(c) => Some(c),
            Controller::Amd(_) => None,
        }
    }

    /// Get a mutable reference to the underlying Intel controller
    pub fn intel_controller_mut(&mut self) -> Option<&mut IchSpiController> {
        match &mut self.controller {
            Controller::Intel(c) => Some(c),
            Controller::Amd(_) => None,
        }
    }

    /// Get a reference to the underlying AMD controller
    pub fn amd_controller(&self) -> Option<&Spi100Controller> {
        match &self.controller {
            Controller::Intel(_) => None,
            Controller::Amd(c) => Some(c),
        }
    }

    /// Get a mutable reference to the underlying AMD controller
    pub fn amd_controller_mut(&mut self) -> Option<&mut Spi100Controller> {
        match &mut self.controller {
            Controller::Intel(_) => None,
            Controller::Amd(c) => Some(c),
        }
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
        match &mut self.controller {
            Controller::Intel(c) => {
                let is_hwseq = c.mode() == SpiMode::HardwareSequencing;
                if is_hwseq {
                    c.hwseq_read(addr, buf).map_err(Self::map_error)
                } else {
                    c.swseq_read(addr, buf).map_err(Self::map_error)
                }
            }
            Controller::Amd(c) => {
                // AMD read needs chip_size parameter, use flash_size if set, otherwise use a safe default
                let chip_size = if self.flash_size > 0 {
                    self.flash_size as u64
                } else {
                    16 * 1024 * 1024 // Default to 16MB if not yet probed
                };
                c.read(chip_size, addr, buf).map_err(Self::map_error)
            }
        }
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> CoreResult<()> {
        if !self.writes_enabled {
            return Err(CoreError::WriteProtected);
        }

        match &mut self.controller {
            Controller::Intel(c) => {
                let is_hwseq = c.mode() == SpiMode::HardwareSequencing;
                if is_hwseq {
                    c.hwseq_write(addr, data).map_err(Self::map_error)
                } else {
                    c.swseq_write(addr, data).map_err(Self::map_error)
                }
            }
            Controller::Amd(_) => {
                // AMD SPI100 uses SpiMaster for write operations, not OpaqueMaster
                // Caller should use SpiMaster::execute() with appropriate SPI commands
                log::warn!("OpaqueMaster::write() not supported for AMD SPI100 - use SpiMaster instead");
                Err(CoreError::OpcodeNotSupported)
            }
        }
    }

    fn erase(&mut self, addr: u32, len: u32) -> CoreResult<()> {
        if !self.writes_enabled {
            return Err(CoreError::WriteProtected);
        }

        match &mut self.controller {
            Controller::Intel(c) => {
                let is_hwseq = c.mode() == SpiMode::HardwareSequencing;
                if is_hwseq {
                    c.hwseq_erase(addr, len).map_err(Self::map_error)
                } else {
                    c.swseq_erase(addr, len).map_err(Self::map_error)
                }
            }
            Controller::Amd(_) => {
                // AMD SPI100 uses SpiMaster for erase operations, not OpaqueMaster
                // Caller should use SpiMaster::execute() with appropriate SPI commands
                log::warn!("OpaqueMaster::erase() not supported for AMD SPI100 - use SpiMaster instead");
                Err(CoreError::OpcodeNotSupported)
            }
        }
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
        match &self.controller {
            Controller::Intel(_) => {
                // Intel ICH/PCH SPI controller only supports single I/O mode
                // No 4-byte addressing, dual, or quad support
                SpiFeatures::empty()
            }
            Controller::Amd(c) => c.features(),
        }
    }

    fn max_read_len(&self) -> usize {
        match &self.controller {
            Controller::Intel(_) => IchSpiController::SWSEQ_MAX_DATA,
            Controller::Amd(c) => c.max_read_len(),
        }
    }

    fn max_write_len(&self) -> usize {
        match &self.controller {
            Controller::Intel(_) => IchSpiController::SWSEQ_MAX_DATA,
            Controller::Amd(c) => c.max_write_len(),
        }
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        match &mut self.controller {
            Controller::Intel(c) => {
                // SpiMaster requires software sequencing mode
                let is_hwseq = c.mode() == SpiMode::HardwareSequencing;
                if is_hwseq {
                    log::warn!(
                        "SpiMaster::execute() called in hwseq mode - this mode doesn't support raw SPI commands"
                    );
                    return Err(CoreError::OpcodeNotSupported);
                }

                // WREN (0x06) and EWSR (0x50) are preops on Intel, not regular opcodes.
                // They're handled automatically via atomic mode when a write/erase command
                // is executed. The higher-level protocol sends these separately, so we
                // silently succeed here - the actual WREN will be sent atomically with
                // the next write/erase command.
                if cmd.opcode == 0x06 || cmd.opcode == 0x50 {
                    log::trace!(
                        "Ignoring preop {:#04x} - will be sent atomically with next command",
                        cmd.opcode
                    );
                    return Ok(());
                }

                // Intel controller doesn't support dummy cycles in swseq
                if cmd.dummy_cycles > 0 {
                    log::debug!(
                        "Dummy cycles ({}) not supported by Intel swseq",
                        cmd.dummy_cycles
                    );
                    return Err(CoreError::OpcodeNotSupported);
                }

                // Only single I/O mode supported
                if cmd.io_mode != rflasher_core::spi::IoMode::Single {
                    log::debug!("Only single I/O mode supported by Intel swseq");
                    return Err(CoreError::OpcodeNotSupported);
                }

                // Build the write array for swseq_send_command
                // Format: opcode + [address bytes] + [write data]
                let mut writearr = [0u8; 68]; // 1 opcode + 3 addr + 64 data max
                let mut write_len = 0;

                // Opcode is always first
                writearr[0] = cmd.opcode;
                write_len += 1;

                // Add address if present
                if let Some(addr) = cmd.address {
                    match cmd.address_width {
                        AddressWidth::ThreeByte => {
                            writearr[1] = ((addr >> 16) & 0xff) as u8;
                            writearr[2] = ((addr >> 8) & 0xff) as u8;
                            writearr[3] = (addr & 0xff) as u8;
                            write_len += 3;
                        }
                        AddressWidth::FourByte => {
                            // Intel swseq doesn't support 4-byte addresses
                            log::debug!("4-byte addressing not supported by Intel swseq");
                            return Err(CoreError::OpcodeNotSupported);
                        }
                        AddressWidth::None => {
                            // Address provided but width is None - shouldn't happen but handle it
                        }
                    }
                }

                // Add write data if present
                if !cmd.write_data.is_empty() {
                    let data_len = cmd.write_data.len();
                    if write_len + data_len > 68 {
                        log::debug!("Write data too long for Intel swseq");
                        return Err(CoreError::IoError);
                    }
                    writearr[write_len..write_len + data_len].copy_from_slice(cmd.write_data);
                    write_len += data_len;
                }

                // Execute the command
                c.swseq_send_command(&writearr[..write_len], cmd.read_buf)
                    .map_err(Self::map_error)
            }
            Controller::Amd(c) => {
                // Forward directly to AMD controller
                // AMD controller's execute returns CoreError, not InternalError
                c.execute(cmd)
            }
        }
    }

    fn probe_opcode(&self, opcode: u8) -> bool {
        match &self.controller {
            Controller::Intel(c) => {
                // In hwseq mode, no raw opcodes are available
                if self.is_hwseq() {
                    return false;
                }

                // Check if the opcode is in the OPMENU table
                c.has_opcode(opcode)
            }
            Controller::Amd(c) => c.probe_opcode(opcode),
        }
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

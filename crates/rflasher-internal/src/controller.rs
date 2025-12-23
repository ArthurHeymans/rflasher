//! Controller trait for internal SPI controllers
//!
//! This module defines a unified trait that both Intel and AMD controllers implement,
//! allowing the InternalProgrammer to work with either controller type without
//! awkward enum matching.

use crate::error::InternalError;
use rflasher_core::error::Result as CoreResult;
use rflasher_core::programmer::SpiFeatures;
use rflasher_core::spi::SpiCommand;

/// Unified trait for SPI controllers (Intel ICH/PCH or AMD SPI100)
pub trait Controller {
    /// Check if this controller is locked (Intel-specific, always false for AMD)
    fn is_locked(&self) -> bool;

    /// Check if BIOS writes are enabled
    fn writes_enabled(&self) -> bool;

    /// Enable BIOS writes if possible
    fn enable_bios_write(&mut self) -> Result<(), InternalError>;

    /// Read from flash using controller-specific method
    ///
    /// For Intel: uses hwseq or swseq depending on mode
    /// For AMD: uses memory-mapped reads when possible
    fn controller_read(&mut self, addr: u32, buf: &mut [u8], chip_size: usize) -> CoreResult<()>;

    /// Write to flash using controller-specific method
    ///
    /// For Intel: uses hwseq or swseq depending on mode
    /// For AMD: delegates to SpiMaster
    fn controller_write(&mut self, addr: u32, data: &[u8]) -> CoreResult<()>;

    /// Erase flash using controller-specific method
    ///
    /// For Intel: uses hwseq or swseq depending on mode
    /// For AMD: delegates to SpiMaster
    fn controller_erase(&mut self, addr: u32, len: u32) -> CoreResult<()>;

    /// Get a human-readable name for this controller type
    fn controller_name(&self) -> &'static str;

    // SpiMaster-like methods that we need for InternalProgrammer

    /// Get the SPI features supported by this controller
    fn features(&self) -> SpiFeatures;

    /// Get the maximum read length
    fn max_read_len(&self) -> usize;

    /// Get the maximum write length
    fn max_write_len(&self) -> usize;

    /// Execute a raw SPI command
    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()>;

    /// Check if an opcode is available
    fn probe_opcode(&self, opcode: u8) -> bool;
}

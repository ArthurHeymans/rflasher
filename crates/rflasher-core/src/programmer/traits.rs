//! Programmer trait definitions

use crate::error::Result;
use crate::spi::SpiCommand;
use bitflags::bitflags;

bitflags! {
    /// SPI master feature flags
    ///
    /// These flags indicate what capabilities a programmer supports.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SpiFeatures: u32 {
        /// Supports 4-byte addressing commands
        const FOUR_BYTE_ADDR = 1 << 0;
        /// Supports dual input mode (1-1-2)
        const DUAL_INPUT     = 1 << 1;
        /// Supports dual output mode (1-2-2)
        const DUAL_OUTPUT    = 1 << 2;
        /// Supports quad input mode (1-1-4)
        const QUAD_INPUT     = 1 << 3;
        /// Supports quad output mode (1-4-4)
        const QUAD_OUTPUT    = 1 << 4;
        /// Supports QPI mode (4-4-4)
        const QPI            = 1 << 5;
    }
}

impl Default for SpiFeatures {
    fn default() -> Self {
        SpiFeatures::empty()
    }
}

/// Synchronous SPI Master trait (blocking, no_std compatible)
///
/// This trait represents a programmer that can execute SPI commands.
/// Implementations should be blocking and suitable for environments
/// without an async runtime.
pub trait SpiMaster {
    /// Get the features supported by this programmer
    fn features(&self) -> SpiFeatures;

    /// Get the maximum number of bytes that can be read in a single transaction
    fn max_read_len(&self) -> usize;

    /// Get the maximum number of bytes that can be written in a single transaction
    fn max_write_len(&self) -> usize;

    /// Execute a single SPI command
    ///
    /// The command contains all the information needed for the transaction:
    /// opcode, address, dummy cycles, and data buffers.
    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()>;

    /// Check if an opcode is supported by this programmer
    ///
    /// Some programmers (like Intel internal) have restrictions on which
    /// opcodes can be executed. Returns true if the opcode is allowed.
    fn probe_opcode(&self, opcode: u8) -> bool {
        let _ = opcode;
        true
    }

    /// Delay for the specified number of microseconds
    fn delay_us(&mut self, us: u32);
}

/// Async SPI Master trait (no_std compatible with Embassy)
///
/// This trait is the async version of SpiMaster, suitable for use
/// with async runtimes like tokio (std) or Embassy (no_std).
pub trait AsyncSpiMaster {
    /// Get the features supported by this programmer
    fn features(&self) -> SpiFeatures;

    /// Get the maximum number of bytes that can be read in a single transaction
    fn max_read_len(&self) -> usize;

    /// Get the maximum number of bytes that can be written in a single transaction
    fn max_write_len(&self) -> usize;

    /// Execute a single SPI command
    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> impl core::future::Future<Output = Result<()>>;

    /// Check if an opcode is supported by this programmer
    fn probe_opcode(&self, opcode: u8) -> bool {
        let _ = opcode;
        true
    }

    /// Delay for the specified number of microseconds
    fn delay_us(&mut self, us: u32) -> impl core::future::Future<Output = ()>;
}

/// Opaque master trait for programmers with restricted access
///
/// Some programmers (like Intel internal flash controller) don't expose
/// raw SPI access. Instead, they provide higher-level read/write/erase
/// operations that handle the protocol internally.
pub trait OpaqueMaster {
    /// Get the total flash size in bytes
    fn size(&self) -> usize;

    /// Read flash contents into the provided buffer
    ///
    /// # Arguments
    /// * `addr` - Starting address to read from
    /// * `buf` - Buffer to read into
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()>;

    /// Write data to flash (assumes the region is already erased)
    ///
    /// # Arguments
    /// * `addr` - Starting address to write to
    /// * `data` - Data to write
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<()>;

    /// Erase a region of flash
    ///
    /// # Arguments
    /// * `addr` - Starting address to erase
    /// * `len` - Number of bytes to erase
    fn erase(&mut self, addr: u32, len: u32) -> Result<()>;
}

/// Async opaque master trait
pub trait AsyncOpaqueMaster {
    /// Get the total flash size in bytes
    fn size(&self) -> usize;

    /// Read flash contents into the provided buffer
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> impl core::future::Future<Output = Result<()>>;

    /// Write data to flash (assumes the region is already erased)
    fn write(&mut self, addr: u32, data: &[u8]) -> impl core::future::Future<Output = Result<()>>;

    /// Erase a region of flash
    fn erase(&mut self, addr: u32, len: u32) -> impl core::future::Future<Output = Result<()>>;
}

/// Information about a programmer
#[derive(Debug, Clone)]
pub struct ProgrammerInfo {
    /// Name of the programmer
    pub name: &'static str,
    /// Description
    pub description: &'static str,
    /// Whether this programmer requires elevated privileges
    pub requires_root: bool,
}

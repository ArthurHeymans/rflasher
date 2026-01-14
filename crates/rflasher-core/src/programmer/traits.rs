//! Programmer trait definitions
//!
//! These traits use `maybe_async` to support both sync and async modes.
//! - By default, traits are async (suitable for WASM/web, Embassy, tokio)
//! - With the `is_sync` feature, traits become synchronous

use crate::error::Result;
use crate::spi::SpiCommand;
use bitflags::bitflags;
use maybe_async::maybe_async;

bitflags! {
    /// SPI master feature flags
    ///
    /// These flags indicate what capabilities a programmer supports.
    /// Naming follows the convention from flashprog for compatibility.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct SpiFeatures: u32 {
        /// Supports 4-byte addressing commands
        const FOUR_BYTE_ADDR = 1 << 0;
        /// Supports no 4BA modes (compatibility modes don't work)
        const NO_4BA_MODES   = 1 << 1;
        /// Can read two bits at once (1-1-2 mode)
        const DUAL_IN        = 1 << 2;
        /// Can transfer two bits at once (1-2-2 mode)
        const DUAL_IO        = 1 << 3;
        /// Can read four bits at once (1-1-4 mode)
        const QUAD_IN        = 1 << 4;
        /// Can transfer four bits at once (1-4-4 mode)
        const QUAD_IO        = 1 << 5;
        /// Can send commands with quad I/O (4-4-4 mode)
        const QPI            = 1 << 6;

        /// Shorthand for dual mode (both DUAL_IN and DUAL_IO)
        const DUAL = Self::DUAL_IN.bits() | Self::DUAL_IO.bits();
        /// Shorthand for quad mode (both QUAD_IN and QUAD_IO)
        const QUAD = Self::QUAD_IN.bits() | Self::QUAD_IO.bits();
    }
}

impl Default for SpiFeatures {
    fn default() -> Self {
        SpiFeatures::empty()
    }
}

/// SPI Master trait (sync or async depending on `is_sync` feature)
///
/// This trait represents a programmer that can execute SPI commands.
/// - With `is_sync` feature: blocking/synchronous
/// - Without `is_sync` feature: async (for WASM, Embassy, tokio)
///
/// ## Multi-I/O Support
///
/// The `SpiCommand` struct contains an `io_mode` field specifying the desired
/// I/O mode (Single, DualOut, DualIo, QuadOut, QuadIo, or Qpi). Implementations
/// should:
///
/// 1. Report supported modes via `features()` using flags like `DUAL_IN`,
///    `DUAL_IO`, `QUAD_IN`, `QUAD_IO`, and `QPI`
/// 2. Handle the `io_mode` in `execute()`:
///    - **Hardware-accelerated programmers** (e.g., FT4222, CH347): Use the
///      hardware's native multi-IO support
///    - **Bitbang programmers** (e.g., linux_gpio): Use the helper traits from
///      the `bitbang` module
/// 3. Fall back to single I/O mode if a requested mode isn't supported
///
/// ## Example: Hardware-accelerated programmer
///
/// ```ignore
/// #[maybe_async]
/// impl SpiMaster for FT4222 {
///     fn features(&self) -> SpiFeatures {
///         SpiFeatures::FOUR_BYTE_ADDR | SpiFeatures::DUAL | SpiFeatures::QUAD
///     }
///
///     async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
///         match cmd.io_mode {
///             IoMode::Single => self.send_single_io(cmd).await,
///             IoMode::DualOut | IoMode::DualIo => self.send_multi_io(cmd, 2).await,
///             IoMode::QuadOut | IoMode::QuadIo | IoMode::Qpi => self.send_multi_io(cmd, 4).await,
///         }
///     }
/// }
/// ```
#[maybe_async(AFIT)]
pub trait SpiMaster {
    /// Get the features supported by this programmer
    ///
    /// This should include multi-I/O capabilities like `DUAL_IN`, `DUAL_IO`,
    /// `QUAD_IN`, `QUAD_IO`, and `QPI` if supported.
    fn features(&self) -> SpiFeatures;

    /// Get the maximum number of bytes that can be read in a single transaction
    fn max_read_len(&self) -> usize;

    /// Get the maximum number of bytes that can be written in a single transaction
    fn max_write_len(&self) -> usize;

    /// Execute a single SPI command
    ///
    /// The command contains all the information needed for the transaction:
    /// - `opcode`: The SPI command opcode
    /// - `address`: Optional address (with width)
    /// - `io_mode`: The I/O mode to use (Single, DualOut, DualIo, QuadOut, QuadIo, Qpi)
    /// - `dummy_cycles`: Number of dummy clock cycles after address
    /// - `write_data`: Data to write after the header
    /// - `read_buf`: Buffer to read data into
    ///
    /// The I/O mode specifies how data is transferred:
    /// - `Single` (1-1-1): All phases use single I/O
    /// - `DualOut` (1-1-2): Opcode and address single, data dual (read only)
    /// - `DualIo` (1-2-2): Opcode single, address and data dual
    /// - `QuadOut` (1-1-4): Opcode and address single, data quad (read only)
    /// - `QuadIo` (1-4-4): Opcode single, address and data quad
    /// - `Qpi` (4-4-4): All phases use quad I/O
    ///
    /// If the requested mode isn't supported, implementations should fall back
    /// to single I/O mode and optionally log a warning.
    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()>;

    /// Check if an opcode is supported by this programmer
    ///
    /// Some programmers (like Intel internal) have restrictions on which
    /// opcodes can be executed. Returns true if the opcode is allowed.
    fn probe_opcode(&self, opcode: u8) -> bool {
        let _ = opcode;
        true
    }

    /// Delay for the specified number of microseconds
    async fn delay_us(&mut self, us: u32);
}

/// Opaque master trait for programmers with restricted access
///
/// Some programmers (like Intel internal flash controller) don't expose
/// raw SPI access. Instead, they provide higher-level read/write/erase
/// operations that handle the protocol internally.
#[maybe_async(AFIT)]
pub trait OpaqueMaster {
    /// Get the total flash size in bytes
    fn size(&self) -> usize;

    /// Read flash contents into the provided buffer
    ///
    /// # Arguments
    /// * `addr` - Starting address to read from
    /// * `buf` - Buffer to read into
    async fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()>;

    /// Write data to flash (assumes the region is already erased)
    ///
    /// # Arguments
    /// * `addr` - Starting address to write to
    /// * `data` - Data to write
    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<()>;

    /// Erase a region of flash
    ///
    /// # Arguments
    /// * `addr` - Starting address to erase
    /// * `len` - Number of bytes to erase
    async fn erase(&mut self, addr: u32, len: u32) -> Result<()>;
}

// Blanket impl for boxed SPI masters to allow trait objects (sync mode only)
// In async mode, traits with async fn are not object-safe
#[cfg(all(feature = "alloc", feature = "is_sync"))]
impl SpiMaster for alloc::boxed::Box<dyn SpiMaster + Send> {
    fn features(&self) -> SpiFeatures {
        (**self).features()
    }

    fn max_read_len(&self) -> usize {
        (**self).max_read_len()
    }

    fn max_write_len(&self) -> usize {
        (**self).max_write_len()
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
        (**self).execute(cmd)
    }

    fn probe_opcode(&self, opcode: u8) -> bool {
        (**self).probe_opcode(opcode)
    }

    fn delay_us(&mut self, us: u32) {
        (**self).delay_us(us)
    }
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

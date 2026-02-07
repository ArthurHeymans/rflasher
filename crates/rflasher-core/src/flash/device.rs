//! Unified flash device trait
//!
//! This module provides the `FlashDevice` trait that abstracts over both
//! SPI-based flash chips (via `SpiMaster` + `FlashContext`) and opaque
//! programmers (via `OpaqueMaster`).
//!
//! Uses `maybe_async` to support both sync and async modes.

use crate::chip::{EraseBlock, WriteGranularity};
use crate::error::Result;
#[cfg(feature = "alloc")]
use crate::wp::{WpConfig, WpError, WpMode, WpRange, WpResult, WriteOptions};
use maybe_async::maybe_async;

/// Unified trait for flash devices
///
/// This trait abstracts over both SPI-based flash chips (where we have raw SPI
/// access and chip metadata from JEDEC probing) and opaque programmers (where
/// the hardware handles the protocol and we only have address-based access).
///
/// # Operations
///
/// All operations use 32-bit addresses, which supports flash sizes up to 4GB.
///
/// # Write Protection
///
/// Write protection operations are optional. The default implementations return
/// `WpError::ChipUnsupported`. SPI-based devices override these to provide
/// actual WP functionality.
///
/// # Example
///
/// ```ignore
/// use rflasher_core::flash::FlashDevice;
///
/// fn read_first_sector<D: FlashDevice>(device: &mut D) -> Result<Vec<u8>> {
///     let mut buf = vec![0u8; 4096];
///     device.read(0, &mut buf)?;
///     Ok(buf)
/// }
/// ```
#[maybe_async(AFIT)]
pub trait FlashDevice {
    /// Get the total flash size in bytes
    fn size(&self) -> u32;

    /// Get the minimum erase block size in bytes
    ///
    /// This is the smallest unit that can be erased. All erase operations
    /// must be aligned to this size and be a multiple of this size.
    fn erase_granularity(&self) -> u32;

    /// Get the write granularity for smart write decisions
    ///
    /// This determines how the smart write algorithm decides whether an
    /// erase is needed before writing:
    /// - `Bit`: Can change individual bits from 1 to 0
    /// - `Byte`: Can write individual bytes (if source is erased)
    /// - `Page`: Must write full pages (if source is erased)
    fn write_granularity(&self) -> WriteGranularity;

    /// Get available erase block sizes
    ///
    /// Returns the erase blocks available for this device, from smallest
    /// to largest. Opaque programmers typically only have one erase size,
    /// while SPI chips may have multiple (4KB, 32KB, 64KB, chip erase).
    fn erase_blocks(&self) -> &[EraseBlock];

    /// Read flash contents into the provided buffer
    ///
    /// # Arguments
    /// * `addr` - Starting address to read from
    /// * `buf` - Buffer to read into
    ///
    /// # Errors
    /// * `AddressOutOfBounds` - If the read extends beyond flash size
    /// * `ReadError` - If the read operation fails
    async fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()>;

    /// Write data to flash
    ///
    /// The target region should be erased first (all bytes 0xFF).
    /// This function handles page alignment internally.
    ///
    /// # Arguments
    /// * `addr` - Starting address to write to
    /// * `data` - Data to write
    ///
    /// # Errors
    /// * `AddressOutOfBounds` - If the write extends beyond flash size
    /// * `WriteError` - If the write operation fails
    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<()>;

    /// Erase a region of flash
    ///
    /// The address and length must be aligned to `erase_granularity()`.
    ///
    /// # Arguments
    /// * `addr` - Starting address to erase (must be aligned)
    /// * `len` - Number of bytes to erase (must be aligned)
    ///
    /// # Errors
    /// * `AddressOutOfBounds` - If the erase extends beyond flash size
    /// * `InvalidAlignment` - If address or length is not properly aligned
    /// * `EraseError` - If the erase operation fails
    async fn erase(&mut self, addr: u32, len: u32) -> Result<()>;

    /// Check if a range is valid for this device
    fn is_valid_range(&self, addr: u32, len: usize) -> bool {
        // Use u64 arithmetic to avoid truncation when len > u32::MAX
        let end = addr as u64 + len as u64;
        end <= self.size() as u64
    }

    // =========================================================================
    // Write Protection (optional, default implementations return unsupported)
    // =========================================================================

    /// Check if write protection operations are supported
    #[cfg(feature = "alloc")]
    fn wp_supported(&self) -> bool {
        false
    }

    /// Read current write protection configuration
    #[cfg(feature = "alloc")]
    async fn read_wp_config(&mut self) -> WpResult<WpConfig> {
        Err(WpError::ChipUnsupported)
    }

    /// Write write protection configuration
    #[cfg(feature = "alloc")]
    async fn write_wp_config(
        &mut self,
        _config: &WpConfig,
        _options: WriteOptions,
    ) -> WpResult<()> {
        Err(WpError::ChipUnsupported)
    }

    /// Set write protection mode only
    #[cfg(feature = "alloc")]
    async fn set_wp_mode(&mut self, _mode: WpMode, _options: WriteOptions) -> WpResult<()> {
        Err(WpError::ChipUnsupported)
    }

    /// Set protected range only
    #[cfg(feature = "alloc")]
    async fn set_wp_range(&mut self, _range: &WpRange, _options: WriteOptions) -> WpResult<()> {
        Err(WpError::ChipUnsupported)
    }

    /// Disable all write protection
    #[cfg(feature = "alloc")]
    async fn disable_wp(&mut self, _options: WriteOptions) -> WpResult<()> {
        Err(WpError::ChipUnsupported)
    }

    /// Get all available protection ranges
    #[cfg(feature = "alloc")]
    fn get_available_wp_ranges(&self) -> alloc::vec::Vec<WpRange> {
        alloc::vec::Vec::new()
    }
}

/// Extension trait for FlashDevice that provides additional capabilities
///
/// This is separate from the main trait to keep the core trait minimal and
/// easier to implement, while still providing useful derived functionality.
#[cfg(feature = "alloc")]
#[maybe_async(AFIT)]
pub trait FlashDeviceExt: FlashDevice {
    /// Read the entire flash contents
    async fn read_all(&mut self) -> Result<alloc::vec::Vec<u8>> {
        let size = self.size() as usize;
        let mut buf = alloc::vec![0u8; size];
        self.read(0, &mut buf).await?;
        Ok(buf)
    }

    /// Erase the entire flash chip
    async fn erase_all(&mut self) -> Result<()> {
        self.erase(0, self.size()).await
    }
}

#[cfg(feature = "alloc")]
impl<D: FlashDevice + ?Sized> FlashDeviceExt for D {}

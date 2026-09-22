//! SPI flash device adapter
//!
//! This module provides `SpiFlashDevice`, an adapter that implements
//! `FlashDevice` for SPI-based programmers.

use crate::chip::{EraseBlock, WriteGranularity};
use crate::error::{Error, Result};
use crate::flash::context::FlashContext;
use crate::flash::device::FlashDevice;
use crate::flash::operations;
use crate::programmer::SpiMaster;

use crate::wp::{
    self, RangeDecoder, WpBits, WpConfig, WpMode, WpRange, WpRegBitMap, WpResult, WriteOptions,
};

/// Flash device adapter for SPI-based programmers
///
/// This wraps a `SpiMaster` implementation along with the `FlashContext`
/// (chip metadata from JEDEC probing) to provide the unified `FlashDevice`
/// interface.
///
/// # Example
///
/// ```ignore
/// use rflasher_core::flash::{SpiFlashDevice, probe};
/// use rflasher_core::chip::ChipProvider;
/// use rflasher_programmers::ch341a::Ch341a;
///
/// fn create_flash_handle(db: &dyn ChipProvider) -> SpiFlashDevice<Ch341a> {
///     let mut master = Ch341a::open().unwrap();
///     let ctx = probe(&mut master, db).unwrap();
///     SpiFlashDevice::new(master, ctx)
/// }
/// ```
pub struct SpiFlashDevice<M: SpiMaster> {
    /// Owned SPI master
    master: M,
    /// Flash chip context
    ctx: FlashContext,
}

impl<M: SpiMaster> SpiFlashDevice<M> {
    /// Create a new SPI flash device adapter
    ///
    /// # Arguments
    /// * `master` - The SPI master to take ownership of
    /// * `ctx` - Flash context with chip metadata (from probing)
    pub fn new(master: M, ctx: FlashContext) -> Self {
        SpiFlashDevice { master, ctx }
    }

    /// Get a mutable reference to the underlying SPI master
    pub fn master(&mut self) -> &mut M {
        &mut self.master
    }

    /// Get a reference to the flash context
    pub fn context(&self) -> &FlashContext {
        &self.ctx
    }

    /// Get a mutable reference to the flash context
    pub fn context_mut(&mut self) -> &mut FlashContext {
        &mut self.ctx
    }

    /// Consume the adapter and return the flash context
    pub fn into_context(self) -> FlashContext {
        self.ctx
    }

    /// Consume the adapter and return both the SPI master and flash context
    pub fn into_parts(self) -> (M, FlashContext) {
        (self.master, self.ctx)
    }
}

impl<M: SpiMaster> FlashDevice for SpiFlashDevice<M> {
    fn size(&self) -> u32 {
        self.context().total_size() as u32
    }

    fn erase_granularity(&self) -> u32 {
        self.context().chip.min_erase_size().unwrap_or(4096) // Default to 4KB if no erase blocks defined
    }

    fn write_granularity(&self) -> WriteGranularity {
        self.context().chip.write_granularity
    }

    fn erase_blocks(&self) -> &[EraseBlock] {
        self.context().chip.erase_blocks()
    }

    fn page_size(&self) -> u32 {
        self.ctx.page_size() as u32
    }

    // Write protection support
    #[cfg(feature = "alloc")]
    fn wp_supported(&self) -> bool {
        true
    }

    #[cfg(feature = "alloc")]
    async fn read_wp_config(&mut self) -> WpResult<WpConfig> {
        SpiFlashDevice::read_wp_config(self).await
    }

    #[cfg(feature = "alloc")]
    async fn write_wp_config(&mut self, config: &WpConfig, options: WriteOptions) -> WpResult<()> {
        SpiFlashDevice::write_wp_config(self, config, options).await
    }

    #[cfg(feature = "alloc")]
    async fn set_wp_mode(&mut self, mode: WpMode, options: WriteOptions) -> WpResult<()> {
        SpiFlashDevice::set_wp_mode(self, mode, options).await
    }

    #[cfg(feature = "alloc")]
    async fn set_wp_range(&mut self, range: &WpRange, options: WriteOptions) -> WpResult<()> {
        SpiFlashDevice::set_wp_range(self, range, options).await
    }

    #[cfg(feature = "alloc")]
    async fn disable_wp(&mut self, options: WriteOptions) -> WpResult<()> {
        SpiFlashDevice::disable_wp(self, options).await
    }

    #[cfg(feature = "alloc")]
    fn get_available_wp_ranges(&self) -> alloc::vec::Vec<WpRange> {
        SpiFlashDevice::get_available_wp_ranges(self)
    }

    async fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        if !self.context().is_valid_range(addr, buf.len()) {
            return Err(Error::AddressOutOfBounds);
        }
        // Delegate to the canonical free function to avoid divergent duplicates
        operations::read(&mut self.master, &self.ctx, addr, buf).await
    }

    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        if !self.context().is_valid_range(addr, data.len()) {
            return Err(Error::AddressOutOfBounds);
        }
        // Delegate to the canonical free function (handles AAI word program,
        // byte-granularity chips, page splitting and 4-byte addressing)
        operations::write(&mut self.master, &self.ctx, addr, data).await
    }

    async fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
        if !self.ctx.is_valid_range(addr, len as usize) {
            return Err(Error::AddressOutOfBounds);
        }
        operations::erase_spi_range(&mut self.master, &self.ctx, addr, len).await?;
        self.check_erased_range(addr, len).await
    }
}

impl<M: SpiMaster> SpiFlashDevice<M> {
    /// Check that a range of flash has been erased (all bytes are 0xFF)
    async fn check_erased_range(&mut self, addr: u32, len: u32) -> Result<()> {
        operations::check_erased_range(&mut self.master, &self.ctx, addr, len).await
    }
}

// =============================================================================
// Write Protection Support
// =============================================================================

impl<M: SpiMaster> SpiFlashDevice<M> {
    /// Get the WP register bit map for this chip
    ///
    /// Returns a standard Winbond-style bit map. In the future, this could
    /// be made chip-specific based on the chip database.
    fn wp_bit_map(&self) -> WpRegBitMap {
        // Check if chip has BP3 (4 BP bits)
        let features = self.ctx.chip.features;
        if features.contains(crate::chip::Features::WP_BP3) {
            WpRegBitMap::winbond_with_bp3()
        } else {
            WpRegBitMap::winbond_standard()
        }
    }

    /// Get the range decoder for this chip
    fn wp_decoder(&self) -> RangeDecoder {
        // Default to standard SPI25 decoding
        // In the future, this could be made chip-specific
        RangeDecoder::Spi25
    }

    /// Read current write protection bits
    pub async fn read_wp_bits(&mut self) -> WpResult<WpBits> {
        let bit_map = self.wp_bit_map();
        wp::read_wp_bits(&mut self.master, &bit_map).await
    }

    /// Read current write protection configuration
    pub async fn read_wp_config(&mut self) -> WpResult<WpConfig> {
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
        wp::read_wp_config(&mut self.master, &bit_map, total_size, decoder).await
    }

    /// Augment `WriteOptions` with chip-specific settings derived from feature flags.
    ///
    /// Injects `use_ewsr = true` when the chip has `WRSR_EWSR` (legacy SST25 chips
    /// that require EWSR (0x50) instead of WREN (0x06) before status register writes).
    fn chip_write_options(&self, options: WriteOptions) -> WriteOptions {
        WriteOptions {
            use_ewsr: self
                .ctx
                .chip
                .features
                .contains(crate::chip::Features::WRSR_EWSR),
            ..options
        }
    }

    /// Write write protection bits
    pub async fn write_wp_bits(&mut self, bits: &WpBits, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        let options = self.chip_write_options(options);
        wp::write_wp_bits(&mut self.master, bits, &bit_map, options).await
    }

    /// Write write protection configuration
    pub async fn write_wp_config(
        &mut self,
        config: &WpConfig,
        options: WriteOptions,
    ) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
        let options = self.chip_write_options(options);
        wp::write_wp_config(
            &mut self.master,
            config,
            &bit_map,
            total_size,
            decoder,
            options,
        )
        .await
    }

    /// Set write protection mode
    pub async fn set_wp_mode(&mut self, mode: WpMode, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        let options = self.chip_write_options(options);
        wp::set_wp_mode(&mut self.master, mode, &bit_map, options).await
    }

    /// Set protected range
    pub async fn set_wp_range(&mut self, range: &WpRange, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
        let options = self.chip_write_options(options);
        wp::set_wp_range(
            &mut self.master,
            range,
            &bit_map,
            total_size,
            decoder,
            options,
        )
        .await
    }

    /// Disable write protection
    pub async fn disable_wp(&mut self, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        let options = self.chip_write_options(options);
        wp::disable_wp(&mut self.master, &bit_map, options).await
    }

    /// Get all available protection ranges
    #[cfg(feature = "alloc")]
    pub fn get_available_wp_ranges(&self) -> alloc::vec::Vec<WpRange> {
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
        wp::get_available_ranges(&bit_map, total_size, decoder)
    }
}

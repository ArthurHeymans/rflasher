//! SPI flash device adapter
//!
//! This module provides `SpiFlashDevice`, an adapter that implements
//! `FlashDevice` for SPI-based programmers.

use crate::chip::{EraseBlock, WriteGranularity};
use crate::error::{EraseFailure, Error, Result};
use crate::flash::context::{AddressMode, FlashContext};
use crate::flash::device::FlashDevice;
use crate::flash::operations::select_erase_block;
use crate::programmer::SpiMaster;
use crate::protocol;
use crate::wp::{
    self, RangeDecoder, WpBits, WpConfig, WpMode, WpRange, WpRegBitMap, WpResult, WriteOptions,
};
use maybe_async::maybe_async;

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
/// use rflasher_core::chip::ChipDatabase;
/// use rflasher_ch341a::Ch341a;
///
/// fn create_flash_handle(db: &ChipDatabase) -> SpiFlashDevice<Ch341a> {
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

#[maybe_async(AFIT)]
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
        use crate::spi::IoMode;

        let ctx = self.context();
        if !ctx.is_valid_range(addr, buf.len()) {
            return Err(Error::AddressOutOfBounds);
        }

        let chip_features = ctx.chip.features;
        let use_4byte_native =
            ctx.address_mode == AddressMode::FourByte && chip_features.supports_4ba_read();
        let enter_exit_4byte = ctx.address_mode == AddressMode::FourByte && !use_4byte_native;

        let master_features = self.master().features();

        // Select the best read mode based on chip and programmer capabilities
        let (io_mode, ..) =
            protocol::select_read_mode(master_features, chip_features, use_4byte_native);

        // Enter 4-byte mode if needed and not using native commands
        if enter_exit_4byte {
            protocol::enter_4byte_mode_with_features(self.master(), chip_features).await?;
        }

        let result = match io_mode {
            IoMode::Single => {
                if use_4byte_native {
                    protocol::read_4b(self.master(), addr, buf).await
                } else {
                    protocol::read_3b(self.master(), addr, buf).await
                }
            }
            IoMode::DualOut => {
                if use_4byte_native {
                    protocol::read_dual_out_4b(self.master(), addr, buf).await
                } else {
                    protocol::read_dual_out_3b(self.master(), addr, buf).await
                }
            }
            IoMode::DualIo => {
                if use_4byte_native {
                    protocol::read_dual_io_4b(self.master(), addr, buf).await
                } else {
                    protocol::read_dual_io_3b(self.master(), addr, buf).await
                }
            }
            IoMode::QuadOut => {
                if use_4byte_native {
                    protocol::read_quad_out_4b(self.master(), addr, buf).await
                } else {
                    protocol::read_quad_out_3b(self.master(), addr, buf).await
                }
            }
            IoMode::QuadIo => {
                if use_4byte_native {
                    protocol::read_quad_io_4b(self.master(), addr, buf).await
                } else {
                    protocol::read_quad_io_3b(self.master(), addr, buf).await
                }
            }
            IoMode::Qpi => {
                // QPI mode requires special handling - fall back to single for now
                log::warn!("QPI read mode not yet implemented, falling back to single I/O");
                if use_4byte_native {
                    protocol::read_4b(self.master(), addr, buf).await
                } else {
                    protocol::read_3b(self.master(), addr, buf).await
                }
            }
        };

        // Exit 4-byte mode if we entered it
        if enter_exit_4byte {
            if let Err(e) =
                protocol::exit_4byte_mode_with_features(self.master(), chip_features).await
            {
                log::warn!("Failed to exit 4-byte address mode: {}", e);
            }
        }

        result
    }

    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        use crate::chip::{Features, WriteGranularity};

        let ctx = self.context();
        if !ctx.is_valid_range(addr, data.len()) {
            return Err(Error::AddressOutOfBounds);
        }

        let features = ctx.chip.features;
        let write_granularity = ctx.chip.write_granularity;
        let page_size = ctx.page_size();
        let use_4byte = ctx.address_mode == AddressMode::FourByte;
        let use_native = features.supports_4ba_program();

        // SST25 AAI word program: chip database sets AAI_WORD for SST25VFxxxB/SST25WFxxx.
        // These chips require a streaming protocol (0xAD) rather than page program (0x02).
        // AAI uses 3-byte addressing only — 4-byte mode is irrelevant for SST25 chips.
        // Note: SFDP-probed chips may report WriteGranularity::Byte (BFPT DWORD1 bit[2]=0)
        // without AAI_WORD being set; those fall through to single-byte page program below.
        if features.contains(Features::AAI_WORD) {
            return protocol::aai_word_program(self.master(), addr, data).await;
        }

        // Get the master's maximum write length - some controllers have limits
        // smaller than a full page (e.g., Intel swseq is limited to 64 bytes)
        let max_write = self.master().max_write_len();

        // Enter 4-byte mode if needed and not using native commands
        if use_4byte && !use_native {
            protocol::enter_4byte_mode_with_features(self.master(), features).await?;
        }

        let mut offset = 0usize;
        let mut current_addr = addr;

        while offset < data.len() {
            let remaining = data.len() - offset;

            let chunk_size = if write_granularity == WriteGranularity::Byte {
                // Single-byte page program: one byte per WREN+PP+WIP cycle.
                // Used for SFDP-detected chips reporting byte granularity (BFPT DWORD1
                // bit[2]=0) that don't have AAI_WORD — matches flashprog spi_chip_write_1.
                1
            } else {
                // Page-granularity program: up to a full page per command, respecting
                // page boundaries and the master's maximum write length.
                let page_offset = (current_addr as usize) % page_size;
                let bytes_to_page_end = page_size - page_offset;
                core::cmp::min(core::cmp::min(bytes_to_page_end, remaining), max_write)
            };

            let chunk = &data[offset..offset + chunk_size];

            let result = if use_4byte && use_native {
                protocol::program_page_4b(self.master(), current_addr, chunk).await
            } else {
                protocol::program_page_3b(self.master(), current_addr, chunk).await
            };

            if result.is_err() {
                if use_4byte && !use_native {
                    if let Err(e) =
                        protocol::exit_4byte_mode_with_features(self.master(), features).await
                    {
                        log::warn!("Failed to exit 4-byte address mode: {}", e);
                    }
                }
                return result;
            }

            offset += chunk_size;
            current_addr += chunk_size as u32;
        }

        // Exit 4-byte mode if we entered it
        if use_4byte && !use_native {
            protocol::exit_4byte_mode_with_features(self.master(), features).await?;
        }

        Ok(())
    }

    async fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
        use crate::chip::Features;

        let ctx = self.context();
        if !ctx.is_valid_range(addr, len as usize) {
            return Err(Error::AddressOutOfBounds);
        }

        // Extract what we need from ctx before taking a mutable borrow on self.master()
        let needs_sst26_unprotect = ctx.chip.features.contains(Features::SST26_BPR);

        // SST26 chips use a per-block protection register (not SR BP bits).
        // A global unlock (WREN + ULBPR 0x98) is required before any erase succeeds.
        // This is equivalent to flashprog's ssi_disable_blockprotect_sst26_global_unprotect().
        if needs_sst26_unprotect {
            protocol::sst26_global_unprotect(self.master()).await?;
        }

        // Re-borrow ctx after the mutable borrow above is released
        let ctx = self.context();

        // Find the best erase block size for this operation
        let erase_block = select_erase_block(ctx.chip.erase_blocks(), addr, len)
            .ok_or(Error::InvalidAlignment)?;

        let chip_features = ctx.chip.features;
        let use_4byte = ctx.address_mode == AddressMode::FourByte;
        let use_native = use_4byte && erase_block.opcode_4b.is_some();
        let opcode = erase_block.opcode_for_address_width(use_native);

        // Enter 4-byte mode if needed
        if use_4byte && !use_native {
            protocol::enter_4byte_mode_with_features(self.master(), chip_features).await?;
        }

        let mut current_addr = addr;
        let end_addr = addr + len;

        // For non-uniform erase blocks, use the maximum block size for timeout calculation
        let max_block_size = erase_block.max_block_size();

        // Poll delay and timeout depend on block size
        let (poll_delay_us, timeout_us) = match max_block_size {
            s if s <= 4096 => (10_000, 1_000_000), // 4KB: 10ms poll, 1s timeout
            s if s <= 32768 => (100_000, 4_000_000), // 32KB: 100ms poll, 4s timeout
            s if s <= 65536 => (100_000, 4_000_000), // 64KB: 100ms poll, 4s timeout
            _ => (500_000, 60_000_000),            // Larger: 500ms poll, 60s timeout
        };

        while current_addr < end_addr {
            // Get the block size at the current offset within the erase layout
            let offset_in_layout = current_addr - addr;
            let block_size = erase_block
                .block_size_at_offset(offset_in_layout)
                .unwrap_or(max_block_size);

            let result = protocol::erase_block(
                self.master(),
                opcode,
                current_addr,
                use_4byte,
                poll_delay_us,
                timeout_us,
            )
            .await;

            if result.is_err() {
                if use_4byte && !use_native {
                    if let Err(e) =
                        protocol::exit_4byte_mode_with_features(self.master(), chip_features).await
                    {
                        log::warn!("Failed to exit 4-byte address mode: {}", e);
                    }
                }
                return result;
            }

            // Verify the block was erased
            if let Err(e) = self.check_erased_range(current_addr, block_size).await {
                if use_4byte && !use_native {
                    if let Err(exit_e) =
                        protocol::exit_4byte_mode_with_features(self.master(), chip_features).await
                    {
                        log::warn!("Failed to exit 4-byte address mode: {}", exit_e);
                    }
                }
                return Err(e);
            }

            current_addr += block_size;
        }

        // Exit 4-byte mode
        if use_4byte && !use_native {
            protocol::exit_4byte_mode_with_features(self.master(), chip_features).await?;
        }

        Ok(())
    }
}

impl<M: SpiMaster> SpiFlashDevice<M> {
    /// Check that a range of flash has been erased (all bytes are 0xFF)
    ///
    /// Uses the `FlashDevice::read` trait method, which differs from
    /// `operations::check_erased_range` that uses the free function `read()`.
    #[maybe_async]
    async fn check_erased_range(&mut self, addr: u32, len: u32) -> Result<()> {
        const ERASED_VALUE: u8 = 0xFF;
        const CHUNK_SIZE: usize = 4096;
        let mut buf = [0u8; CHUNK_SIZE];

        let mut offset = 0u32;
        while offset < len {
            let chunk_len = core::cmp::min(CHUNK_SIZE as u32, len - offset) as usize;
            let chunk_buf = &mut buf[..chunk_len];

            FlashDevice::read(self, addr + offset, chunk_buf).await?;

            if let Some((idx, &found)) = chunk_buf
                .iter()
                .enumerate()
                .find(|(_, &b)| b != ERASED_VALUE)
            {
                return Err(Error::EraseError(EraseFailure::VerifyFailed {
                    addr: addr + offset + idx as u32,
                    found,
                }));
            }

            offset += chunk_len as u32;
        }

        Ok(())
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
    #[maybe_async]
    pub async fn read_wp_bits(&mut self) -> WpResult<WpBits> {
        let bit_map = self.wp_bit_map();
        wp::read_wp_bits(&mut self.master, &bit_map).await
    }

    /// Read current write protection configuration
    #[maybe_async]
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
    #[maybe_async]
    pub async fn write_wp_bits(&mut self, bits: &WpBits, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        let options = self.chip_write_options(options);
        wp::write_wp_bits(&mut self.master, bits, &bit_map, options).await
    }

    /// Write write protection configuration
    #[maybe_async]
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
    #[maybe_async]
    pub async fn set_wp_mode(&mut self, mode: WpMode, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        let options = self.chip_write_options(options);
        wp::set_wp_mode(&mut self.master, mode, &bit_map, options).await
    }

    /// Set protected range
    #[maybe_async]
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
    #[maybe_async]
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

//! Hybrid flash device adapter
//!
//! This module provides `HybridFlashDevice`, an adapter for programmers that
//! implement both `SpiMaster` (for probe, erase, status, write protection) and
//! `OpaqueMaster` (for fast bulk read/write via hardware-accelerated paths).
//!
//! This is the natural fit for programmers like the Dediprog SF-series, which
//! support generic SPI command pass-through (`CMD_TRANSCEIVE`) for arbitrary
//! opcodes, but also have dedicated firmware commands (`CMD_READ`/`CMD_WRITE`)
//! that handle SPI flash protocols internally with USB bulk transfers for
//! dramatically higher throughput.
//!
//! # Architecture
//!
//! ```text
//!   FlashDevice::read()  ──► OpaqueMaster::read()   (CMD_READ + bulk IN)
//!   FlashDevice::write() ──► OpaqueMaster::write()   (CMD_WRITE + bulk OUT)
//!   FlashDevice::erase() ──► SpiMaster (WREN + SE/BE + RDSR polling)
//!   FlashDevice::wp_*()  ──► SpiMaster (status register access)
//! ```

use crate::chip::{EraseBlock, WriteGranularity};
use crate::error::{Error, Result};
use crate::flash::context::{AddressMode, FlashContext};
use crate::flash::device::FlashDevice;
use crate::flash::operations::{map_to_4byte_erase_opcode, select_erase_block};
use crate::programmer::{OpaqueMaster, SpiMaster};
use crate::protocol;
#[cfg(feature = "alloc")]
use crate::wp::{
    self, RangeDecoder, WpBits, WpConfig, WpMode, WpRange, WpRegBitMap, WpResult, WriteOptions,
};
use maybe_async::maybe_async;

/// Flash device adapter for hybrid programmers (SpiMaster + OpaqueMaster)
///
/// Uses `OpaqueMaster` for bulk read/write (fast path) and `SpiMaster` for
/// everything else (probe, erase, status registers, write protection).
///
/// # Example
///
/// ```ignore
/// use rflasher_core::flash::{HybridFlashDevice, probe};
/// use rflasher_core::chip::ChipDatabase;
/// use rflasher_dediprog::Dediprog;
///
/// let mut master = Dediprog::open().unwrap();
/// let ctx = probe(&mut master, &db).unwrap();
/// master.set_flash_size(ctx.total_size() as u32);
/// let mut device = HybridFlashDevice::new(master, ctx);
/// ```
pub struct HybridFlashDevice<M: SpiMaster + OpaqueMaster> {
    /// Owned master (implements both SpiMaster and OpaqueMaster)
    master: M,
    /// Flash chip context (from probing via SpiMaster)
    ctx: FlashContext,
}

impl<M: SpiMaster + OpaqueMaster> HybridFlashDevice<M> {
    /// Create a new hybrid flash device adapter
    ///
    /// # Arguments
    /// * `master` - The programmer (must implement both SpiMaster and OpaqueMaster)
    /// * `ctx` - Flash context with chip metadata (from probing via SpiMaster)
    pub fn new(master: M, ctx: FlashContext) -> Self {
        HybridFlashDevice { master, ctx }
    }

    /// Get a mutable reference to the underlying master
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

    /// Consume the adapter and return both the master and flash context
    pub fn into_parts(self) -> (M, FlashContext) {
        (self.master, self.ctx)
    }
}

#[maybe_async(AFIT)]
impl<M: SpiMaster + OpaqueMaster> FlashDevice for HybridFlashDevice<M> {
    fn size(&self) -> u32 {
        self.ctx.total_size() as u32
    }

    fn erase_granularity(&self) -> u32 {
        self.ctx.chip.min_erase_size().unwrap_or(4096)
    }

    fn write_granularity(&self) -> WriteGranularity {
        self.ctx.chip.write_granularity
    }

    fn erase_blocks(&self) -> &[EraseBlock] {
        self.ctx.chip.erase_blocks()
    }

    fn page_size(&self) -> u32 {
        self.ctx.page_size() as u32
    }

    // Write protection support (delegates to SpiMaster, same as SpiFlashDevice)
    #[cfg(feature = "alloc")]
    fn wp_supported(&self) -> bool {
        true
    }

    #[cfg(feature = "alloc")]
    async fn read_wp_config(&mut self) -> WpResult<WpConfig> {
        HybridFlashDevice::read_wp_config(self).await
    }

    #[cfg(feature = "alloc")]
    async fn write_wp_config(&mut self, config: &WpConfig, options: WriteOptions) -> WpResult<()> {
        HybridFlashDevice::write_wp_config(self, config, options).await
    }

    #[cfg(feature = "alloc")]
    async fn set_wp_mode(&mut self, mode: WpMode, options: WriteOptions) -> WpResult<()> {
        HybridFlashDevice::set_wp_mode(self, mode, options).await
    }

    #[cfg(feature = "alloc")]
    async fn set_wp_range(&mut self, range: &WpRange, options: WriteOptions) -> WpResult<()> {
        HybridFlashDevice::set_wp_range(self, range, options).await
    }

    #[cfg(feature = "alloc")]
    async fn disable_wp(&mut self, options: WriteOptions) -> WpResult<()> {
        HybridFlashDevice::disable_wp(self, options).await
    }

    #[cfg(feature = "alloc")]
    fn get_available_wp_ranges(&self) -> alloc::vec::Vec<WpRange> {
        HybridFlashDevice::get_available_wp_ranges(self)
    }

    // =========================================================================
    // Read/Write: use OpaqueMaster (fast bulk path)
    // =========================================================================

    async fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        let ctx = self.context();
        if !ctx.is_valid_range(addr, buf.len()) {
            return Err(Error::AddressOutOfBounds);
        }

        // OpaqueMaster::read handles alignment splitting internally
        OpaqueMaster::read(&mut self.master, addr, buf).await
    }

    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        let ctx = self.context();
        if !ctx.is_valid_range(addr, data.len()) {
            return Err(Error::AddressOutOfBounds);
        }

        // OpaqueMaster::write handles alignment splitting internally
        OpaqueMaster::write(&mut self.master, addr, data).await
    }

    // =========================================================================
    // Erase: use SpiMaster (no bulk erase command on Dediprog)
    // =========================================================================

    async fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
        let ctx = self.context();
        if !ctx.is_valid_range(addr, len as usize) {
            return Err(Error::AddressOutOfBounds);
        }

        let erase_block = select_erase_block(ctx.chip.erase_blocks(), addr, len)
            .ok_or(Error::InvalidAlignment)?;

        let use_4byte = ctx.address_mode == AddressMode::FourByte;
        let use_native = ctx.use_native_4byte;

        let opcode = if use_4byte && use_native {
            map_to_4byte_erase_opcode(erase_block.opcode)
        } else {
            erase_block.opcode
        };

        if use_4byte && !use_native {
            protocol::enter_4byte_mode(self.master()).await?;
        }

        let mut current_addr = addr;
        let end_addr = addr + len;
        let max_block_size = erase_block.max_block_size();

        let (poll_delay_us, timeout_us) = match max_block_size {
            s if s <= 4096 => (10_000, 1_000_000),
            s if s <= 32768 => (100_000, 4_000_000),
            s if s <= 65536 => (100_000, 4_000_000),
            _ => (500_000, 60_000_000),
        };

        while current_addr < end_addr {
            let offset_in_layout = current_addr - addr;
            let block_size = erase_block
                .block_size_at_offset(offset_in_layout)
                .unwrap_or(max_block_size);

            // Try native erase first (on-device busy-wait, e.g. SPI_CMD_SPINOR_WAIT).
            // Falls back to generic WREN + erase + host-side RDSR polling.
            let result = if let Some(r) =
                self.master()
                    .native_erase_block(opcode, current_addr, use_4byte && use_native)
            {
                r
            } else {
                protocol::erase_block(
                    self.master(),
                    opcode,
                    current_addr,
                    use_4byte && use_native,
                    poll_delay_us,
                    timeout_us,
                )
                .await
            };

            if result.is_err() {
                if use_4byte && !use_native {
                    let _ = protocol::exit_4byte_mode(self.master()).await;
                }
                return result;
            }

            current_addr += block_size;
        }

        if use_4byte && !use_native {
            protocol::exit_4byte_mode(self.master()).await?;
        }

        Ok(())
    }
}

// =============================================================================
// Write Protection Support (delegates to SpiMaster, identical to SpiFlashDevice)
// =============================================================================

impl<M: SpiMaster + OpaqueMaster> HybridFlashDevice<M> {
    fn wp_bit_map(&self) -> WpRegBitMap {
        let features = self.ctx.chip.features;
        if features.contains(crate::chip::Features::WP_BP3) {
            WpRegBitMap::winbond_with_bp3()
        } else {
            WpRegBitMap::winbond_standard()
        }
    }

    fn wp_decoder(&self) -> RangeDecoder {
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

    /// Write write protection bits
    #[maybe_async]
    pub async fn write_wp_bits(&mut self, bits: &WpBits, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
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
        wp::set_wp_mode(&mut self.master, mode, &bit_map, options).await
    }

    /// Set protected range
    #[maybe_async]
    pub async fn set_wp_range(&mut self, range: &WpRange, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
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

    /// Disable all write protection
    #[maybe_async]
    pub async fn disable_wp(&mut self, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
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

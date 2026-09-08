//! SpiFlashDevice: operation-scoped SPI flash access.
use crate::chip::{EraseBlock, WriteGranularity};
use crate::error::{Error, Result};
use crate::flash::io::{self, IoLifecycle};
use crate::flash::{FlashContext, FlashDevice};
use crate::programmer::SpiMaster;
use crate::protocol;
use crate::wp::{
    self, RangeDecoder, WpBits, WpConfig, WpMode, WpRange, WpRegBitMap, WpResult, WriteOptions,
};

/// Adapter assuming an externally established ordinary-SPI, three-byte baseline.
/// Probe does not reset the chip. After cancellation/failed cleanup, explicitly
/// recover the hardware and reprobe; USB disconnect alone is not recovery.
pub struct SpiFlashDevice<M: SpiMaster> {
    master: M,
    ctx: FlashContext,
    lifecycle: IoLifecycle,
}
impl<M: SpiMaster> SpiFlashDevice<M> {
    /// Construct on a known idle ordinary-SPI, three-byte baseline.
    pub fn new(master: M, ctx: FlashContext) -> Self {
        Self {
            master,
            ctx,
            lifecycle: IoLifecycle::default(),
        }
    }
    /// Low-level escape: caller owns hardware recovery if the adapter is latched.
    pub fn master(&mut self) -> &mut M {
        &mut self.master
    }
    /// Chip metadata; this does not describe temporary hardware state.
    pub fn context(&self) -> &FlashContext {
        &self.ctx
    }
    /// Mutable chip metadata, not a hardware recovery mechanism.
    pub fn context_mut(&mut self) -> &mut FlashContext {
        &mut self.ctx
    }
    /// Whether explicit hardware recovery and reprobe are required.
    pub fn recovery_required(&self) -> bool {
        self.lifecycle.recovery_required()
    }
    /// Consume the adapter, discarding its master.
    pub fn into_context(self) -> FlashContext {
        self.ctx
    }
    /// Low-level escape, not recovery. Do not reconstruct an adapter on uncertain hardware.
    pub fn into_parts(self) -> (M, FlashContext) {
        (self.master, self.ctx)
    }
}
impl<M: SpiMaster> FlashDevice for SpiFlashDevice<M> {
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
        self.lifecycle.check()?;
        if !self.ctx.is_valid_range(addr, buf.len()) {
            return Err(Error::AddressOutOfBounds);
        }
        let plan = io::select_read_plan(&self.master, &self.ctx, false, |_| true)?;
        self.lifecycle
            .run(&mut self.master, plan.address, plan.qe, async |master| {
                io::read_spi(master, &plan, addr, buf).await
            })
            .await
    }
    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        self.lifecycle.check()?;
        if !self.ctx.is_valid_range(addr, data.len()) {
            return Err(Error::AddressOutOfBounds);
        }
        let plan = io::select_write_plan(&self.master, &self.ctx, |_| true)?;
        self.lifecycle
            .run(
                &mut self.master,
                plan.address,
                protocol::QuadEnableMethod::None,
                async |master| io::write_spi(master, &plan, addr, data).await,
            )
            .await
    }
    async fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
        self.lifecycle.check()?;
        if !self.ctx.is_valid_range(addr, len as usize) {
            return Err(Error::AddressOutOfBounds);
        }
        let plan = io::select_erase_plan(&self.master, &self.ctx, addr, len)?;
        let verify_plan = io::select_read_plan(&self.master, &self.ctx, true, |_| true)?;
        let unprotect = self
            .ctx
            .chip
            .features
            .contains(crate::chip::Features::SST26_BPR);
        self.lifecycle
            .run(
                &mut self.master,
                plan.address,
                protocol::QuadEnableMethod::None,
                async |master| {
                    if unprotect {
                        protocol::sst26_global_unprotect(master).await?;
                    }
                    io::erase_spi(master, &plan, addr, len).await
                },
            )
            .await?;
        // Verification owns a fresh, single-I/O scope after erase addressing is restored.
        io::verify_erased(
            &mut self.lifecycle,
            &mut self.master,
            &verify_plan,
            addr,
            len,
        )
        .await
    }
}

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
        self.lifecycle.check()?;
        let bit_map = self.wp_bit_map();
        self.lifecycle
            .run(
                &mut self.master,
                io::AddressPlan::ThreeByte,
                protocol::QuadEnableMethod::None,
                async |master| wp::read_wp_bits(master, &bit_map).await,
            )
            .await
    }

    /// Read current write protection configuration
    pub async fn read_wp_config(&mut self) -> WpResult<WpConfig> {
        self.lifecycle.check()?;
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
        self.lifecycle
            .run(
                &mut self.master,
                io::AddressPlan::ThreeByte,
                protocol::QuadEnableMethod::None,
                async |master| wp::read_wp_config(master, &bit_map, total_size, decoder).await,
            )
            .await
    }

    /// Write write protection bits
    pub async fn write_wp_bits(&mut self, bits: &WpBits, options: WriteOptions) -> WpResult<()> {
        self.lifecycle.check()?;
        let options = wp::chip_write_options(self.ctx.chip.features, options);
        let bit_map = self.wp_bit_map();
        self.lifecycle
            .run(
                &mut self.master,
                io::AddressPlan::ThreeByte,
                protocol::QuadEnableMethod::None,
                async |master| wp::write_wp_bits(master, bits, &bit_map, options).await,
            )
            .await
    }

    /// Write write protection configuration
    pub async fn write_wp_config(
        &mut self,
        config: &WpConfig,
        options: WriteOptions,
    ) -> WpResult<()> {
        self.lifecycle.check()?;
        let options = wp::chip_write_options(self.ctx.chip.features, options);
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
        self.lifecycle
            .run(
                &mut self.master,
                io::AddressPlan::ThreeByte,
                protocol::QuadEnableMethod::None,
                async |master| {
                    wp::write_wp_config(master, config, &bit_map, total_size, decoder, options)
                        .await
                },
            )
            .await
    }

    /// Set write protection mode
    pub async fn set_wp_mode(&mut self, mode: WpMode, options: WriteOptions) -> WpResult<()> {
        self.lifecycle.check()?;
        let options = wp::chip_write_options(self.ctx.chip.features, options);
        let bit_map = self.wp_bit_map();
        self.lifecycle
            .run(
                &mut self.master,
                io::AddressPlan::ThreeByte,
                protocol::QuadEnableMethod::None,
                async |master| wp::set_wp_mode(master, mode, &bit_map, options).await,
            )
            .await
    }

    /// Set protected range
    pub async fn set_wp_range(&mut self, range: &WpRange, options: WriteOptions) -> WpResult<()> {
        self.lifecycle.check()?;
        let options = wp::chip_write_options(self.ctx.chip.features, options);
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
        self.lifecycle
            .run(
                &mut self.master,
                io::AddressPlan::ThreeByte,
                protocol::QuadEnableMethod::None,
                async |master| {
                    wp::set_wp_range(master, range, &bit_map, total_size, decoder, options).await
                },
            )
            .await
    }

    /// Disable write protection
    pub async fn disable_wp(&mut self, options: WriteOptions) -> WpResult<()> {
        self.lifecycle.check()?;
        let options = wp::chip_write_options(self.ctx.chip.features, options);
        let bit_map = self.wp_bit_map();
        self.lifecycle
            .run(
                &mut self.master,
                io::AddressPlan::ThreeByte,
                protocol::QuadEnableMethod::None,
                async |master| wp::disable_wp(master, &bit_map, options).await,
            )
            .await
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

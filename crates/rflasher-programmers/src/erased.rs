//! Object erasure for the async core traits.
//!
//! `rflasher-core`'s I/O traits use `async fn` and are therefore not
//! dyn-compatible. The registry still needs runtime backend selection, so
//! this module provides object-safe mirror traits whose async methods return
//! boxed futures, blanket adapters from any concrete core implementation,
//! and concrete wrapper types (`ErasedFlashDevice`, `ErasedSpiMaster`) that
//! implement the real core traits again on top of the erased objects.
//!
//! The one boxed-future allocation per operation is negligible next to USB
//! and flash latency. These types are implementation details of the registry
//! and REPL support; treat them as unstable.

use std::future::Future;
use std::pin::Pin;

use rflasher_core::chip::{EraseBlock, WriteGranularity};
use rflasher_core::error::Result;
use rflasher_core::flash::FlashDevice;
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::SpiCommand;
use rflasher_core::wp::{WpConfig, WpMode, WpRange, WpResult, WriteOptions};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

// =============================================================================
// FlashDevice erasure
// =============================================================================

/// Object-safe mirror of [`FlashDevice`].
trait DynFlashDevice {
    fn size(&self) -> u32;
    fn erase_granularity(&self) -> u32;
    fn write_granularity(&self) -> WriteGranularity;
    fn erase_blocks(&self) -> &[EraseBlock];
    fn page_size(&self) -> u32;
    fn is_valid_range(&self, addr: u32, len: usize) -> bool;
    fn read<'a>(&'a mut self, addr: u32, buf: &'a mut [u8]) -> BoxFuture<'a, Result<()>>;
    fn write<'a>(&'a mut self, addr: u32, data: &'a [u8]) -> BoxFuture<'a, Result<()>>;
    fn erase(&mut self, addr: u32, len: u32) -> BoxFuture<'_, Result<()>>;
    fn finish(&mut self) -> BoxFuture<'_, Result<()>>;
    fn wp_supported(&self) -> bool;
    fn read_wp_config(&mut self) -> BoxFuture<'_, WpResult<WpConfig>>;
    fn write_wp_config<'a>(
        &'a mut self,
        config: &'a WpConfig,
        options: WriteOptions,
    ) -> BoxFuture<'a, WpResult<()>>;
    fn set_wp_mode(&mut self, mode: WpMode, options: WriteOptions) -> BoxFuture<'_, WpResult<()>>;
    fn set_wp_range<'a>(
        &'a mut self,
        range: &'a WpRange,
        options: WriteOptions,
    ) -> BoxFuture<'a, WpResult<()>>;
    fn disable_wp(&mut self, options: WriteOptions) -> BoxFuture<'_, WpResult<()>>;
    fn get_available_wp_ranges(&self) -> Vec<WpRange>;
}

impl<D: FlashDevice> DynFlashDevice for D {
    fn size(&self) -> u32 {
        FlashDevice::size(self)
    }
    fn erase_granularity(&self) -> u32 {
        FlashDevice::erase_granularity(self)
    }
    fn write_granularity(&self) -> WriteGranularity {
        FlashDevice::write_granularity(self)
    }
    fn erase_blocks(&self) -> &[EraseBlock] {
        FlashDevice::erase_blocks(self)
    }
    fn page_size(&self) -> u32 {
        FlashDevice::page_size(self)
    }
    fn is_valid_range(&self, addr: u32, len: usize) -> bool {
        FlashDevice::is_valid_range(self, addr, len)
    }
    fn read<'a>(&'a mut self, addr: u32, buf: &'a mut [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(FlashDevice::read(self, addr, buf))
    }
    fn write<'a>(&'a mut self, addr: u32, data: &'a [u8]) -> BoxFuture<'a, Result<()>> {
        Box::pin(FlashDevice::write(self, addr, data))
    }
    fn erase(&mut self, addr: u32, len: u32) -> BoxFuture<'_, Result<()>> {
        Box::pin(FlashDevice::erase(self, addr, len))
    }
    fn finish(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(FlashDevice::finish(self))
    }
    fn wp_supported(&self) -> bool {
        FlashDevice::wp_supported(self)
    }
    fn read_wp_config(&mut self) -> BoxFuture<'_, WpResult<WpConfig>> {
        Box::pin(FlashDevice::read_wp_config(self))
    }
    fn write_wp_config<'a>(
        &'a mut self,
        config: &'a WpConfig,
        options: WriteOptions,
    ) -> BoxFuture<'a, WpResult<()>> {
        Box::pin(FlashDevice::write_wp_config(self, config, options))
    }
    fn set_wp_mode(&mut self, mode: WpMode, options: WriteOptions) -> BoxFuture<'_, WpResult<()>> {
        Box::pin(FlashDevice::set_wp_mode(self, mode, options))
    }
    fn set_wp_range<'a>(
        &'a mut self,
        range: &'a WpRange,
        options: WriteOptions,
    ) -> BoxFuture<'a, WpResult<()>> {
        Box::pin(FlashDevice::set_wp_range(self, range, options))
    }
    fn disable_wp(&mut self, options: WriteOptions) -> BoxFuture<'_, WpResult<()>> {
        Box::pin(FlashDevice::disable_wp(self, options))
    }
    fn get_available_wp_ranges(&self) -> Vec<WpRange> {
        FlashDevice::get_available_wp_ranges(self)
    }
}

/// A type-erased [`FlashDevice`].
///
/// Wraps any concrete flash device behind dynamic dispatch while still
/// implementing the real async `FlashDevice` trait, so generic core
/// algorithms keep working on it.
pub struct ErasedFlashDevice {
    inner: Box<dyn DynFlashDevice + Send>,
}

impl ErasedFlashDevice {
    /// Erase a concrete flash device.
    pub fn new<D: FlashDevice + Send + 'static>(device: D) -> Self {
        Self {
            inner: Box::new(device),
        }
    }
}

impl FlashDevice for ErasedFlashDevice {
    fn size(&self) -> u32 {
        self.inner.size()
    }
    fn erase_granularity(&self) -> u32 {
        self.inner.erase_granularity()
    }
    fn write_granularity(&self) -> WriteGranularity {
        self.inner.write_granularity()
    }
    fn erase_blocks(&self) -> &[EraseBlock] {
        self.inner.erase_blocks()
    }
    fn page_size(&self) -> u32 {
        self.inner.page_size()
    }
    fn is_valid_range(&self, addr: u32, len: usize) -> bool {
        self.inner.is_valid_range(addr, len)
    }
    async fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        self.inner.read(addr, buf).await
    }
    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        self.inner.write(addr, data).await
    }
    async fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
        self.inner.erase(addr, len).await
    }
    async fn finish(&mut self) -> Result<()> {
        self.inner.finish().await
    }
    fn wp_supported(&self) -> bool {
        self.inner.wp_supported()
    }
    async fn read_wp_config(&mut self) -> WpResult<WpConfig> {
        self.inner.read_wp_config().await
    }
    async fn write_wp_config(&mut self, config: &WpConfig, options: WriteOptions) -> WpResult<()> {
        self.inner.write_wp_config(config, options).await
    }
    async fn set_wp_mode(&mut self, mode: WpMode, options: WriteOptions) -> WpResult<()> {
        self.inner.set_wp_mode(mode, options).await
    }
    async fn set_wp_range(&mut self, range: &WpRange, options: WriteOptions) -> WpResult<()> {
        self.inner.set_wp_range(range, options).await
    }
    async fn disable_wp(&mut self, options: WriteOptions) -> WpResult<()> {
        self.inner.disable_wp(options).await
    }
    fn get_available_wp_ranges(&self) -> Vec<WpRange> {
        self.inner.get_available_wp_ranges()
    }
}

// =============================================================================
// SpiMaster erasure
// =============================================================================

/// Object-safe mirror of [`SpiMaster`].
trait DynSpiMaster {
    fn features(&self) -> SpiFeatures;
    fn max_read_len(&self) -> usize;
    fn max_write_len(&self) -> usize;
    fn probe_opcode(&self, opcode: u8) -> bool;
    fn execute<'a>(&'a mut self, cmd: &'a mut SpiCommand<'_>) -> BoxFuture<'a, Result<()>>;
    fn delay_us(&mut self, us: u32) -> BoxFuture<'_, ()>;
}

impl<M: SpiMaster> DynSpiMaster for M {
    fn features(&self) -> SpiFeatures {
        SpiMaster::features(self)
    }
    fn max_read_len(&self) -> usize {
        SpiMaster::max_read_len(self)
    }
    fn max_write_len(&self) -> usize {
        SpiMaster::max_write_len(self)
    }
    fn probe_opcode(&self, opcode: u8) -> bool {
        SpiMaster::probe_opcode(self, opcode)
    }
    fn execute<'a>(&'a mut self, cmd: &'a mut SpiCommand<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(SpiMaster::execute(self, cmd))
    }
    fn delay_us(&mut self, us: u32) -> BoxFuture<'_, ()> {
        Box::pin(SpiMaster::delay_us(self, us))
    }
}

/// A type-erased [`SpiMaster`].
///
/// Used by the REPL, which selects a backend at runtime but drives it through
/// the ordinary `SpiMaster` trait.
pub struct ErasedSpiMaster {
    inner: Box<dyn DynSpiMaster + Send>,
}

impl ErasedSpiMaster {
    /// Erase a concrete SPI master.
    pub fn new<M: SpiMaster + Send + 'static>(master: M) -> Self {
        Self {
            inner: Box::new(master),
        }
    }
}

impl SpiMaster for ErasedSpiMaster {
    fn features(&self) -> SpiFeatures {
        self.inner.features()
    }
    fn max_read_len(&self) -> usize {
        self.inner.max_read_len()
    }
    fn max_write_len(&self) -> usize {
        self.inner.max_write_len()
    }
    fn probe_opcode(&self, opcode: u8) -> bool {
        self.inner.probe_opcode(opcode)
    }
    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
        self.inner.execute(cmd).await
    }
    async fn delay_us(&mut self, us: u32) {
        self.inner.delay_us(us).await
    }
}

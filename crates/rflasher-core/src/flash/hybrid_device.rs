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
use crate::flash::operations::{
    addressing_for_4byte_operation, check_erased_range_in_session, select_erase_block,
};
use crate::flash::prepare::PreparedState;
use crate::programmer::{OpaqueMaster, SpiFeatures, SpiMaster};
use crate::protocol::{self, CommandAddressing};
#[cfg(feature = "alloc")]
use crate::wp::{
    self, RangeDecoder, WpBits, WpConfig, WpMode, WpRange, WpRegBitMap, WpResult, WriteOptions,
};

/// Flash device adapter for hybrid programmers (SpiMaster + OpaqueMaster)
///
/// Uses `OpaqueMaster` for bulk read/write (fast path) and `SpiMaster` for
/// everything else (probe, erase, status registers, write protection).
///
/// # Example
///
/// ```ignore
/// use rflasher_core::flash::{HybridFlashDevice, probe};
/// use rflasher_core::chip::ChipProvider;
/// use rflasher_programmers::dediprog::Dediprog;
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
    /// Prepared session state (read op + QPI mode + 4BA state)
    prepared: PreparedState,
    /// True once `prepare()`/`set_prepared()` established session state.
    prepared_established: bool,
}

impl<M: SpiMaster + OpaqueMaster> HybridFlashDevice<M> {
    /// Create a new hybrid flash device adapter
    ///
    /// # Arguments
    /// * `master` - The programmer (must implement both SpiMaster and OpaqueMaster)
    /// * `ctx` - Flash context with chip metadata (from probing via SpiMaster)
    ///
    /// The device starts unprepared. The first read/write/erase lazily runs
    /// `prepare()` so the opaque bulk path always sees a consistent read op
    /// and 4-byte addressing state.
    pub fn new(master: M, ctx: FlashContext) -> Self {
        let prepared = PreparedState::default_for(&ctx);
        HybridFlashDevice {
            master,
            ctx,
            prepared,
            prepared_established: false,
        }
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

    /// Get a reference to the prepared session state.
    pub fn prepared(&self) -> &PreparedState {
        &self.prepared
    }

    /// Prepare the chip for multi-IO operation.
    ///
    /// For hybrid programmers (e.g. Dediprog), this also pushes the selected
    /// read op into the programmer via `set_read_op` so that the opaque
    /// bulk-read path uses multi-IO framing.
    pub async fn prepare(&mut self) -> Result<()> {
        if self.prepared_established {
            return Ok(());
        }
        if let Err(error) = crate::flash::prepare::prepare_io_in_place(
            &mut self.ctx,
            &mut self.master,
            &mut self.prepared,
        )
        .await
        {
            self.suspend_prepared_io().await?;
            return Err(error);
        }
        self.push_read_op();
        self.prepared_established = true;
        Ok(())
    }

    /// Push the prepared read op into the `OpaqueMaster` so bulk transfers
    /// use the same opcode, I/O mode, and addressing as the SPI layer.
    fn push_read_op(&mut self) {
        OpaqueMaster::set_read_op(
            &mut self.master,
            self.prepared.read_op,
            self.ctx.chip.features,
            self.prepared.read_addressing,
        );
    }

    /// Run `prepare()` once, before the first opaque (bulk) operation.
    ///
    /// The opaque path does not bracket 4-byte addressing itself, so an
    /// unprepared read/write on a >16 MiB chip would use a 3-byte address
    /// and return data from the wrong address.
    async fn ensure_prepared(&mut self) -> Result<()> {
        if !self.prepared_established {
            self.prepare().await?;
        }
        Ok(())
    }

    /// Set the prepared state directly (mostly for tests).
    ///
    /// Also pushes the read op down to the `OpaqueMaster` so the device and
    /// the programmer stay in agreement.
    pub fn set_prepared(&mut self, state: PreparedState) {
        self.prepared = state;
        self.push_read_op();
        self.prepared_established = true;
    }

    /// Undo side-effects from `prepare()`.
    pub async fn finish(&mut self) -> Result<()> {
        self.suspend_prepared_io().await
    }

    // Invalidate first, even if restoration fails. Never reuse quad after a
    // status write with uncertain completion. EN4B ownership is independent.
    async fn suspend_prepared_io(&mut self) -> Result<()> {
        self.prepared_established = false;
        let entered_4ba = self.prepared.entered_4ba;
        let mut fallback = PreparedState::default_for(&self.ctx);
        fallback.entered_4ba = entered_4ba;
        self.prepared.read_op = fallback.read_op;
        self.prepared.read_addressing = fallback.read_addressing;
        self.push_read_op();
        if self.prepared.in_qpi_mode {
            protocol::exit_qpi_with(&mut self.master, self.prepared.qpi_exit_opcode).await?;
            self.prepared.in_qpi_mode = false;
        }
        crate::flash::prepare::restore_temporary_qe(&mut self.prepared, &mut self.master).await
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

        self.ensure_prepared().await?;

        // OpaqueMaster::read handles alignment splitting internally
        OpaqueMaster::read(&mut self.master, addr, buf).await
    }

    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        let ctx = self.context();
        if !ctx.is_valid_range(addr, data.len()) {
            return Err(Error::AddressOutOfBounds);
        }

        self.ensure_prepared().await?;

        // OpaqueMaster::write handles alignment splitting internally
        OpaqueMaster::write(&mut self.master, addr, data).await
    }

    // =========================================================================
    // Erase: try OpaqueMaster first, fall back to SpiMaster
    //
    // Following flashprog's architecture: erase is a first-class operation
    // on the opaque interface. Programmers with firmware-accelerated erase
    // (e.g., SPI_CMD_SPINOR_WAIT) implement OpaqueMaster::erase(). Those
    // without (e.g., Dediprog) return Err, triggering the SPI fallback.
    // =========================================================================

    async fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
        // Bounds check (borrow ctx briefly, then drop before mutable borrow)
        if !self.context().is_valid_range(addr, len as usize) {
            return Err(Error::AddressOutOfBounds);
        }

        self.ensure_prepared().await?;

        // Try opaque erase first — the programmer handles everything internally
        // (block selection, busy-wait, etc.). If it returns Ok, we're done.
        // Programmers without firmware erase (e.g., Dediprog) return Err,
        // triggering the SPI fallback below.
        if OpaqueMaster::erase(&mut self.master, addr, len)
            .await
            .is_ok()
        {
            return Ok(());
        }

        // Opaque erase not supported — fall back to SPI-based erase.
        // NOTE: OpaqueMaster::erase cannot distinguish "unsupported" from a
        // genuine mid-erase failure; the SPI fallback retries the whole range
        // either way, which is safe for flash (erase is idempotent) but means
        // opaque hardware errors are not surfaced here.
        // SST26 chips use a per-block protection register (not SR BP bits).
        // A global unlock (WREN + ULBPR 0x98) is required before any erase
        // succeeds — same as SpiFlashDevice::erase.
        let needs_sst26_unprotect = self
            .context()
            .chip
            .features
            .contains(crate::chip::Features::SST26_BPR);
        if needs_sst26_unprotect {
            protocol::sst26_global_unprotect(&mut self.master).await?;
        }

        let ctx = self.context();
        let erase_block = select_erase_block(ctx.chip.erase_blocks(), addr, len)
            .ok_or(Error::InvalidAlignment)?;

        let chip_features = ctx.chip.features;
        let use_4byte = ctx.address_mode == AddressMode::FourByte;
        let master_features = self.master.features();
        let use_native = use_4byte
            && erase_block.opcode_4b.is_some_and(|opcode| {
                master_features.contains(SpiFeatures::FOUR_BYTE_ADDR)
                    && self.master.probe_opcode(opcode)
            });
        let opcode = erase_block.opcode_for_address_width(use_native);
        let (addressing, enter_exit_4byte) = if use_4byte {
            addressing_for_4byte_operation(use_native, chip_features, master_features)?
        } else {
            (CommandAddressing::ThreeByte, false)
        };
        let enter_exit_4byte = enter_exit_4byte && !self.prepared.entered_4ba;

        if enter_exit_4byte {
            protocol::enter_4byte_mode_with_features(self.master(), chip_features).await?;
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

            let result = protocol::erase_block(
                self.master(),
                opcode,
                current_addr,
                addressing,
                poll_delay_us,
                timeout_us,
            )
            .await;

            if result.is_err() {
                if enter_exit_4byte
                    && let Err(e) =
                        protocol::exit_4byte_mode_with_features(self.master(), chip_features).await
                {
                    log::warn!("Failed to exit 4-byte address mode: {}", e);
                }
                return result;
            }

            current_addr += block_size;
        }

        if enter_exit_4byte {
            protocol::exit_4byte_mode_with_features(self.master(), chip_features).await?;
        }

        // Single-I/O verification borrows the retained EN4B session; it must
        // not issue EX4B for an entry owned by preparation.
        check_erased_range_in_session(
            &mut self.master,
            &self.ctx,
            addr,
            len,
            self.prepared.entered_4ba,
        )
        .await
    }

    async fn finish(&mut self) -> Result<()> {
        HybridFlashDevice::finish(self).await
    }
}

// =============================================================================
// Write Protection Support (delegates to SpiMaster, identical to SpiFlashDevice)
// =============================================================================

#[cfg(feature = "alloc")]
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

    /// Write write protection bits
    pub async fn write_wp_bits(&mut self, bits: &WpBits, options: WriteOptions) -> WpResult<()> {
        self.suspend_prepared_io().await?;
        let options = wp::chip_write_options(self.ctx.chip.features, options);
        let bit_map = self.wp_bit_map();
        wp::write_wp_bits(&mut self.master, bits, &bit_map, options).await
    }

    /// Write write protection configuration
    pub async fn write_wp_config(
        &mut self,
        config: &WpConfig,
        options: WriteOptions,
    ) -> WpResult<()> {
        self.suspend_prepared_io().await?;
        let options = wp::chip_write_options(self.ctx.chip.features, options);
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
    pub async fn set_wp_mode(&mut self, mode: WpMode, options: WriteOptions) -> WpResult<()> {
        self.suspend_prepared_io().await?;
        let options = wp::chip_write_options(self.ctx.chip.features, options);
        let bit_map = self.wp_bit_map();
        wp::set_wp_mode(&mut self.master, mode, &bit_map, options).await
    }

    /// Set protected range
    pub async fn set_wp_range(&mut self, range: &WpRange, options: WriteOptions) -> WpResult<()> {
        self.suspend_prepared_io().await?;
        let options = wp::chip_write_options(self.ctx.chip.features, options);
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
    pub async fn disable_wp(&mut self, options: WriteOptions) -> WpResult<()> {
        self.suspend_prepared_io().await?;
        let options = wp::chip_write_options(self.ctx.chip.features, options);
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

#[cfg(all(test, feature = "alloc"))]
mod tests {
    use super::*;
    use crate::chip::{EraseBlock, Features, FlashChip, QeMethod, WriteGranularity};
    use crate::protocol::SpiReadOp;
    use crate::spi::SpiCommand;
    use alloc::vec;

    /// Records whether the read op was pushed before a bulk read happened.
    #[derive(Default)]
    struct MockHybrid {
        pushed_op: Option<SpiReadOp>,
        pushes: usize,
        reads: usize,
    }

    impl SpiMaster for MockHybrid {
        fn features(&self) -> SpiFeatures {
            SpiFeatures::FOUR_BYTE_ADDR
        }
        fn max_read_len(&self) -> usize {
            4096
        }
        fn max_write_len(&self) -> usize {
            4096
        }
        async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
            cmd.read_buf.fill(0xff);
            Ok(())
        }
        async fn delay_us(&mut self, _us: u32) {}
    }

    impl OpaqueMaster for MockHybrid {
        fn size(&self) -> usize {
            2 * 1024 * 1024
        }
        fn set_read_op(&mut self, op: SpiReadOp, _f: Features, _a: CommandAddressing) {
            self.pushed_op = Some(op);
            self.pushes += 1;
        }
        async fn read(&mut self, _addr: u32, buf: &mut [u8]) -> Result<()> {
            assert!(
                self.pushed_op.is_some(),
                "bulk read ran before the read op was pushed"
            );
            self.reads += 1;
            buf.fill(0xff);
            Ok(())
        }
        async fn write(&mut self, _addr: u32, _data: &[u8]) -> Result<()> {
            Ok(())
        }
        async fn erase(&mut self, _addr: u32, _len: u32) -> Result<()> {
            Err(Error::ProgrammerError)
        }
    }

    fn simple_chip() -> FlashChip {
        FlashChip {
            vendor: "Test".into(),
            name: "TestChip".into(),
            jedec_manufacturer: 0xEF,
            jedec_device: 0x4014,
            total_size: 2 * 1024 * 1024,
            page_size: 256,
            features: Features::FAST_READ,
            voltage_min_mv: 2700,
            voltage_max_mv: 3600,
            write_granularity: WriteGranularity::Page,
            erase_blocks: vec![EraseBlock::new(0x20, 4096)],
            tested: Default::default(),
            qe_method: QeMethod::None,
            dummy_cycles_112: 0,
            dummy_cycles_122: 0,
            dummy_cycles_114: 0,
            dummy_cycles_144: 0,
            dummy_cycles_qpi: 0,
        }
    }

    #[test]
    fn unprepared_read_runs_prepare_first() {
        futures_lite::future::block_on(async {
            let mut dev =
                HybridFlashDevice::new(MockHybrid::default(), FlashContext::new(simple_chip()));
            assert!(!dev.prepared_established);
            let mut buf = [0u8; 4];
            FlashDevice::read(&mut dev, 0, &mut buf).await.unwrap();
            assert!(dev.prepared_established);
            assert_eq!(dev.master.reads, 1);
            assert!(dev.master.pushed_op.is_some());
        })
    }

    #[test]
    fn prepare_is_not_repeated_for_second_read() {
        futures_lite::future::block_on(async {
            let mut dev =
                HybridFlashDevice::new(MockHybrid::default(), FlashContext::new(simple_chip()));
            let mut buf = [0u8; 4];
            FlashDevice::read(&mut dev, 0, &mut buf).await.unwrap();
            FlashDevice::read(&mut dev, 0, &mut buf).await.unwrap();
            assert_eq!(dev.master.reads, 2);
            assert_eq!(dev.master.pushes, 1, "prepare must run only once");
        })
    }
}

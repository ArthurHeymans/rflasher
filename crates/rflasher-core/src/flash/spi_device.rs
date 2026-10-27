//! SPI flash device adapter
//!
//! This module provides `SpiFlashDevice`, an adapter that implements
//! `FlashDevice` for SPI-based programmers.

use crate::chip::{EraseBlock, WriteGranularity};
use crate::error::{Error, Result};
use crate::flash::context::FlashContext;
use crate::flash::device::{EraseOutcome, FlashDevice};
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
        self.has_wp_layout()
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
        // Uses the master's read fast path, if it has one
        operations::read(&mut self.master, &self.ctx, addr, buf).await
    }

    fn validate_write(&self, addr: u32, len: usize) -> Result<()> {
        operations::validate_spi_program(&self.master, &self.ctx, addr, len)
    }

    async fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        if !self.context().is_valid_range(addr, data.len()) {
            return Err(Error::AddressOutOfBounds);
        }
        // Lifts protection around the master's program fast path or the
        // generic SPI program (AAI, byte granularity, pages, 4-byte addressing)
        operations::write(&mut self.master, &self.ctx, addr, data).await
    }

    async fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
        if !self.ctx.is_valid_range(addr, len as usize) {
            return Err(Error::AddressOutOfBounds);
        }
        // This entry point always verifies; refuse unreadable ranges before erasing.
        self.master
            .check_access(crate::programmer::FlashAccess::Read, addr, len)?;
        operations::erase_spi_range(&mut self.master, &self.ctx, addr, len).await?;
        self.check_erased_range(addr, len).await
    }

    fn validate_erase_operation(&self, op: &operations::OptimalEraseOp) -> Result<()> {
        operations::validate_spi_erase_operation(&self.master, &self.ctx, op)
    }

    fn check_access(
        &self,
        access: crate::programmer::FlashAccess,
        addr: u32,
        len: u32,
    ) -> Result<()> {
        self.master.check_access(access, addr, len)
    }

    async fn erase_operation(&mut self, op: &operations::OptimalEraseOp) -> Result<EraseOutcome> {
        if !super::protection::wps_unknown(&self.ctx) {
            operations::erase_spi_operation(&mut self.master, &self.ctx, op).await?;
            return Ok(EraseOutcome::Unverified);
        }
        // Possible WPS individual locks ignore the erase without an error, so
        // a direct caller must not see success unless the range reads erased.
        self.master
            .check_access(crate::programmer::FlashAccess::Read, op.start, op.size)?;
        operations::erase_spi_operation(&mut self.master, &self.ctx, op).await?;
        self.check_erased_range(op.start, op.size).await?;
        Ok(EraseOutcome::VerifiedErased)
    }

    /// Read the complete UID using the identified chip's supported SPI format.
    #[cfg(feature = "alloc")]
    async fn read_unique_id(&mut self) -> Result<alloc::vec::Vec<u8>> {
        let id = crate::protocol::read_unique_id(&mut self.master, &self.ctx.chip).await?;
        Ok(id.to_vec())
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
    fn has_wp_layout(&self) -> bool {
        let features = self.ctx.chip.features;
        !super::protection::wps_unknown(&self.ctx)
            && features.contains(crate::chip::Features::WP_WINBOND)
            && !features
                .intersects(crate::chip::Features::WP_UNRESOLVED | crate::chip::Features::SST26_BPR)
    }

    /// Get the WP register bit map for this chip
    ///
    /// Only used for catalog entries with a positively established Winbond map.
    fn wp_bit_map(&self) -> WpRegBitMap {
        // Check if chip has BP3 (4 BP bits)
        let features = self.ctx.chip.features;
        let mut map = if features.contains(crate::chip::Features::WP_BP3) {
            WpRegBitMap::winbond_with_bp3()
        } else {
            WpRegBitMap::winbond_standard()
        };
        map.sector_bp6_undefined = self.ctx.chip.jedec_id() == 0xef4018
            && features.contains(crate::chip::Features::EXPLICIT_PROFILE);
        if features.contains(crate::chip::Features::WP_WPS) {
            map.wps = wp::RegBitInfo::new(
                wp::StatusRegister::Status3,
                2,
                wp::BitWritability::ReadWrite,
            );
        }
        map
    }

    /// Get the range decoder for this chip
    fn wp_decoder(&self) -> RangeDecoder {
        // Default to standard SPI25 decoding
        // In the future, this could be made chip-specific
        RangeDecoder::Spi25
    }

    /// Read current write protection bits
    pub async fn read_wp_bits(&mut self) -> WpResult<WpBits> {
        if !self.has_wp_layout() {
            return Err(crate::wp::WpError::ChipUnsupported);
        }
        let bit_map = self.wp_bit_map();
        wp::read_wp_bits(&mut self.master, &bit_map).await
    }

    /// Read current write protection configuration
    pub async fn read_wp_config(&mut self) -> WpResult<WpConfig> {
        if !self.has_wp_layout() {
            return Err(crate::wp::WpError::ChipUnsupported);
        }
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
        wp::read_wp_config(&mut self.master, &bit_map, total_size, decoder).await
    }

    /// Resolve the requested persistence into a documented enable command
    /// (see [`WriteOptions::for_features`]).
    fn chip_write_options(&self, options: WriteOptions) -> WpResult<wp::ResolvedWriteOptions> {
        if !self.has_wp_layout() {
            return Err(crate::wp::WpError::ChipUnsupported);
        }
        // Explicit W25Q128 profiles read only the current registers, which a
        // 50h write may have overridden. A persistent 06h+01h write would then
        // persist unrelated volatile SR2 bits (including OTP LB) or, with a
        // volatile WPS=0 over a persistent WPS=1, be ignored after power-up.
        // No command reads the persistent values, so refuse before any SPI
        // command. Volatile writes only change the current state.
        if !options.volatile
            && self
                .ctx
                .chip
                .features
                .contains(crate::chip::Features::EXPLICIT_PROFILE)
        {
            return Err(crate::wp::WpError::PersistentStateUnknown);
        }
        options.for_features(self.ctx.chip.features)
    }

    /// Write write protection bits
    pub async fn write_wp_bits(&mut self, bits: &WpBits, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        let options = self.chip_write_options(options)?;
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
        let options = self.chip_write_options(options)?;
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
        let options = self.chip_write_options(options)?;
        wp::set_wp_mode(&mut self.master, mode, &bit_map, options).await
    }

    /// Set protected range
    pub async fn set_wp_range(&mut self, range: &WpRange, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
        let options = self.chip_write_options(options)?;
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
        let options = self.chip_write_options(options)?;
        wp::disable_wp(&mut self.master, &bit_map, options).await
    }

    /// Get all available protection ranges
    #[cfg(feature = "alloc")]
    pub fn get_available_wp_ranges(&self) -> alloc::vec::Vec<WpRange> {
        if !self.has_wp_layout() {
            return alloc::vec::Vec::new();
        }
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
        wp::get_available_ranges(&bit_map, total_size, decoder)
    }
}

#[cfg(all(test, feature = "std"))]
mod tests {
    use super::*;
    use crate::{programmer::SpiFeatures, spi::SpiCommand, wp::WpError};
    use std::{vec, vec::Vec};

    /// SR1-SR3 with persistent values and volatile (50h) overrides.
    #[derive(Default)]
    struct Registers {
        persistent: [u8; 3],
        current: [u8; 3],
        wel: bool,
        ewsr: bool,
        fail: u8,
        unreadable: bool,
        commands: Vec<(u8, Vec<u8>)>,
    }
    impl Registers {
        fn new(sr1: u8, sr2: u8, sr3: u8) -> Self {
            Self {
                persistent: [sr1, sr2, sr3],
                current: [sr1, sr2, sr3],
                ..Default::default()
            }
        }
        fn mutated(&self) -> bool {
            self.commands
                .iter()
                .any(|(op, _)| [0x01, 0x06, 0x50].contains(op))
        }
    }
    impl SpiMaster for Registers {
        fn features(&self) -> SpiFeatures {
            SpiFeatures::empty()
        }
        fn max_read_len(&self) -> usize {
            256
        }
        fn max_write_len(&self) -> usize {
            256
        }
        async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
            self.commands.push((cmd.opcode, cmd.write_data.to_vec()));
            if cmd.opcode == self.fail {
                return Err(Error::SpiTransferFailed);
            }
            match cmd.opcode {
                0x05 => cmd.read_buf.fill(self.current[0]),
                0x35 => cmd.read_buf.fill(self.current[1]),
                0x15 => cmd.read_buf.fill(self.current[2]),
                0x06 => self.wel = true,
                0x50 => self.ewsr = true,
                0x01 => {
                    for (i, value) in cmd.write_data.iter().enumerate() {
                        if self.wel {
                            self.persistent[i] = *value;
                        }
                        if self.wel || self.ewsr {
                            self.current[i] = *value;
                        }
                    }
                    (self.wel, self.ewsr) = (false, false);
                }
                _ => (),
            }
            Ok(())
        }
        fn check_access(
            &self,
            access: crate::programmer::FlashAccess,
            _: u32,
            _: u32,
        ) -> Result<()> {
            if self.unreadable && access == crate::programmer::FlashAccess::Read {
                Err(Error::RegionProtected)
            } else {
                Ok(())
            }
        }
        async fn delay_us(&mut self, _: u32) {}
    }
    fn device(name: &str, registers: Registers) -> SpiFlashDevice<Registers> {
        let db = rflasher_chips::ChipDatabase::new();
        SpiFlashDevice::new(
            registers,
            FlashContext::new(db.find_by_name(name)[0].clone()),
        )
    }
    const TEMPORARY: WriteOptions = WriteOptions { volatile: true };

    #[test]
    fn direct_erase_requires_verification_access_before_mutation() {
        futures_lite::future::block_on(async {
            for name in ["W25Q128.V", "W25Q128BV-WP"] {
                let mut dev = device(
                    name,
                    Registers {
                        unreadable: true,
                        ..Default::default()
                    },
                );
                assert_eq!(dev.erase(0, 4096).await, Err(Error::RegionProtected));
                assert!(dev.master.commands.is_empty());
            }
        });
    }

    #[test]
    fn extended_status_format_preserves_sr2_without_wp_fields_in_sr2() {
        futures_lite::future::block_on(async {
            let mut map = WpRegBitMap::winbond_standard();
            map.srl = wp::RegBitInfo::not_present();
            map.cmp = wp::RegBitInfo::not_present();
            let mut registers = Registers::new(0, 0x42, 0);
            let mut bits = WpBits::empty();
            bits.set_bp_value(1, 3);
            let options = WriteOptions::default()
                .for_features(crate::chip::Features::WRSR_WREN | crate::chip::Features::WRSR_EXT)
                .unwrap();
            wp::write_wp_bits(&mut registers, &bits, &map, options)
                .await
                .unwrap();
            assert!(registers.commands.contains(&(0x01, vec![0x04, 0x42])));
            assert_eq!(registers.current, [0x04, 0x42, 0]);
        });
    }

    #[test]
    fn variant_wp_registers_and_fail_closed() {
        futures_lite::future::block_on(async {
            let mut unknown = device("W25Q128.V", Registers::default());
            assert!(matches!(
                unknown.read_wp_config().await,
                Err(WpError::ChipUnsupported)
            ));
            assert!(unknown.master.commands.is_empty());
            for name in ["W25Q128BV-WP", "W25Q128FVJV-WP"] {
                for (sr1, sr2, range) in [
                    (0x04, 0, WpRange::new(0xfc0000, 0x40000)),
                    (0x64, 0, WpRange::new(0, 0x1000)),
                    (0x64, 0x40, WpRange::new(0x1000, 0xfff000)),
                    (0x1c, 0x40, WpRange::none()),
                ] {
                    let mut dev = device(name, Registers::new(sr1, sr2, 0));
                    let actual = dev.read_wp_config().await.unwrap().range;
                    assert_eq!(actual.len, range.len);
                    if range.len != 0 {
                        assert_eq!(actual.start, range.start);
                    }
                }
                let mut undefined = device(name, Registers::new(0x58, 0, 0));
                assert!(matches!(
                    undefined.read_wp_config().await,
                    Err(WpError::UnsupportedState)
                ));
                let mut invalid = WpBits::empty();
                invalid.sec = Some(1);
                invalid.set_bp_value(6, 3);
                assert!(matches!(
                    undefined.write_wp_bits(&invalid, TEMPORARY).await,
                    Err(WpError::UnsupportedState)
                ));
                assert!(!undefined.master.mutated());
                let mut dev = device(name, Registers::new(0, 0x42, 0));
                let mut bits = WpBits::empty();
                bits.set_bp_value(1, 3);
                dev.write_wp_bits(&bits, TEMPORARY).await.unwrap();
                assert!(dev.master.commands.contains(&(0x01, vec![0x04, 0x42])));
                assert_eq!(
                    dev.master.commands.iter().any(|(op, _)| *op == 0x15),
                    name.contains("FVJV")
                );
                for fail in [0x05, 0x35, 0x15] {
                    if fail == 0x15 && !name.contains("FVJV") {
                        continue;
                    }
                    let mut dev = device(
                        name,
                        Registers {
                            fail,
                            ..Default::default()
                        },
                    );
                    assert!(dev.write_wp_bits(&bits, TEMPORARY).await.is_err());
                    assert!(!dev.master.mutated());
                }
            }
            let mut dev = device("W25Q128FVJV-WP", Registers::new(0, 0, 4));
            assert!(matches!(
                dev.disable_wp(TEMPORARY).await,
                Err(WpError::UnsupportedState)
            ));
            assert!(!dev.master.mutated());
        });
    }

    #[test]
    fn explicit_profiles_change_only_the_volatile_state() {
        futures_lite::future::block_on(async {
            let persistent = WriteOptions::default();
            for name in ["W25Q128BV-WP", "W25Q128FVJV-WP"] {
                // Persistent WPS=1 and unrelated SR2 bits overridden by 50h.
                let registers = || Registers {
                    persistent: [0x1c, 0x00, 0x04],
                    current: [0x1c, 0x0a, 0x00],
                    ..Default::default()
                };
                let mut dev = device(name, registers());
                let mut bits = WpBits::empty();
                bits.set_bp_value(1, 3);
                let config = WpConfig::new(WpMode::Disabled, WpRange::none());
                let refused = [
                    dev.disable_wp(persistent).await,
                    dev.write_wp_bits(&bits, persistent).await,
                    dev.set_wp_mode(WpMode::Hardware, persistent).await,
                    dev.set_wp_range(&WpRange::none(), persistent).await,
                    dev.write_wp_config(&config, persistent).await,
                ];
                assert!(
                    refused
                        .iter()
                        .all(|r| matches!(r, Err(WpError::PersistentStateUnknown)))
                );
                assert!(dev.master.commands.is_empty());

                let mut dev = device(name, registers());
                dev.disable_wp(TEMPORARY).await.unwrap();
                assert_eq!(dev.master.current, [0x00, 0x0a, 0x00]);
                assert_eq!(dev.master.persistent, [0x1c, 0x00, 0x04]);
                assert!(!dev.master.commands.iter().any(|(op, _)| *op == 0x06));
            }
        });
    }
}

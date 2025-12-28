//! SPI flash device adapter
//!
//! This module provides `SpiFlashDevice`, an adapter that implements
//! `FlashDevice` for SPI-based programmers.

use crate::chip::{EraseBlock, WriteGranularity};
use crate::error::{Error, Result};
use crate::flash::context::{AddressMode, FlashContext};
use crate::flash::device::FlashDevice;
use crate::programmer::SpiMaster;
use crate::protocol;
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

    // Write protection support
    #[cfg(feature = "alloc")]
    fn wp_supported(&self) -> bool {
        true
    }

    #[cfg(feature = "alloc")]
    fn read_wp_config(&mut self) -> WpResult<WpConfig> {
        SpiFlashDevice::read_wp_config(self)
    }

    #[cfg(feature = "alloc")]
    fn write_wp_config(&mut self, config: &WpConfig, options: WriteOptions) -> WpResult<()> {
        SpiFlashDevice::write_wp_config(self, config, options)
    }

    #[cfg(feature = "alloc")]
    fn set_wp_mode(&mut self, mode: WpMode, options: WriteOptions) -> WpResult<()> {
        SpiFlashDevice::set_wp_mode(self, mode, options)
    }

    #[cfg(feature = "alloc")]
    fn set_wp_range(&mut self, range: &WpRange, options: WriteOptions) -> WpResult<()> {
        SpiFlashDevice::set_wp_range(self, range, options)
    }

    #[cfg(feature = "alloc")]
    fn disable_wp(&mut self, options: WriteOptions) -> WpResult<()> {
        SpiFlashDevice::disable_wp(self, options)
    }

    #[cfg(feature = "alloc")]
    fn get_available_wp_ranges(&self) -> alloc::vec::Vec<WpRange> {
        SpiFlashDevice::get_available_wp_ranges(self)
    }

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        let ctx = self.context();
        if !ctx.is_valid_range(addr, buf.len()) {
            return Err(Error::AddressOutOfBounds);
        }

        match ctx.address_mode {
            AddressMode::ThreeByte => protocol::read_3b(self.master(), addr, buf),
            AddressMode::FourByte => {
                if ctx.use_native_4byte {
                    protocol::read_4b(self.master(), addr, buf)
                } else {
                    // Enter 4-byte mode, read, exit
                    protocol::enter_4byte_mode(self.master())?;
                    let result = protocol::read_3b(self.master(), addr, buf);
                    let _ = protocol::exit_4byte_mode(self.master());
                    result
                }
            }
        }
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        let ctx = self.context();
        if !ctx.is_valid_range(addr, data.len()) {
            return Err(Error::AddressOutOfBounds);
        }

        let page_size = ctx.page_size();
        let use_4byte = ctx.address_mode == AddressMode::FourByte;
        let use_native = ctx.use_native_4byte;

        // Get the master's maximum write length - some controllers have limits
        // smaller than a full page (e.g., Intel swseq is limited to 64 bytes)
        let max_write = self.master().max_write_len();

        // Enter 4-byte mode if needed and not using native commands
        if use_4byte && !use_native {
            protocol::enter_4byte_mode(self.master())?;
        }

        let mut offset = 0usize;
        let mut current_addr = addr;

        while offset < data.len() {
            // Calculate how many bytes until the next page boundary
            let page_offset = (current_addr as usize) % page_size;
            let bytes_to_page_end = page_size - page_offset;
            let remaining = data.len() - offset;
            // Respect both page boundaries and the master's maximum write length
            let chunk_size =
                core::cmp::min(core::cmp::min(bytes_to_page_end, remaining), max_write);

            let chunk = &data[offset..offset + chunk_size];

            // Program timeout: typical page program time is 0.7-3ms
            let timeout_us = 10_000; // 10ms

            let result = if use_4byte && use_native {
                protocol::program_page_4b(self.master(), current_addr, chunk, timeout_us)
            } else {
                protocol::program_page_3b(self.master(), current_addr, chunk, timeout_us)
            };

            if result.is_err() {
                // Try to exit 4-byte mode before returning error
                if use_4byte && !use_native {
                    let _ = protocol::exit_4byte_mode(self.master());
                }
                return result;
            }

            offset += chunk_size;
            current_addr += chunk_size as u32;
        }

        // Exit 4-byte mode if we entered it
        if use_4byte && !use_native {
            protocol::exit_4byte_mode(self.master())?;
        }

        Ok(())
    }

    fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
        let ctx = self.context();
        if !ctx.is_valid_range(addr, len as usize) {
            return Err(Error::AddressOutOfBounds);
        }

        // Find the best erase block size for this operation
        let erase_block = select_erase_block(ctx.chip.erase_blocks(), addr, len)
            .ok_or(Error::InvalidAlignment)?;

        let use_4byte = ctx.address_mode == AddressMode::FourByte;
        let use_native = ctx.use_native_4byte;

        // Map 3-byte opcode to 4-byte opcode if needed
        let opcode = if use_4byte && use_native {
            map_to_4byte_erase_opcode(erase_block.opcode)
        } else {
            erase_block.opcode
        };

        // Enter 4-byte mode if needed
        if use_4byte && !use_native {
            protocol::enter_4byte_mode(self.master())?;
        }

        let mut current_addr = addr;
        let end_addr = addr + len;

        // For non-uniform erase blocks, use the maximum block size for timeout calculation
        let max_block_size = erase_block.max_block_size();

        // Erase timeout depends on block size
        let timeout_us = match max_block_size {
            s if s <= 4096 => 500_000,    // 4KB: 500ms
            s if s <= 32768 => 1_000_000, // 32KB: 1s
            s if s <= 65536 => 2_000_000, // 64KB: 2s
            _ => 60_000_000,              // Chip erase: 60s
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
                use_4byte && use_native,
                timeout_us,
            );

            if result.is_err() {
                if use_4byte && !use_native {
                    let _ = protocol::exit_4byte_mode(self.master());
                }
                return result;
            }

            // Verify the block was erased
            if let Err(e) = self.check_erased_range(current_addr, block_size) {
                if use_4byte && !use_native {
                    let _ = protocol::exit_4byte_mode(self.master());
                }
                return Err(e);
            }

            current_addr += block_size;
        }

        // Exit 4-byte mode
        if use_4byte && !use_native {
            protocol::exit_4byte_mode(self.master())?;
        }

        Ok(())
    }
}

impl<M: SpiMaster> SpiFlashDevice<M> {
    /// Check that a range of flash has been erased (all bytes are 0xFF)
    fn check_erased_range(&mut self, addr: u32, len: u32) -> Result<()> {
        const ERASED_VALUE: u8 = 0xFF;
        const CHUNK_SIZE: usize = 4096;
        let mut buf = [0u8; CHUNK_SIZE];

        let mut offset = 0u32;
        while offset < len {
            let chunk_len = core::cmp::min(CHUNK_SIZE as u32, len - offset) as usize;
            let chunk_buf = &mut buf[..chunk_len];

            self.read(addr + offset, chunk_buf)?;

            if !chunk_buf.iter().all(|&b| b == ERASED_VALUE) {
                return Err(Error::EraseError);
            }

            offset += chunk_len as u32;
        }

        Ok(())
    }
}

/// Select the best erase block size for the given operation
fn select_erase_block(erase_blocks: &[EraseBlock], addr: u32, len: u32) -> Option<EraseBlock> {
    erase_blocks
        .iter()
        .filter(|eb| {
            // Skip chip erase for partial operations
            // For non-uniform layouts, check the total coverage
            eb.total_size() <= len
        })
        .filter(|eb| {
            // For uniform blocks, check alignment
            // For non-uniform blocks, we need the min block size for alignment
            let min_size = eb.min_block_size();
            addr.is_multiple_of(min_size) && len.is_multiple_of(min_size)
        })
        .max_by_key(|eb| eb.max_block_size())
        .copied()
}

/// Map a 3-byte erase opcode to its 4-byte equivalent
fn map_to_4byte_erase_opcode(opcode: u8) -> u8 {
    use crate::spi::opcodes;
    match opcode {
        opcodes::SE_20 => opcodes::SE_21,
        opcodes::BE_52 => opcodes::BE_5C,
        opcodes::BE_D8 => opcodes::BE_DC,
        _ => opcode, // Chip erase doesn't need address
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
    pub fn read_wp_bits(&mut self) -> WpResult<WpBits> {
        let bit_map = self.wp_bit_map();
        wp::read_wp_bits(&mut self.master, &bit_map)
    }

    /// Read current write protection configuration
    pub fn read_wp_config(&mut self) -> WpResult<WpConfig> {
        let bit_map = self.wp_bit_map();
        let decoder = self.wp_decoder();
        let total_size = self.ctx.chip.total_size;
        wp::read_wp_config(&mut self.master, &bit_map, total_size, decoder)
    }

    /// Write write protection bits
    pub fn write_wp_bits(&mut self, bits: &WpBits, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        wp::write_wp_bits(&mut self.master, bits, &bit_map, options)
    }

    /// Write write protection configuration
    pub fn write_wp_config(&mut self, config: &WpConfig, options: WriteOptions) -> WpResult<()> {
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
    }

    /// Set write protection mode
    pub fn set_wp_mode(&mut self, mode: WpMode, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        wp::set_wp_mode(&mut self.master, mode, &bit_map, options)
    }

    /// Set protected range
    pub fn set_wp_range(&mut self, range: &WpRange, options: WriteOptions) -> WpResult<()> {
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
    }

    /// Disable write protection
    pub fn disable_wp(&mut self, options: WriteOptions) -> WpResult<()> {
        let bit_map = self.wp_bit_map();
        wp::disable_wp(&mut self.master, &bit_map, options)
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

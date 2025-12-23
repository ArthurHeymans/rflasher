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
///
/// fn read_flash<M: SpiMaster>(master: &mut M, db: &ChipDatabase) {
///     let ctx = probe(master, db).unwrap();
///     let mut device = SpiFlashDevice::new(master, ctx);
///
///     let mut buf = [0u8; 4096];
///     device.read(0, &mut buf).unwrap();
/// }
/// ```
pub struct SpiFlashDevice<'a, M: SpiMaster + ?Sized> {
    master: &'a mut M,
    ctx: FlashContext,
}

impl<'a, M: SpiMaster + ?Sized> SpiFlashDevice<'a, M> {
    /// Create a new SPI flash device adapter
    ///
    /// # Arguments
    /// * `master` - The SPI master to use for communication
    /// * `ctx` - Flash context with chip metadata (from probing)
    pub fn new(master: &'a mut M, ctx: FlashContext) -> Self {
        Self { master, ctx }
    }

    /// Get a reference to the underlying SPI master
    pub fn master(&mut self) -> &mut M {
        self.master
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

impl<M: SpiMaster + ?Sized> FlashDevice for SpiFlashDevice<'_, M> {
    fn size(&self) -> u32 {
        self.ctx.total_size() as u32
    }

    fn erase_granularity(&self) -> u32 {
        self.ctx.chip.min_erase_size().unwrap_or(4096) // Default to 4KB if no erase blocks defined
    }

    fn write_granularity(&self) -> WriteGranularity {
        self.ctx.chip.write_granularity
    }

    fn erase_blocks(&self) -> &[EraseBlock] {
        self.ctx.chip.erase_blocks()
    }

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        if !self.ctx.is_valid_range(addr, buf.len()) {
            return Err(Error::AddressOutOfBounds);
        }

        match self.ctx.address_mode {
            AddressMode::ThreeByte => protocol::read_3b(self.master, addr, buf),
            AddressMode::FourByte => {
                if self.ctx.use_native_4byte {
                    protocol::read_4b(self.master, addr, buf)
                } else {
                    // Enter 4-byte mode, read, exit
                    protocol::enter_4byte_mode(self.master)?;
                    let result = protocol::read_3b(self.master, addr, buf);
                    let _ = protocol::exit_4byte_mode(self.master);
                    result
                }
            }
        }
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        if !self.ctx.is_valid_range(addr, data.len()) {
            return Err(Error::AddressOutOfBounds);
        }

        let page_size = self.ctx.page_size();
        let use_4byte = self.ctx.address_mode == AddressMode::FourByte;
        let use_native = self.ctx.use_native_4byte;

        // Get the master's maximum write length - some controllers have limits
        // smaller than a full page (e.g., Intel swseq is limited to 64 bytes)
        let max_write = self.master.max_write_len();

        // Enter 4-byte mode if needed and not using native commands
        if use_4byte && !use_native {
            protocol::enter_4byte_mode(self.master)?;
        }

        let mut offset = 0usize;
        let mut current_addr = addr;

        while offset < data.len() {
            // Calculate how many bytes until the next page boundary
            let page_offset = (current_addr as usize) % page_size;
            let bytes_to_page_end = page_size - page_offset;
            let remaining = data.len() - offset;
            // Respect both page boundaries and the master's maximum write length
            let chunk_size = core::cmp::min(core::cmp::min(bytes_to_page_end, remaining), max_write);

            let chunk = &data[offset..offset + chunk_size];

            // Program timeout: typical page program time is 0.7-3ms
            let timeout_us = 10_000; // 10ms

            let result = if use_4byte && use_native {
                protocol::program_page_4b(self.master, current_addr, chunk, timeout_us)
            } else {
                protocol::program_page_3b(self.master, current_addr, chunk, timeout_us)
            };

            if result.is_err() {
                // Try to exit 4-byte mode before returning error
                if use_4byte && !use_native {
                    let _ = protocol::exit_4byte_mode(self.master);
                }
                return result;
            }

            offset += chunk_size;
            current_addr += chunk_size as u32;
        }

        // Exit 4-byte mode if we entered it
        if use_4byte && !use_native {
            protocol::exit_4byte_mode(self.master)?;
        }

        Ok(())
    }

    fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
        if !self.ctx.is_valid_range(addr, len as usize) {
            return Err(Error::AddressOutOfBounds);
        }

        // Find the best erase block size for this operation
        let erase_block = select_erase_block(self.ctx.chip.erase_blocks(), addr, len)
            .ok_or(Error::InvalidAlignment)?;

        let use_4byte = self.ctx.address_mode == AddressMode::FourByte;
        let use_native = self.ctx.use_native_4byte;

        // Map 3-byte opcode to 4-byte opcode if needed
        let opcode = if use_4byte && use_native {
            map_to_4byte_erase_opcode(erase_block.opcode)
        } else {
            erase_block.opcode
        };

        // Enter 4-byte mode if needed
        if use_4byte && !use_native {
            protocol::enter_4byte_mode(self.master)?;
        }

        let mut current_addr = addr;
        let end_addr = addr + len;

        // Erase timeout depends on block size
        let timeout_us = match erase_block.size {
            s if s <= 4096 => 500_000,    // 4KB: 500ms
            s if s <= 32768 => 1_000_000, // 32KB: 1s
            s if s <= 65536 => 2_000_000, // 64KB: 2s
            _ => 60_000_000,              // Chip erase: 60s
        };

        while current_addr < end_addr {
            let result = protocol::erase_block(
                self.master,
                opcode,
                current_addr,
                use_4byte && use_native,
                timeout_us,
            );

            if result.is_err() {
                if use_4byte && !use_native {
                    let _ = protocol::exit_4byte_mode(self.master);
                }
                return result;
            }

            // Verify the block was erased
            if let Err(e) = self.check_erased_range(current_addr, erase_block.size) {
                if use_4byte && !use_native {
                    let _ = protocol::exit_4byte_mode(self.master);
                }
                return Err(e);
            }

            current_addr += erase_block.size;
        }

        // Exit 4-byte mode
        if use_4byte && !use_native {
            protocol::exit_4byte_mode(self.master)?;
        }

        Ok(())
    }
}

impl<M: SpiMaster + ?Sized> SpiFlashDevice<'_, M> {
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

            for &byte in chunk_buf.iter() {
                if byte != ERASED_VALUE {
                    return Err(Error::EraseError);
                }
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
        .filter(|eb| eb.size <= len)
        .filter(|eb| addr.is_multiple_of(eb.size) && len.is_multiple_of(eb.size))
        .max_by_key(|eb| eb.size)
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

//! High-level flash operations

#[cfg(feature = "alloc")]
use crate::chip::ChipDatabase;
use crate::chip::EraseBlock;
use crate::error::{Error, Result};
use crate::programmer::SpiMaster;
use crate::protocol;

use super::context::{AddressMode, FlashContext};

/// Probe for a flash chip using a chip database and return a context if found
#[cfg(feature = "alloc")]
pub fn probe<M: SpiMaster>(master: &mut M, db: &ChipDatabase) -> Result<FlashContext> {
    let (manufacturer, device) = protocol::read_jedec_id(master)?;

    let chip = db
        .find_by_jedec_id(manufacturer, device)
        .ok_or(Error::ChipNotFound)?
        .clone();

    Ok(FlashContext::new(chip))
}

/// Read the JEDEC ID from the flash chip
///
/// Returns (manufacturer_id, device_id) tuple.
pub fn read_jedec_id<M: SpiMaster>(master: &mut M) -> Result<(u8, u16)> {
    protocol::read_jedec_id(master)
}

/// Read flash contents
pub fn read<M: SpiMaster>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    if !ctx.is_valid_range(addr, buf.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    match ctx.address_mode {
        AddressMode::ThreeByte => protocol::read_3b(master, addr, buf),
        AddressMode::FourByte => {
            if ctx.use_native_4byte {
                protocol::read_4b(master, addr, buf)
            } else {
                // Enter 4-byte mode, read, exit
                protocol::enter_4byte_mode(master)?;
                let result = protocol::read_3b(master, addr, buf);
                let _ = protocol::exit_4byte_mode(master);
                result
            }
        }
    }
}

/// Write data to flash
///
/// This function handles page alignment and splitting large writes
/// into page-sized chunks. The target region must be erased first.
pub fn write<M: SpiMaster>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    data: &[u8],
) -> Result<()> {
    if !ctx.is_valid_range(addr, data.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    let page_size = ctx.page_size();
    let use_4byte = ctx.address_mode == AddressMode::FourByte;
    let use_native = ctx.use_native_4byte;

    // Enter 4-byte mode if needed and not using native commands
    if use_4byte && !use_native {
        protocol::enter_4byte_mode(master)?;
    }

    let mut offset = 0usize;
    let mut current_addr = addr;

    while offset < data.len() {
        // Calculate how many bytes until the next page boundary
        let page_offset = (current_addr as usize) % page_size;
        let bytes_to_page_end = page_size - page_offset;
        let remaining = data.len() - offset;
        let chunk_size = core::cmp::min(bytes_to_page_end, remaining);

        let chunk = &data[offset..offset + chunk_size];

        // Program timeout: typical page program time is 0.7-3ms
        let timeout_us = 10_000; // 10ms

        let result = if use_4byte && use_native {
            protocol::program_page_4b(master, current_addr, chunk, timeout_us)
        } else {
            protocol::program_page_3b(master, current_addr, chunk, timeout_us)
        };

        if result.is_err() {
            // Try to exit 4-byte mode before returning error
            if use_4byte && !use_native {
                let _ = protocol::exit_4byte_mode(master);
            }
            return result;
        }

        offset += chunk_size;
        current_addr += chunk_size as u32;
    }

    // Exit 4-byte mode if we entered it
    if use_4byte && !use_native {
        protocol::exit_4byte_mode(master)?;
    }

    Ok(())
}

/// Erase a region of flash
///
/// The region must be aligned to erase block boundaries.
pub fn erase<M: SpiMaster>(master: &mut M, ctx: &FlashContext, addr: u32, len: u32) -> Result<()> {
    if !ctx.is_valid_range(addr, len as usize) {
        return Err(Error::AddressOutOfBounds);
    }

    // Find the best erase block size for this operation
    let erase_block =
        select_erase_block(ctx.chip.erase_blocks(), addr, len).ok_or(Error::InvalidAlignment)?;

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
        protocol::enter_4byte_mode(master)?;
    }

    let mut current_addr = addr;
    let end_addr = addr + len;

    // Erase timeout depends on block size (larger blocks take longer)
    let timeout_us = match erase_block.size {
        s if s <= 4096 => 500_000,    // 4KB: 500ms
        s if s <= 32768 => 1_000_000, // 32KB: 1s
        s if s <= 65536 => 2_000_000, // 64KB: 2s
        _ => 60_000_000,              // Chip erase: 60s
    };

    while current_addr < end_addr {
        let result = protocol::erase_block(
            master,
            opcode,
            current_addr,
            use_4byte && use_native,
            timeout_us,
        );

        if result.is_err() {
            if use_4byte && !use_native {
                let _ = protocol::exit_4byte_mode(master);
            }
            return result;
        }

        current_addr += erase_block.size;
    }

    // Exit 4-byte mode
    if use_4byte && !use_native {
        protocol::exit_4byte_mode(master)?;
    }

    Ok(())
}

/// Erase the entire chip
pub fn chip_erase<M: SpiMaster>(master: &mut M, _ctx: &FlashContext) -> Result<()> {
    // Chip erase timeout: up to 2 minutes for large chips
    let timeout_us = 120_000_000;
    protocol::chip_erase(master, timeout_us)
}

/// Verify flash contents match the provided data
pub fn verify<M: SpiMaster>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    expected: &[u8],
    buf: &mut [u8],
) -> Result<()> {
    if !ctx.is_valid_range(addr, expected.len()) {
        return Err(Error::AddressOutOfBounds);
    }

    if buf.len() < expected.len() {
        return Err(Error::BufferTooSmall);
    }

    let verify_buf = &mut buf[..expected.len()];
    read(master, ctx, addr, verify_buf)?;

    if verify_buf != expected {
        return Err(Error::VerifyError);
    }

    Ok(())
}

/// Select the best erase block size for the given operation
fn select_erase_block(erase_blocks: &[EraseBlock], addr: u32, len: u32) -> Option<EraseBlock> {
    // Find the largest block size that:
    // 1. Evenly divides the length
    // 2. The address is aligned to

    erase_blocks
        .iter()
        .filter(|eb| {
            // Skip chip erase for partial operations
            eb.size <= len
        })
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

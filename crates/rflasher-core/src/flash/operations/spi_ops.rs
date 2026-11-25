//! Low-level SPI read, program, erase, and preflight validation operations.

use crate::chip::{EraseBlock, Features, WriteGranularity};
use crate::error::{EraseFailure, Error, Result};
use crate::flash::context::{AddressMode, FlashContext};
use crate::flash::protection;
use crate::programmer::{FlashAccess, SpiFeatures, SpiMaster};
use crate::protocol::{self, CommandAddressing};

use super::erase_plan::OptimalEraseOp;

pub(crate) fn compatible_4byte_addressing(
    chip_features: Features,
    master_features: SpiFeatures,
) -> Result<(CommandAddressing, bool)> {
    if master_features.contains(SpiFeatures::NO_4BA_MODES) {
        return Err(Error::ChipNotSupported);
    }

    if master_features.contains(SpiFeatures::FOUR_BYTE_ADDR)
        && chip_features.supports_4ba_mode_switch()
    {
        return Ok((CommandAddressing::FourByte, true));
    }

    if chip_features.supports_extended_address_register() {
        return Ok((
            CommandAddressing::ExtendedAddressRegister(chip_features),
            false,
        ));
    }

    Err(Error::ChipNotSupported)
}

pub(crate) fn addressing_for_4byte_operation(
    native_4byte: bool,
    chip_features: Features,
    master_features: SpiFeatures,
) -> Result<(CommandAddressing, bool)> {
    if native_4byte {
        Ok((CommandAddressing::FourByte, false))
    } else {
        compatible_4byte_addressing(chip_features, master_features)
    }
}

/// Read flash contents through the programmer's fastest read path
/// ([`SpiMaster::read_flash`]).
pub async fn read<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    if !ctx.is_valid_range(addr, buf.len()) {
        return Err(Error::AddressOutOfBounds);
    }
    master.read_flash(ctx, addr, buf).await
}

/// Read flash contents with generic SPI read commands.
///
/// Uses single-I/O reads because chip capabilities do not describe the exact
/// multi-I/O opcode, mode clocks, or quad-enable state. This is the default
/// [`SpiMaster::read_flash`] and the fallback for programmer fast paths.
pub async fn spi_read<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    if !ctx.is_valid_range(addr, buf.len()) {
        return Err(Error::AddressOutOfBounds);
    }
    let features = ctx.chip.features;
    let master_features = master.features();
    let native_4byte = ctx.address_mode == AddressMode::FourByte
        && features.supports_4ba_read()
        && master_features.contains(SpiFeatures::FOUR_BYTE_ADDR)
        && master.probe_opcode(crate::spi::opcodes::READ_4B);
    let opcode = if native_4byte {
        crate::spi::opcodes::READ_4B
    } else {
        crate::spi::opcodes::READ
    };

    let (addressing, enter_exit_4byte) = if ctx.address_mode == AddressMode::FourByte {
        addressing_for_4byte_operation(native_4byte, features, master_features)?
    } else {
        (CommandAddressing::ThreeByte, false)
    };

    if enter_exit_4byte {
        protocol::enter_4byte_mode_with_features(master, features).await?;
    }

    let result = protocol::read_with_addressing(master, opcode, addr, buf, addressing).await;

    if enter_exit_4byte {
        let exit_result = protocol::exit_4byte_mode_with_features(master, features).await;
        if result.is_ok() {
            exit_result?;
        } else if let Err(e) = exit_result {
            log::warn!(
                "Failed to exit 4-byte address mode after read failure: {}",
                e
            );
        }
    }

    result
}

/// Write data to flash
///
/// Validates that the generic SPI protocol could program the range, then
/// temporarily lifts chip protection around [`SpiMaster::program_flash`].
/// The target region must be erased first. On the generic W25Q128.V profile,
/// whose possible WPS individual locks ignore programs without an error, the
/// range is read back and a mismatch is reported as [`Error::VerifyError`].
pub async fn write<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    data: &[u8],
) -> Result<()> {
    validate_spi_program(master, ctx, addr, data.len())?;
    if data.is_empty() {
        return Ok(());
    }
    // In range, so the length fits. Refuse before the first page, not after.
    master.check_access(FlashAccess::Modify, addr, data.len() as u32)?;
    let read_back = protection::needs_read_back(master, ctx);
    if read_back {
        master.check_access(FlashAccess::Read, addr, data.len() as u32)?;
    }
    let saved = protection::prepare(master, ctx).await?;
    let result = match master.program_flash(ctx, addr, data).await {
        Ok(()) => protection::check_result(master, ctx, addr, false).await,
        Err(error) => Err(error),
    };
    protection::finish(
        master,
        ctx,
        saved,
        result,
        (protocol::WRSR_POLL_US, protocol::WRSR_TIMEOUT_US),
    )
    .await?;
    if read_back {
        check_programmed_range(master, ctx, addr, data).await?;
    }
    Ok(())
}

/// Read `data`'s range back and report the first byte that differs.
async fn check_programmed_range<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    data: &[u8],
) -> Result<()> {
    const CHUNK_SIZE: usize = 4096;
    let mut buf = [0u8; CHUNK_SIZE];
    for (i, expected) in data.chunks(CHUNK_SIZE).enumerate() {
        let chunk_addr = addr + (i * CHUNK_SIZE) as u32;
        let actual = &mut buf[..expected.len()];
        read(master, ctx, chunk_addr, actual).await?;
        if let Some(pos) = actual.iter().zip(expected).position(|(a, b)| a != b) {
            let addr = chunk_addr + pos as u32;
            log::error!(
                "Program at 0x{addr:08X} was ignored or incomplete; WPS individual \
                 locks may protect it. Select W25Q128BV-WP or W25Q128FVJV-WP if the marking is known"
            );
            return Err(Error::VerifyError {
                addr,
                expected: expected[pos],
                found: actual[pos],
            });
        }
    }
    Ok(())
}

fn check_mutation_opcodes<M: SpiMaster + ?Sized>(master: &M) -> Result<()> {
    if [crate::spi::opcodes::WREN, crate::spi::opcodes::RDSR]
        .into_iter()
        .all(|opcode| master.probe_opcode(opcode))
    {
        Ok(())
    } else {
        Err(Error::OpcodeNotSupported)
    }
}

/// Whether `master` can program and erase the chip at all, judged without
/// sending a command (see [`super::SpiFlashDevice::check_writable`]).
pub(crate) fn check_spi_writable<M: SpiMaster + ?Sized>(
    master: &M,
    ctx: &FlashContext,
) -> Result<()> {
    // A word-aligned pair: what every program procedure can write, including
    // AAI on a programmer without the byte-program opcode.
    validate_spi_program(master, ctx, 0, 2)?;
    let erasable = ctx
        .chip
        .erase_blocks()
        .iter()
        .any(|block| !block.is_chip_erase() && spi_eraser_usable(master, ctx, block));
    if erasable {
        Ok(())
    } else {
        Err(Error::OpcodeNotSupported)
    }
}

pub(crate) fn validate_spi_program<M: SpiMaster + ?Sized>(
    master: &M,
    ctx: &FlashContext,
    addr: u32,
    len: usize,
) -> Result<()> {
    if !ctx.is_valid_range(addr, len) {
        return Err(Error::AddressOutOfBounds);
    }
    if len == 0 {
        return Ok(());
    }
    check_mutation_opcodes(master)?;
    protection::check_supported(master, ctx)?;
    if ctx.page_size() == 0 {
        return Err(Error::ChipNotSupported);
    }
    if master.max_write_len() == 0 {
        return Err(Error::ProgrammerError);
    }
    let native = ctx.address_mode == AddressMode::FourByte
        && ctx.chip.features.supports_4ba_program()
        && master.features().contains(SpiFeatures::FOUR_BYTE_ADDR)
        && master.probe_opcode(crate::spi::opcodes::PP_4B);
    if ctx.address_mode == AddressMode::FourByte {
        addressing_for_4byte_operation(native, ctx.chip.features, master.features())?;
    }
    let aai = use_aai(master, ctx);
    if aai {
        let leading_byte = usize::from(!addr.is_multiple_of(2));
        let remaining = len - leading_byte;
        let has_words = remaining >= 2;
        let has_bytes = leading_byte != 0 || !remaining.is_multiple_of(2);
        if (has_words
            && (!master.probe_opcode(crate::spi::opcodes::AAI_WP)
                || !master.probe_opcode(crate::spi::opcodes::WRDI)))
            || (has_bytes && !master.probe_opcode(crate::spi::opcodes::PP))
        {
            return Err(Error::OpcodeNotSupported);
        }
    } else if !master.probe_opcode(if native {
        crate::spi::opcodes::PP_4B
    } else {
        crate::spi::opcodes::PP
    }) {
        return Err(Error::OpcodeNotSupported);
    }
    Ok(())
}

fn use_aai<M: SpiMaster + ?Sized>(master: &M, ctx: &FlashContext) -> bool {
    ctx.chip.features.contains(Features::AAI_WORD)
        && master.max_write_len() >= 2
        && master.probe_opcode(crate::spi::opcodes::AAI_WP)
        && master.probe_opcode(crate::spi::opcodes::WRDI)
}

/// Program already-erased flash with generic SPI page-program commands.
///
/// Handles page alignment and splits writes into chunks that respect the
/// programmer's `max_write_len()` (e.g., Intel swseq is limited to 64 bytes).
/// This is the default [`SpiMaster::program_flash`] and the fallback for
/// programmer fast paths; it does not lift chip protection, so call
/// [`write`] for a complete write.
pub async fn spi_program<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    data: &[u8],
) -> Result<()> {
    validate_spi_program(master, ctx, addr, data.len())?;
    if data.is_empty() {
        return Ok(());
    }
    let features = ctx.chip.features;
    let write_granularity = ctx.chip.write_granularity;
    let page_size = ctx.page_size();

    // SST25 AAI word program: chip database sets AAI_WORD for SST25VFxxxB/SST25WFxxx.
    // These chips require a streaming protocol (0xAD) rather than page program (0x02).
    // AAI uses 3-byte addressing only — 4-byte mode is irrelevant for SST25 chips.
    // Note: SFDP-probed chips may report WriteGranularity::Byte (BFPT DWORD1 bit[2]=0)
    // without AAI_WORD being set; those fall through to single-byte page program below.
    // Fall back to single-byte page program when the master cannot transfer
    // two bytes or its opcode menu cannot execute and terminate AAI.
    if use_aai(master, ctx) {
        return protocol::aai_word_program(master, addr, data).await;
    }

    let use_4byte = ctx.address_mode == AddressMode::FourByte;
    let master_features = master.features();
    let use_native = use_4byte
        && features.supports_4ba_program()
        && master_features.contains(SpiFeatures::FOUR_BYTE_ADDR)
        && master.probe_opcode(crate::spi::opcodes::PP_4B);
    let (addressing, enter_exit_4byte) = if use_4byte {
        addressing_for_4byte_operation(use_native, features, master_features)?
    } else {
        (CommandAddressing::ThreeByte, false)
    };
    let opcode = if use_native {
        crate::spi::opcodes::PP_4B
    } else {
        crate::spi::opcodes::PP
    };

    // Get the master's maximum write length - some controllers have limits
    // smaller than a full page (e.g., Intel swseq is limited to 64 bytes)
    let max_write = master.max_write_len();
    if max_write == 0 {
        // A zero limit would produce zero-sized chunks and never make progress
        return Err(Error::ProgrammerError);
    }

    if enter_exit_4byte {
        protocol::enter_4byte_mode_with_features(master, features).await?;
    }

    let mut offset = 0usize;
    let mut current_addr = addr;

    while offset < data.len() {
        let remaining = data.len() - offset;

        let chunk_size = if write_granularity == WriteGranularity::Byte
            || features.contains(Features::AAI_WORD)
        {
            // Single-byte page program: one byte per WREN+PP+WIP cycle.
            // Also used for SST chips when AAI is unavailable, matching
            // flashprog spi_chip_write_1.
            1
        } else {
            // Page-granularity program: up to a full page per command, respecting
            // page boundaries and the master's maximum write length.
            let page_offset = (current_addr as usize) % page_size;
            let bytes_to_page_end = page_size - page_offset;
            core::cmp::min(core::cmp::min(bytes_to_page_end, remaining), max_write)
        };

        let chunk = &data[offset..offset + chunk_size];

        let result =
            protocol::program_page_with_addressing(master, opcode, current_addr, chunk, addressing)
                .await;
        let result = match result {
            Ok(()) => protection::check_result(master, ctx, current_addr, false).await,
            Err(error) => Err(error),
        };

        if result.is_err() {
            if enter_exit_4byte
                && let Err(e) = protocol::exit_4byte_mode_with_features(master, features).await
            {
                log::warn!("Failed to exit 4-byte address mode: {}", e);
            }
            return result;
        }

        offset += chunk_size;
        current_addr += chunk_size as u32;
    }

    if enter_exit_4byte {
        protocol::exit_4byte_mode_with_features(master, features).await?;
    }

    Ok(())
}

/// Check that a range of flash has been erased (all bytes are 0xFF)
///
/// Reads through the full read path (I/O mode selection, 4-byte addressing)
/// and returns [`Error::EraseError`] with the exact address of the first
/// non-erased byte on failure.
pub async fn check_erased_range<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    len: u32,
) -> Result<()> {
    const CHUNK_SIZE: usize = 4096;
    let mut buf = [0u8; CHUNK_SIZE];

    let mut offset = 0u32;
    while offset < len {
        let chunk_len = core::cmp::min(CHUNK_SIZE as u32, len - offset) as usize;
        let chunk_buf = &mut buf[..chunk_len];

        read(master, ctx, addr + offset, chunk_buf).await?;

        if let Some((idx, &found)) = chunk_buf.iter().enumerate().find(|&(_, &b)| b != 0xFF) {
            return Err(Error::EraseError(EraseFailure::VerifyFailed {
                addr: addr + offset + idx as u32,
                found,
            }));
        }

        offset += chunk_len as u32;
    }

    Ok(())
}

/// Erase through SPI commands. The physical geometry is measured from chip
/// address zero, including across region changes.
pub(crate) async fn erase_spi_range<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    len: u32,
) -> Result<()> {
    // Static catalogs cannot prune their maps at probe time. Select from the
    // supported erasers here instead of requiring an allocating filtered copy.
    let supported = ctx
        .chip
        .erase_blocks()
        .iter()
        .filter(|eb| spi_eraser_usable(master, ctx, eb));
    let chip_erase = if addr == 0 && len == ctx.chip.total_size {
        supported
            .clone()
            .find(|eb| eb.is_chip_erase() && eb.total_size() == len)
            .cloned()
    } else {
        None
    };
    let block = chip_erase
        .or_else(|| select_erase_block_from(supported, addr, len))
        // Preserve the specific opcode error if geometry exists but no menu
        // entry can execute it. Validation still precedes any mutation.
        .or_else(|| select_erase_block(ctx.chip.erase_blocks(), addr, len))
        .ok_or(Error::InvalidAlignment)?;
    erase_spi_operation(
        master,
        ctx,
        &OptimalEraseOp {
            start: addr,
            size: len,
            erase_block: block,
        },
    )
    .await
}

/// Check all planned-command prerequisites without issuing any SPI command.
pub(crate) fn validate_spi_erase_operation<M: SpiMaster + ?Sized>(
    master: &M,
    ctx: &FlashContext,
    op: &OptimalEraseOp,
) -> Result<()> {
    if op.size == 0
        || !ctx.is_valid_range(op.start, op.size as usize)
        || !ctx.chip.erase_blocks().contains(&op.erase_block)
    {
        return Err(Error::InvalidAlignment);
    }
    check_mutation_opcodes(master)?;
    protection::check_supported(master, ctx)?;
    let block = &op.erase_block;
    if block.is_chip_erase() {
        if op.start != 0 || op.size != ctx.chip.total_size || block.total_size() != op.size {
            return Err(Error::InvalidAlignment);
        }
        return check_spi_eraser(master, ctx.chip.features, false, block);
    }
    let end = op
        .start
        .checked_add(op.size)
        .ok_or(Error::AddressOutOfBounds)?;
    let mut addr = op.start;
    while addr < end {
        let size = block
            .block_size_at_boundary(addr)
            .filter(|s| *s > 0)
            .ok_or(Error::InvalidAlignment)?;
        addr = addr.checked_add(size).ok_or(Error::AddressOutOfBounds)?;
        if addr > end {
            return Err(Error::InvalidAlignment);
        }
    }
    let four_byte = ctx.address_mode == AddressMode::FourByte;
    check_spi_eraser(master, ctx.chip.features, four_byte, block)
}

/// Whether `master` can issue `block` on the chip of `ctx`.
pub(crate) fn spi_eraser_usable<M: SpiMaster + ?Sized>(
    master: &M,
    ctx: &FlashContext,
    block: &EraseBlock,
) -> bool {
    let four_byte = ctx.address_mode == AddressMode::FourByte;
    check_spi_eraser(master, ctx.chip.features, four_byte, block).is_ok()
}

/// Check that `master` can issue `block`.
///
/// Some programmers only send an allowed set of opcodes (e.g. Intel software
/// sequencing with a locked opcode menu). With 4-byte addressing a native
/// opcode is preferred when the programmer can send it; otherwise the
/// three-byte opcode needs a usable 4-byte addressing procedure.
pub(super) fn check_spi_eraser<M: SpiMaster + ?Sized>(
    master: &M,
    chip_features: Features,
    four_byte: bool,
    block: &EraseBlock,
) -> Result<()> {
    let four_byte = four_byte && !block.is_chip_erase();
    let native = four_byte
        && block.opcode_4b.is_some_and(|opcode| {
            master.features().contains(SpiFeatures::FOUR_BYTE_ADDR) && master.probe_opcode(opcode)
        });
    if !master.probe_opcode(block.opcode_for_address_width(native)) {
        return Err(Error::OpcodeNotSupported);
    }
    if four_byte {
        addressing_for_4byte_operation(native, chip_features, master.features())?;
    }
    Ok(())
}

/// Execute exactly the validated eraser selected by the planner.
pub(crate) async fn erase_spi_operation<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    op: &OptimalEraseOp,
) -> Result<()> {
    validate_spi_erase_operation(master, ctx, op)?;
    // The chip's geometry (from its metadata) decides what this erases.
    master.check_access(FlashAccess::Modify, op.start, op.size)?;
    let saved = protection::prepare(master, ctx).await?;
    let result = match erase_spi_unprotected(master, ctx, op).await {
        Ok(()) => protection::check_result(master, ctx, op.start, true).await,
        Err(error) => Err(error),
    };
    let ready_timing = if op.erase_block.is_chip_erase() {
        (
            protocol::CHIP_ERASE_POLL_US,
            protocol::CHIP_ERASE_TIMEOUT_US,
        )
    } else {
        protocol::erase_timing(op.erase_block.max_block_size())
    };
    protection::finish(master, ctx, saved, result, ready_timing).await
}

async fn erase_spi_unprotected<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    op: &OptimalEraseOp,
) -> Result<()> {
    let addr = op.start;
    let len = op.size;
    if len == 0
        || !ctx.is_valid_range(addr, len as usize)
        || !ctx.chip.erase_blocks().contains(&op.erase_block)
    {
        return Err(Error::InvalidAlignment);
    }
    let block = &op.erase_block;
    // A request for the whole chip uses the addressless chip-erase opcode when
    // the chip has one. This is what the erase planner selects for a full-chip
    // erase; walking every sector instead is correct but far slower. Chips
    // without one (e.g. stacked-die parts that only offer per-die erase) fall
    // through to the addressed blocks below.
    if block.is_chip_erase() {
        if addr != 0 || len != ctx.chip.total_size || block.total_size() != len {
            return Err(Error::InvalidAlignment);
        }
        if !master.probe_opcode(block.opcode) {
            return Err(Error::OpcodeNotSupported);
        }
        return protocol::chip_erase_with_opcode(master, block.opcode).await;
    }
    let features = ctx.chip.features;
    let use_4byte = ctx.address_mode == AddressMode::FourByte;
    let master_features = master.features();
    let use_native = use_4byte
        && block.opcode_4b.is_some_and(|opcode| {
            master_features.contains(SpiFeatures::FOUR_BYTE_ADDR) && master.probe_opcode(opcode)
        });
    let opcode = block.opcode_for_address_width(use_native);
    if !master.probe_opcode(opcode) {
        return Err(Error::OpcodeNotSupported);
    }
    let (addressing, enter_exit_4byte) = if use_4byte {
        addressing_for_4byte_operation(use_native, features, master_features)?
    } else {
        (CommandAddressing::ThreeByte, false)
    };
    // Validate the complete walk before issuing any erase command.
    let mut current = addr;
    let end = addr.checked_add(len).ok_or(Error::AddressOutOfBounds)?;
    while current < end {
        let size = block
            .block_size_at_boundary(current)
            .ok_or(Error::InvalidAlignment)?;
        current = current.checked_add(size).ok_or(Error::AddressOutOfBounds)?;
        if current > end {
            return Err(Error::InvalidAlignment);
        }
    }

    if enter_exit_4byte {
        protocol::enter_4byte_mode_with_features(master, features).await?;
    }
    let (poll_delay_us, timeout_us) = protocol::erase_timing(block.max_block_size());
    let mut current = addr;
    let mut result = Ok(());
    while current < end {
        let size = block
            .block_size_at_boundary(current)
            .expect("validated erase geometry");
        if let Err(err) = protocol::erase_block(
            master,
            opcode,
            current,
            addressing,
            poll_delay_us,
            timeout_us,
        )
        .await
        {
            result = Err(err);
            break;
        }
        if let Err(error) = protection::check_result(master, ctx, current, true).await {
            result = Err(error);
            break;
        }
        current += size;
    }
    if enter_exit_4byte {
        let exit_result = protocol::exit_4byte_mode_with_features(master, features).await;
        if let Err(err) = exit_result {
            if result.is_ok() {
                return Err(err);
            }
            log::warn!("Failed to exit 4-byte address mode: {}", err);
        }
    }
    result
}

/// Select the best erase block size for the given operation
///
/// Finds the largest erase block that:
/// 1. Is not a chip erase (for partial operations)
/// 2. Has a minimum block size <= the requested length
/// 3. The address is aligned to the minimum block size
/// 4. The length is a multiple of the minimum block size
pub fn select_erase_block(erase_blocks: &[EraseBlock], addr: u32, len: u32) -> Option<EraseBlock> {
    select_erase_block_from(erase_blocks.iter(), addr, len)
}

fn select_erase_block_from<'a>(
    erase_blocks: impl Iterator<Item = &'a EraseBlock>,
    addr: u32,
    len: u32,
) -> Option<EraseBlock> {
    // Find the largest block size that:
    // 1. Evenly divides the length
    // 2. The address is aligned to

    erase_blocks
        .filter(|eb| {
            // Skip chip erase for partial operations
            // Use min_block_size() to get the individual block size (not total coverage)
            !eb.is_chip_erase() && eb.min_block_size() <= len
        })
        .filter(|eb| {
            if eb.is_uniform() {
                let size = eb.min_block_size();
                return size > 0 && addr.is_multiple_of(size) && len.is_multiple_of(size);
            }
            let mut current = addr;
            let Some(end) = addr.checked_add(len) else {
                return false;
            };
            while current < end {
                let Some(size) = eb.block_size_at_boundary(current) else {
                    return false;
                };
                let Some(next) = current.checked_add(size) else {
                    return false;
                };
                if next > end {
                    return false;
                }
                current = next;
            }
            true
        })
        .max_by_key(|eb| eb.max_block_size())
        .cloned()
}

// =============================================================================
// Layout-aware operations
// =============================================================================
//
// These operations handle the case where region boundaries don't align with
// erase block boundaries. Following the same logic as flashprog:
//
// When erasing/writing a region that crosses erase boundaries:
// 1. Read the data outside the region but inside the erase block (to preserve it)
// 2. Erase the full block
// 3. Write back the preserved data
//
// This is known as a "read-modify-write" pattern for unaligned regions.

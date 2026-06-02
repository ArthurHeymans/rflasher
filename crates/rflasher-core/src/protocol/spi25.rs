//! SPI25 protocol implementation
//!
//! This module implements the common SPI flash command sequences
//! as defined by JEDEC.
//!
//! Uses `maybe_async` to support both sync and async modes:
//! - With `is_sync` feature: blocking/synchronous
//! - Without `is_sync` feature: async (for WASM, Embassy, tokio)
//!
//! ## Multi-IO Support
//!
//! This module provides functions for dual and quad I/O reads when supported
//! by both the programmer and the flash chip. The following modes are available:
//!
//! - **Dual Output (1-1-2)**: Command and address in single mode, data in dual
//! - **Dual I/O (1-2-2)**: Command in single mode, address and data in dual
//! - **Quad Output (1-1-4)**: Command and address in single mode, data in quad
//! - **Quad I/O (1-4-4)**: Command in single mode, address and data in quad
//! - **QPI (4-4-4)**: Everything in quad mode

use crate::error::{Error, Result};
use crate::programmer::{SpiFeatures, SpiMaster};
use crate::spi::{AddressWidth, IoMode, SpiCommand, opcodes};
use maybe_async::maybe_async;

// Timing constants for SPI flash operations
/// Poll interval for status register write completion (microseconds)
const WRSR_POLL_US: u32 = 10_000;
/// Timeout for status register write completion (microseconds)
const WRSR_TIMEOUT_US: u32 = 500_000;
/// Poll interval for page program completion (microseconds)
const PAGE_PROGRAM_POLL_US: u32 = 10;
/// Timeout for page program completion (microseconds)
const PAGE_PROGRAM_TIMEOUT_US: u32 = 10_000;
/// Poll interval for chip erase completion (microseconds)
const CHIP_ERASE_POLL_US: u32 = 1_000_000;
/// Timeout for chip erase completion (microseconds)
const CHIP_ERASE_TIMEOUT_US: u32 = 200_000_000;
/// Poll interval for block erase completion (microseconds)
pub const BLOCK_ERASE_POLL_US: u32 = 10_000;
/// Timeout for block erase completion (microseconds)
pub const BLOCK_ERASE_TIMEOUT_US: u32 = 10_000_000;

/// Addressing behavior for an addressed SPI command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAddressing {
    /// Send a 24-bit address directly in the command.
    ThreeByte,
    /// Send a 32-bit address directly in the command.
    FourByte,
    /// Select the high address byte through the chip's extended address
    /// register, then send the low 24 bits in the command.
    ExtendedAddressRegister(crate::chip::Features),
}

impl CommandAddressing {
    fn address_width(self) -> AddressWidth {
        match self {
            Self::ThreeByte | Self::ExtendedAddressRegister(_) => AddressWidth::ThreeByte,
            Self::FourByte => AddressWidth::FourByte,
        }
    }

    fn max_chunk_len(self, addr: u32, requested: usize) -> usize {
        match self {
            Self::ExtendedAddressRegister(_) => {
                let bytes_to_bank_end = 0x01_00_00_00usize - (addr as usize & 0x00FF_FFFF);
                core::cmp::min(requested, bytes_to_bank_end)
            }
            Self::ThreeByte | Self::FourByte => requested,
        }
    }
}

/// Read the JEDEC ID from a flash chip
///
/// Returns (manufacturer_id, device_id) on success.
#[maybe_async]
pub async fn read_jedec_id<M: SpiMaster + ?Sized>(master: &mut M) -> Result<(u8, u16)> {
    let mut buf = [0u8; 3];
    let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut buf);
    master.execute(&mut cmd).await?;

    let manufacturer = buf[0];
    let device = ((buf[1] as u16) << 8) | (buf[2] as u16);

    Ok((manufacturer, device))
}

/// Read the status register 1
#[maybe_async]
pub async fn read_status1<M: SpiMaster + ?Sized>(master: &mut M) -> Result<u8> {
    let mut buf = [0u8; 1];
    let mut cmd = SpiCommand::read_reg(opcodes::RDSR, &mut buf);
    master.execute(&mut cmd).await?;
    Ok(buf[0])
}

/// Read the status register 2
#[maybe_async]
pub async fn read_status2<M: SpiMaster + ?Sized>(master: &mut M) -> Result<u8> {
    let mut buf = [0u8; 1];
    let mut cmd = SpiCommand::read_reg(opcodes::RDSR2, &mut buf);
    master.execute(&mut cmd).await?;
    Ok(buf[0])
}

/// Read the status register 3
#[maybe_async]
pub async fn read_status3<M: SpiMaster + ?Sized>(master: &mut M) -> Result<u8> {
    let mut buf = [0u8; 1];
    let mut cmd = SpiCommand::read_reg(opcodes::RDSR3, &mut buf);
    master.execute(&mut cmd).await?;
    Ok(buf[0])
}

/// Send the Write Enable command (WREN, 0x06)
#[maybe_async]
pub async fn write_enable<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::WREN);
    master.execute(&mut cmd).await
}

/// Send the Enable Write Status Register command (EWSR, 0x50)
///
/// Used on legacy SST25 chips instead of WREN before a WRSR command.
/// Not a general write-enable — it only enables the next WRSR to succeed.
#[maybe_async]
pub async fn write_enable_ewsr<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::EWSR);
    master.execute(&mut cmd).await
}

/// Send the Write Disable command (WRDI, 0x04)
#[maybe_async]
pub async fn write_disable<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::WRDI);
    master.execute(&mut cmd).await
}

/// Wait for the WIP (Write In Progress) bit to clear
///
/// Polls the status register until the Write In Progress bit clears.
/// The `poll_delay_us` parameter specifies the delay between polls.
///
/// # Arguments
/// * `poll_delay_us` - Delay in microseconds between status register polls
/// * `timeout_us` - Maximum time to wait before returning Error::Timeout
///
/// # Typical poll delays (from flashprog):
/// * Page program: 10us
/// * 4KB sector erase: 10,000us (10ms)
/// * 32KB/64KB block erase: 100,000us (100ms)
/// * Chip erase: 1,000,000us (1s)
#[maybe_async]
pub async fn wait_ready<M: SpiMaster + ?Sized>(
    master: &mut M,
    poll_delay_us: u32,
    timeout_us: u32,
) -> Result<()> {
    let max_polls = timeout_us.checked_div(poll_delay_us).unwrap_or(timeout_us);

    for _ in 0..max_polls {
        let status = read_status1(master).await?;
        if status & opcodes::SR1_WIP == 0 {
            return Ok(());
        }
        if poll_delay_us > 0 {
            master.delay_us(poll_delay_us).await;
        }
    }

    Err(Error::Timeout)
}

/// Write the status register 1
///
/// Sends WREN (0x06) before writing. For chips that require EWSR (0x50)
/// instead (legacy SST25 chips), use [`write_status1_ewsr`].
#[maybe_async]
pub async fn write_status1<M: SpiMaster + ?Sized>(master: &mut M, value: u8) -> Result<()> {
    write_enable(master).await?;
    let data = [value];
    let mut cmd = SpiCommand::write_reg(opcodes::WRSR, &data);
    master.execute(&mut cmd).await?;
    // Status register write typically takes 5-200ms, poll every 10ms
    wait_ready(master, WRSR_POLL_US, WRSR_TIMEOUT_US).await
}

/// Write the status register 1, using EWSR (0x50) instead of WREN
///
/// Required for legacy SST25 chips (those with the `WRSR_EWSR` feature flag).
#[maybe_async]
pub async fn write_status1_ewsr<M: SpiMaster + ?Sized>(master: &mut M, value: u8) -> Result<()> {
    write_enable_ewsr(master).await?;
    let data = [value];
    let mut cmd = SpiCommand::write_reg(opcodes::WRSR, &data);
    master.execute(&mut cmd).await?;
    wait_ready(master, WRSR_POLL_US, WRSR_TIMEOUT_US).await
}

/// Write status registers 1 and 2 together
///
/// Some chips require writing both registers in a single command.
/// For chips that require EWSR, use [`write_status12_ewsr`].
#[maybe_async]
pub async fn write_status12<M: SpiMaster + ?Sized>(master: &mut M, sr1: u8, sr2: u8) -> Result<()> {
    write_enable(master).await?;
    let data = [sr1, sr2];
    let mut cmd = SpiCommand::write_reg(opcodes::WRSR, &data);
    master.execute(&mut cmd).await?;
    // Status register write typically takes 5-200ms, poll every 10ms
    wait_ready(master, WRSR_POLL_US, WRSR_TIMEOUT_US).await
}

/// Write status registers 1 and 2 together, using EWSR (0x50) instead of WREN
#[maybe_async]
pub async fn write_status12_ewsr<M: SpiMaster + ?Sized>(
    master: &mut M,
    sr1: u8,
    sr2: u8,
) -> Result<()> {
    write_enable_ewsr(master).await?;
    let data = [sr1, sr2];
    let mut cmd = SpiCommand::write_reg(opcodes::WRSR, &data);
    master.execute(&mut cmd).await?;
    wait_ready(master, WRSR_POLL_US, WRSR_TIMEOUT_US).await
}

/// Read data from flash with an explicitly selected opcode, I/O mode, and addressing mode.
#[maybe_async]
pub async fn read_io_with_addressing<M: SpiMaster + ?Sized>(
    master: &mut M,
    opcode: u8,
    addr: u32,
    buf: &mut [u8],
    addressing: CommandAddressing,
    io_mode: IoMode,
    dummy_cycles: u8,
) -> Result<()> {
    read_multi_io(master, opcode, addr, buf, addressing, io_mode, dummy_cycles).await
}

/// Read data from flash with an explicitly selected opcode and addressing mode.
#[maybe_async]
pub async fn read_with_addressing<M: SpiMaster + ?Sized>(
    master: &mut M,
    opcode: u8,
    addr: u32,
    buf: &mut [u8],
    addressing: CommandAddressing,
) -> Result<()> {
    read_io_with_addressing(master, opcode, addr, buf, addressing, IoMode::Single, 0).await
}

/// Read data from flash using 3-byte addressing
#[maybe_async]
pub async fn read_3b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    read_with_addressing(
        master,
        opcodes::READ,
        addr,
        buf,
        CommandAddressing::ThreeByte,
    )
    .await
}

/// Read data from flash using native 4-byte addressing opcode
#[maybe_async]
pub async fn read_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    read_with_addressing(
        master,
        opcodes::READ_4B,
        addr,
        buf,
        CommandAddressing::FourByte,
    )
    .await
}

/// Program a single page with an explicitly selected opcode and addressing mode.
///
/// The data must not cross a page boundary.
/// Page program typically takes 0.7-5ms, we poll every 10us with 10ms timeout.
#[maybe_async]
pub async fn program_page_with_addressing<M: SpiMaster + ?Sized>(
    master: &mut M,
    opcode: u8,
    addr: u32,
    data: &[u8],
    addressing: CommandAddressing,
) -> Result<()> {
    if let CommandAddressing::ExtendedAddressRegister(features) = addressing {
        set_extended_address(master, features, (addr >> 24) as u8).await?;
    }

    write_enable(master).await?;

    let mut cmd = SpiCommand {
        opcode,
        address: Some(addr),
        address_width: addressing.address_width(),
        io_mode: IoMode::Single,
        dummy_cycles: 0,
        write_data: data,
        read_buf: &mut [],
    };
    master.execute(&mut cmd).await?;

    // Page program: poll every 10us, timeout after 10ms (typical is 0.7-5ms)
    wait_ready(master, PAGE_PROGRAM_POLL_US, PAGE_PROGRAM_TIMEOUT_US).await
}

/// Program a single page (up to page_size bytes) using 3-byte addressing
#[maybe_async]
pub async fn program_page_3b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    data: &[u8],
) -> Result<()> {
    program_page_with_addressing(
        master,
        opcodes::PP,
        addr,
        data,
        CommandAddressing::ThreeByte,
    )
    .await
}

/// Program a single page using native 4-byte addressing opcode
#[maybe_async]
pub async fn program_page_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    data: &[u8],
) -> Result<()> {
    program_page_with_addressing(
        master,
        opcodes::PP_4B,
        addr,
        data,
        CommandAddressing::FourByte,
    )
    .await
}

// ============================================================================
// SST-specific write functions
// ============================================================================

/// AAI (Auto Address Increment) word program for SST25 chips
///
/// Used for SST25VFxxxB chips (those with the `AAI_WORD` feature flag). These
/// chips do not support standard page program (0x02) for multi-byte writes;
/// instead they use a streaming protocol:
///
/// 1. WREN + AAI_WP (0xAD) + 3-byte addr + 2 data bytes → poll WIP
/// 2. Repeat: AAI_WP (0xAD) + 2 data bytes (no addr, no WREN) → poll WIP
/// 3. WRDI (0x04) to exit AAI mode
///
/// Any leading byte at an odd start address, and any trailing byte when `data`
/// has odd length, are written individually via single-byte page program
/// (WREN + PP (0x02) + addr + 1 byte), which SST25 chips support alongside AAI.
///
/// Note: AAI uses 3-byte addressing only. 4-byte address mode is irrelevant
/// for SST25 chips (all are ≤8 MiB).
///
/// # Arguments
/// * `addr` - Start address
/// * `data` - Data to write
#[maybe_async]
pub async fn aai_word_program<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    data: &[u8],
) -> Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    let mut pos = 0usize;
    let mut current_addr = addr;

    // Handle leading odd byte: single byte-program (WREN + PP + addr + 1 byte)
    if !current_addr.is_multiple_of(2) {
        write_enable(master).await?;
        let mut cmd = SpiCommand::write_3b(opcodes::PP, current_addr, &data[pos..pos + 1]);
        master.execute(&mut cmd).await?;
        wait_ready(master, PAGE_PROGRAM_POLL_US, PAGE_PROGRAM_TIMEOUT_US).await?;
        pos += 1;
        current_addr += 1;
    }

    // AAI streaming: requires at least 2 bytes remaining
    if pos + 1 < data.len() {
        // First AAI command: WREN then AAI_WP + 3-byte address + 2 data bytes
        write_enable(master).await?;
        let mut cmd = SpiCommand::write_3b(opcodes::AAI_WP, current_addr, &data[pos..pos + 2]);
        master.execute(&mut cmd).await?;
        if let Err(e) = wait_ready(master, PAGE_PROGRAM_POLL_US, PAGE_PROGRAM_TIMEOUT_US).await {
            // Best-effort exit from AAI mode before propagating the error
            let _ = write_disable(master).await;
            return Err(e);
        }
        pos += 2;
        current_addr += 2;

        // Continuation: AAI_WP + 2 bytes only — no address, no WREN
        while pos + 1 < data.len() {
            let mut cmd = SpiCommand::write_reg(opcodes::AAI_WP, &data[pos..pos + 2]);
            master.execute(&mut cmd).await?;
            if let Err(e) = wait_ready(master, PAGE_PROGRAM_POLL_US, PAGE_PROGRAM_TIMEOUT_US).await
            {
                let _ = write_disable(master).await;
                return Err(e);
            }
            pos += 2;
            current_addr += 2;
        }

        // Always exit AAI mode with WRDI before issuing any other command
        write_disable(master).await?;
    }

    // Handle trailing odd byte (when original data length was odd)
    if pos < data.len() {
        write_enable(master).await?;
        let mut cmd = SpiCommand::write_3b(opcodes::PP, current_addr, &data[pos..pos + 1]);
        master.execute(&mut cmd).await?;
        wait_ready(master, PAGE_PROGRAM_POLL_US, PAGE_PROGRAM_TIMEOUT_US).await?;
    }

    Ok(())
}

/// Global block-protection unlock for SST26 chips (ULBPR, 0x98)
///
/// SST26 chips use a separate block-protection register (not SR BP bits).
/// This function sends WREN + ULBPR to clear all per-block protection bits.
/// Must be called before writing to any region that may be protected.
#[maybe_async]
pub async fn sst26_global_unprotect<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    write_enable(master).await?;
    let mut cmd = SpiCommand::simple(opcodes::ULBPR);
    master.execute(&mut cmd).await
}

/// Erase a sector/block at the given address
///
/// Poll delay should match the expected erase time:
/// - 4KB sector: 10ms poll, 1s timeout (typical 45-400ms)
/// - 32KB block: 100ms poll, 4s timeout (typical 120-1600ms)
/// - 64KB block: 100ms poll, 4s timeout (typical 150-2000ms)
#[maybe_async]
pub async fn erase_block<M: SpiMaster + ?Sized>(
    master: &mut M,
    opcode: u8,
    addr: u32,
    addressing: CommandAddressing,
    poll_delay_us: u32,
    timeout_us: u32,
) -> Result<()> {
    if let CommandAddressing::ExtendedAddressRegister(features) = addressing {
        set_extended_address(master, features, (addr >> 24) as u8).await?;
    }

    write_enable(master).await?;

    let mut cmd = SpiCommand {
        opcode,
        address: Some(addr),
        address_width: addressing.address_width(),
        io_mode: IoMode::Single,
        dummy_cycles: 0,
        write_data: &[],
        read_buf: &mut [],
    };
    master.execute(&mut cmd).await?;

    wait_ready(master, poll_delay_us, timeout_us).await
}

/// Erase the entire chip
///
/// Chip erase typically takes 25-100s for large chips.
/// We poll every 1s with a 200s timeout.
#[maybe_async]
pub async fn chip_erase<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    write_enable(master).await?;

    let mut cmd = SpiCommand::simple(opcodes::CE_C7);
    master.execute(&mut cmd).await?;

    // Chip erase: poll every 1s, timeout after 200s
    wait_ready(master, CHIP_ERASE_POLL_US, CHIP_ERASE_TIMEOUT_US).await
}

/// Enter 4-byte address mode with the plain B7h instruction.
#[maybe_async]
pub async fn enter_4byte_mode<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::EN4B);
    master.execute(&mut cmd).await
}

/// Exit 4-byte address mode with the plain E9h instruction.
#[maybe_async]
pub async fn exit_4byte_mode<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::EX4B);
    master.execute(&mut cmd).await
}

fn extended_address_write_opcode(features: crate::chip::Features) -> Result<u8> {
    use crate::chip::Features;

    if features.contains(Features::EXT_ADDR_REG_C5C8) || features.contains(Features::EXT_ADDR_REG) {
        Ok(opcodes::WREAR)
    } else if features.contains(Features::EXT_ADDR_REG_1716) {
        Ok(opcodes::WREAR_ALT)
    } else {
        Err(Error::ChipNotSupported)
    }
}

/// Write the high address byte to the chip's extended address register.
#[maybe_async]
pub async fn set_extended_address<M: SpiMaster + ?Sized>(
    master: &mut M,
    features: crate::chip::Features,
    addr_high: u8,
) -> Result<()> {
    let opcode = extended_address_write_opcode(features)?;
    write_enable(master).await?;
    let data = [addr_high];
    let mut cmd = SpiCommand::write_reg(opcode, &data);
    master.execute(&mut cmd).await
}

/// Enter 4-byte address mode using the method described by the chip features.
#[maybe_async]
pub async fn enter_4byte_mode_with_features<M: SpiMaster + ?Sized>(
    master: &mut M,
    features: crate::chip::Features,
) -> Result<()> {
    use crate::chip::Features;

    if features.contains(Features::FOUR_BYTE_ENTER) {
        enter_4byte_mode(master).await
    } else if features.contains(Features::FOUR_BYTE_ENTER_WREN) {
        write_enable(master).await?;
        enter_4byte_mode(master).await
    } else if features.contains(Features::FOUR_BYTE_ENTER_EAR7) {
        set_extended_address(master, features, 0x80).await
    } else {
        Err(Error::ChipNotSupported)
    }
}

/// Exit 4-byte address mode using the method described by the chip features.
#[maybe_async]
pub async fn exit_4byte_mode_with_features<M: SpiMaster + ?Sized>(
    master: &mut M,
    features: crate::chip::Features,
) -> Result<()> {
    use crate::chip::Features;

    if features.contains(Features::FOUR_BYTE_ENTER) {
        exit_4byte_mode(master).await
    } else if features.contains(Features::FOUR_BYTE_ENTER_WREN) {
        write_enable(master).await?;
        exit_4byte_mode(master).await
    } else if features.contains(Features::FOUR_BYTE_ENTER_EAR7) {
        set_extended_address(master, features, 0x00).await
    } else {
        Err(Error::ChipNotSupported)
    }
}

/// Send software reset sequence
#[maybe_async]
pub async fn software_reset<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::RSTEN);
    master.execute(&mut cmd).await?;
    master.delay_us(50).await;
    let mut cmd = SpiCommand::simple(opcodes::RST);
    master.execute(&mut cmd).await?;
    master.delay_us(100).await;
    Ok(())
}

/// Read SFDP (Serial Flash Discoverable Parameters)
#[maybe_async]
pub async fn read_sfdp<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    let max_read = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_read, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand {
            opcode: opcodes::RDSFDP,
            address: Some(addr + offset as u32),
            address_width: AddressWidth::ThreeByte,
            io_mode: crate::spi::IoMode::Single,
            dummy_cycles: 8, // SFDP requires 8 dummy cycles
            write_data: &[],
            read_buf: chunk,
        };
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
}

/// Check if the Write Enable Latch is set
#[maybe_async]
pub async fn check_wel<M: SpiMaster + ?Sized>(master: &mut M) -> Result<bool> {
    let status = read_status1(master).await?;
    Ok(status & opcodes::SR1_WEL != 0)
}

/// Check if a write or erase operation is in progress
#[maybe_async]
pub async fn is_busy<M: SpiMaster + ?Sized>(master: &mut M) -> Result<bool> {
    let status = read_status1(master).await?;
    Ok(status & opcodes::SR1_WIP != 0)
}

// ============================================================================
// Multi-I/O Read Functions
// ============================================================================

/// Internal helper to perform a chunked multi-IO read
#[maybe_async]
async fn read_multi_io<M: SpiMaster + ?Sized>(
    master: &mut M,
    opcode: u8,
    addr: u32,
    buf: &mut [u8],
    addressing: CommandAddressing,
    io_mode: IoMode,
    dummy_cycles: u8,
) -> Result<()> {
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let current_addr = addr + offset as u32;
        let chunk_len =
            addressing.max_chunk_len(current_addr, core::cmp::min(max_len, buf.len() - offset));
        let chunk = &mut buf[offset..offset + chunk_len];

        if let CommandAddressing::ExtendedAddressRegister(features) = addressing {
            set_extended_address(master, features, (current_addr >> 24) as u8).await?;
        }

        let mut cmd = SpiCommand {
            opcode,
            address: Some(current_addr),
            address_width: addressing.address_width(),
            io_mode,
            dummy_cycles,
            write_data: &[],
            read_buf: chunk,
        };
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
}

/// Read data using Dual Output mode (1-1-2) with 3-byte address
///
/// Uses opcode 0x3B with 8 dummy cycles.
#[maybe_async]
pub async fn read_dual_out_3b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    read_multi_io(
        master,
        opcodes::DOR,
        addr,
        buf,
        CommandAddressing::ThreeByte,
        IoMode::DualOut,
        8,
    )
    .await
}

/// Read data using Dual I/O mode (1-2-2) with 3-byte address
///
/// Uses opcode 0xBB with mode byte and dummy cycles.
#[maybe_async]
pub async fn read_dual_io_3b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    read_multi_io(
        master,
        opcodes::DIOR,
        addr,
        buf,
        CommandAddressing::ThreeByte,
        IoMode::DualIo,
        4,
    )
    .await
}

/// Read data using Quad Output mode (1-1-4) with 3-byte address
///
/// Uses opcode 0x6B with 8 dummy cycles.
/// Requires Quad Enable (QE) bit to be set.
#[maybe_async]
pub async fn read_quad_out_3b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    read_multi_io(
        master,
        opcodes::QOR,
        addr,
        buf,
        CommandAddressing::ThreeByte,
        IoMode::QuadOut,
        8,
    )
    .await
}

/// Read data using Quad I/O mode (1-4-4) with 3-byte address
///
/// Uses opcode 0xEB with mode byte and dummy cycles.
/// Requires Quad Enable (QE) bit to be set.
#[maybe_async]
pub async fn read_quad_io_3b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    read_multi_io(
        master,
        opcodes::QIOR,
        addr,
        buf,
        CommandAddressing::ThreeByte,
        IoMode::QuadIo,
        6,
    )
    .await
}

/// Read data using Dual Output mode (1-1-2) with 4-byte address
#[maybe_async]
pub async fn read_dual_out_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    read_multi_io(
        master,
        opcodes::DOR_4B,
        addr,
        buf,
        CommandAddressing::FourByte,
        IoMode::DualOut,
        8,
    )
    .await
}

/// Read data using Dual I/O mode (1-2-2) with 4-byte address
#[maybe_async]
pub async fn read_dual_io_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    read_multi_io(
        master,
        opcodes::DIOR_4B,
        addr,
        buf,
        CommandAddressing::FourByte,
        IoMode::DualIo,
        4,
    )
    .await
}

/// Read data using Quad Output mode (1-1-4) with 4-byte address
#[maybe_async]
pub async fn read_quad_out_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    read_multi_io(
        master,
        opcodes::QOR_4B,
        addr,
        buf,
        CommandAddressing::FourByte,
        IoMode::QuadOut,
        8,
    )
    .await
}

/// Read data using Quad I/O mode (1-4-4) with 4-byte address
#[maybe_async]
pub async fn read_quad_io_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    read_multi_io(
        master,
        opcodes::QIOR_4B,
        addr,
        buf,
        CommandAddressing::FourByte,
        IoMode::QuadIo,
        6,
    )
    .await
}

// ============================================================================
// Quad Enable (QE) Functions
// ============================================================================

/// Quad Enable requirement types
///
/// Different flash chips have different ways to enable quad mode.
/// These correspond to the values defined in SFDP JESD216.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuadEnableMethod {
    /// No QE bit required - device does not have a QE bit
    None,
    /// QE is bit 1 of SR2, use WRSR with 2 data bytes to write
    Sr2Bit1WriteSr,
    /// QE is bit 6 of SR1
    Sr1Bit6,
    /// QE is bit 7 of SR2 (use special sequence)
    Sr2Bit7,
    /// QE is bit 1 of SR2, use dedicated 0x31 command
    Sr2Bit1WriteSr2,
}

/// Enable quad mode using the appropriate method for the chip
#[maybe_async]
pub async fn enable_quad_mode<M: SpiMaster + ?Sized>(
    master: &mut M,
    method: QuadEnableMethod,
) -> Result<()> {
    match method {
        QuadEnableMethod::None => Ok(()),
        QuadEnableMethod::Sr2Bit1WriteSr => {
            // Read SR1 and SR2, set QE bit (bit 1 of SR2), write both
            let sr1 = read_status1(master).await?;
            let sr2 = read_status2(master).await?;
            if sr2 & opcodes::SR2_QE != 0 {
                return Ok(()); // Already enabled
            }
            write_status12(master, sr1, sr2 | opcodes::SR2_QE).await
        }
        QuadEnableMethod::Sr1Bit6 => {
            // QE is bit 6 of SR1
            let sr1 = read_status1(master).await?;
            if sr1 & 0x40 != 0 {
                return Ok(()); // Already enabled
            }
            write_status1(master, sr1 | 0x40).await
        }
        QuadEnableMethod::Sr2Bit7 => {
            // QE is bit 7 of SR2 - use special sequence
            let sr2 = read_status2(master).await?;
            if sr2 & 0x80 != 0 {
                return Ok(()); // Already enabled
            }
            write_status2_direct(master, sr2 | 0x80).await
        }
        QuadEnableMethod::Sr2Bit1WriteSr2 => {
            // QE is bit 1 of SR2, use dedicated 0x31 command
            let sr2 = read_status2(master).await?;
            if sr2 & opcodes::SR2_QE != 0 {
                return Ok(()); // Already enabled
            }
            write_status2_direct(master, sr2 | opcodes::SR2_QE).await
        }
    }
}

/// Disable quad mode using the appropriate method for the chip
#[maybe_async]
pub async fn disable_quad_mode<M: SpiMaster + ?Sized>(
    master: &mut M,
    method: QuadEnableMethod,
) -> Result<()> {
    match method {
        QuadEnableMethod::None => Ok(()),
        QuadEnableMethod::Sr2Bit1WriteSr => {
            let sr1 = read_status1(master).await?;
            let sr2 = read_status2(master).await?;
            if sr2 & opcodes::SR2_QE == 0 {
                return Ok(()); // Already disabled
            }
            write_status12(master, sr1, sr2 & !opcodes::SR2_QE).await
        }
        QuadEnableMethod::Sr1Bit6 => {
            let sr1 = read_status1(master).await?;
            if sr1 & 0x40 == 0 {
                return Ok(()); // Already disabled
            }
            write_status1(master, sr1 & !0x40).await
        }
        QuadEnableMethod::Sr2Bit7 => {
            let sr2 = read_status2(master).await?;
            if sr2 & 0x80 == 0 {
                return Ok(()); // Already disabled
            }
            write_status2_direct(master, sr2 & !0x80).await
        }
        QuadEnableMethod::Sr2Bit1WriteSr2 => {
            let sr2 = read_status2(master).await?;
            if sr2 & opcodes::SR2_QE == 0 {
                return Ok(()); // Already disabled
            }
            write_status2_direct(master, sr2 & !opcodes::SR2_QE).await
        }
    }
}

/// Write SR2 directly using opcode 0x31
#[maybe_async]
async fn write_status2_direct<M: SpiMaster + ?Sized>(master: &mut M, value: u8) -> Result<()> {
    write_enable(master).await?;
    let data = [value];
    let mut cmd = SpiCommand::write_reg(opcodes::WRSR2, &data);
    master.execute(&mut cmd).await?;
    // Status register write typically takes 5-200ms, poll every 10ms
    wait_ready(master, WRSR_POLL_US, WRSR_TIMEOUT_US).await
}

/// Check if quad mode is enabled
#[maybe_async]
pub async fn is_quad_enabled<M: SpiMaster + ?Sized>(
    master: &mut M,
    method: QuadEnableMethod,
) -> Result<bool> {
    match method {
        QuadEnableMethod::None => Ok(true), // No QE needed, always "enabled"
        QuadEnableMethod::Sr2Bit1WriteSr | QuadEnableMethod::Sr2Bit1WriteSr2 => {
            let sr2 = read_status2(master).await?;
            Ok(sr2 & opcodes::SR2_QE != 0)
        }
        QuadEnableMethod::Sr1Bit6 => {
            let sr1 = read_status1(master).await?;
            Ok(sr1 & 0x40 != 0)
        }
        QuadEnableMethod::Sr2Bit7 => {
            let sr2 = read_status2(master).await?;
            Ok(sr2 & 0x80 != 0)
        }
    }
}

// ============================================================================
// QPI Mode Functions
// ============================================================================

/// Enter QPI mode (4-4-4)
///
/// Different chips use different opcodes - common ones are 0x35 and 0x38.
#[maybe_async]
pub async fn enter_qpi_mode<M: SpiMaster + ?Sized>(master: &mut M, opcode: u8) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcode);
    master.execute(&mut cmd).await
}

/// Exit QPI mode
///
/// Common exit opcodes are 0xF5 and 0xFF.
/// Note: This command must be sent in QPI mode (4-4-4).
#[maybe_async]
pub async fn exit_qpi_mode<M: SpiMaster + ?Sized>(master: &mut M, opcode: u8) -> Result<()> {
    let mut cmd = SpiCommand {
        opcode,
        address: None,
        address_width: AddressWidth::None,
        io_mode: IoMode::Qpi,
        dummy_cycles: 0,
        write_data: &[],
        read_buf: &mut [],
    };
    master.execute(&mut cmd).await
}

// ============================================================================
// Read Mode Selection Helper
// ============================================================================

/// Select the best available read mode based on programmer and chip capabilities.
///
/// Returns the I/O mode, opcode, and whether the selected opcode is a native
/// 4-byte-address opcode. Prefers higher bandwidth modes:
/// Quad I/O > Quad Out > Dual I/O > Dual Out > Single.
pub fn select_read_mode(
    master_features: SpiFeatures,
    chip_features: crate::chip::Features,
    try_native_4byte: bool,
    mut opcode_supported: impl FnMut(u8) -> bool,
) -> (IoMode, u8, bool) {
    let chip_has_dual = chip_features.contains(crate::chip::Features::DUAL_IO);
    let chip_has_quad = chip_features.contains(crate::chip::Features::QUAD_IO);

    if try_native_4byte && master_features.contains(SpiFeatures::FOUR_BYTE_ADDR) {
        let native_candidates: [(bool, SpiFeatures, IoMode, u8); 4] = [
            (
                chip_features.supports_4ba_quad_io_read(),
                SpiFeatures::QUAD_IO,
                IoMode::QuadIo,
                opcodes::QIOR_4B,
            ),
            (
                chip_features.supports_4ba_quad_out_read(),
                SpiFeatures::QUAD_IN,
                IoMode::QuadOut,
                opcodes::QOR_4B,
            ),
            (
                chip_features.supports_4ba_dual_io_read(),
                SpiFeatures::DUAL_IO,
                IoMode::DualIo,
                opcodes::DIOR_4B,
            ),
            (
                chip_features.supports_4ba_dual_out_read(),
                SpiFeatures::DUAL_IN,
                IoMode::DualOut,
                opcodes::DOR_4B,
            ),
        ];

        for (chip_capable, feature, mode, opcode) in native_candidates {
            if chip_capable && master_features.contains(feature) && opcode_supported(opcode) {
                return (mode, opcode, true);
            }
        }

        if chip_features.supports_4ba_read() && opcode_supported(opcodes::READ_4B) {
            return (IoMode::Single, opcodes::READ_4B, true);
        }
    }

    // Compatibility-mode candidates. These opcodes may still be sent with a
    // 32-bit address when the chip has been switched into 4BA mode.
    let candidates: [(bool, SpiFeatures, IoMode, u8); 4] = [
        (
            chip_has_quad,
            SpiFeatures::QUAD_IO,
            IoMode::QuadIo,
            opcodes::QIOR,
        ),
        (
            chip_has_quad,
            SpiFeatures::QUAD_IN,
            IoMode::QuadOut,
            opcodes::QOR,
        ),
        (
            chip_has_dual,
            SpiFeatures::DUAL_IO,
            IoMode::DualIo,
            opcodes::DIOR,
        ),
        (
            chip_has_dual,
            SpiFeatures::DUAL_IN,
            IoMode::DualOut,
            opcodes::DOR,
        ),
    ];

    for (chip_capable, feature, mode, opcode) in candidates {
        if chip_capable && master_features.contains(feature) && opcode_supported(opcode) {
            return (mode, opcode, false);
        }
    }

    (IoMode::Single, opcodes::READ, false)
}

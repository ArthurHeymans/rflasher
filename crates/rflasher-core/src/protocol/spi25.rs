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
use crate::spi::{opcodes, AddressWidth, IoMode, SpiCommand};
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

/// Read data from flash using 3-byte addressing
#[maybe_async]
pub async fn read_3b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand::read_3b(opcodes::READ, addr + offset as u32, chunk);
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
}

/// Read data from flash using 4-byte addressing
#[maybe_async]
pub async fn read_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand::read_4b(opcodes::READ_4B, addr + offset as u32, chunk);
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
}

/// Program a single page (up to page_size bytes)
///
/// The data must not cross a page boundary.
/// Page program typically takes 0.7-5ms, we poll every 10us with 10ms timeout.
#[maybe_async]
pub async fn program_page_3b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    data: &[u8],
) -> Result<()> {
    write_enable(master).await?;

    let mut cmd = SpiCommand::write_3b(opcodes::PP, addr, data);
    master.execute(&mut cmd).await?;

    // Page program: poll every 10us, timeout after 10ms (typical is 0.7-5ms)
    wait_ready(master, PAGE_PROGRAM_POLL_US, PAGE_PROGRAM_TIMEOUT_US).await
}

/// Program a single page using 4-byte addressing
/// Page program typically takes 0.7-5ms, we poll every 10us with 10ms timeout.
#[maybe_async]
pub async fn program_page_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    data: &[u8],
) -> Result<()> {
    write_enable(master).await?;

    let mut cmd = SpiCommand::write_4b(opcodes::PP_4B, addr, data);
    master.execute(&mut cmd).await?;

    // Page program: poll every 10us, timeout after 10ms (typical is 0.7-5ms)
    wait_ready(master, PAGE_PROGRAM_POLL_US, PAGE_PROGRAM_TIMEOUT_US).await
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
    use_4byte: bool,
    poll_delay_us: u32,
    timeout_us: u32,
) -> Result<()> {
    write_enable(master).await?;

    let mut cmd = if use_4byte {
        SpiCommand::erase_4b(opcode, addr)
    } else {
        SpiCommand::erase_3b(opcode, addr)
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

/// Enter 4-byte address mode
#[maybe_async]
pub async fn enter_4byte_mode<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::EN4B);
    master.execute(&mut cmd).await
}

/// Exit 4-byte address mode
#[maybe_async]
pub async fn exit_4byte_mode<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::EX4B);
    master.execute(&mut cmd).await
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
    address_width: AddressWidth,
    io_mode: IoMode,
    dummy_cycles: u8,
) -> Result<()> {
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand {
            opcode,
            address: Some(addr + offset as u32),
            address_width,
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
        AddressWidth::ThreeByte,
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
        AddressWidth::ThreeByte,
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
        AddressWidth::ThreeByte,
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
        AddressWidth::ThreeByte,
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
        AddressWidth::FourByte,
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
        AddressWidth::FourByte,
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
        AddressWidth::FourByte,
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
        AddressWidth::FourByte,
        IoMode::QuadIo,
        6,
    )
    .await
}

/// Read data from flash using a pre-selected `SpiReadOp`.
///
/// Handles address-width and dummy-cycle encoding. This is the general-purpose
/// entry point for the flash layer once a read op has been chosen via
/// `select_read_op`. Chunks the request by `master.max_read_len()`.
///
/// The mode byte (when applicable for 1-2-2 / 1-4-4 / 4-4-4 reads) is emitted
/// implicitly as the first byte of the dummy phase; since all dummy bytes
/// are filled with 0xFF and `M[7:4]` ≠ `A` for 0xFF, this avoids entering
/// continuous read mode on chips that key off of that nibble.
#[maybe_async]
pub async fn read_with_op<M: SpiMaster + ?Sized>(
    master: &mut M,
    op: &SpiReadOp,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    let addr_width = op.address_width();
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand {
            opcode: op.opcode,
            address: Some(addr + offset as u32),
            address_width: addr_width,
            io_mode: op.io_mode,
            dummy_cycles: op.dummy_cycles,
            write_data: &[],
            read_buf: chunk,
        };
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }
    Ok(())
}

// ============================================================================
// QPI Mode Entry / Exit
// ============================================================================

/// Enter QPI mode by sending the specified enter opcode.
///
/// Common values:
/// - `0x35` (Winbond, GigaDevice) — exit with `0xF5`
/// - `0x38` (Macronix, ISSI, Spansion) — exit with `0xFF`
#[maybe_async]
pub async fn enter_qpi_with<M: SpiMaster + ?Sized>(
    master: &mut M,
    enter_opcode: u8,
) -> Result<()> {
    let mut cmd = SpiCommand::simple(enter_opcode);
    master.execute(&mut cmd).await
}

/// Exit QPI mode by sending the specified exit opcode in QPI framing.
#[maybe_async]
pub async fn exit_qpi_with<M: SpiMaster + ?Sized>(
    master: &mut M,
    exit_opcode: u8,
) -> Result<()> {
    let mut cmd = SpiCommand::simple(exit_opcode).with_io_mode(IoMode::Qpi);
    master.execute(&mut cmd).await
}

/// Set Read Parameters (SRP, 0xC0) to configure QPI dummy cycles.
///
/// Parameter byte encodes burst length and dummy cycles (chip-specific).
/// Chips supporting this command: Macronix MX25L, ISSI IS25WP, some Spansion.
#[maybe_async]
pub async fn set_read_params<M: SpiMaster + ?Sized>(
    master: &mut M,
    params: u8,
) -> Result<()> {
    let data = [params];
    let mut cmd = SpiCommand {
        opcode: 0xC0,
        address: None,
        address_width: AddressWidth::None,
        io_mode: IoMode::Qpi,
        dummy_cycles: 0,
        write_data: &data,
        read_buf: &mut [],
    };
    master.execute(&mut cmd).await
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

/// A chosen read operation.
///
/// Carries everything a programmer needs to issue a flash read:
/// - `opcode`: the SPI command byte
/// - `io_mode`: the wire format (Single / DualOut / DualIo / QuadOut / QuadIo / Qpi)
/// - `dummy_cycles`: total number of clock cycles (at the I/O mode's lane
///   count) between end-of-address and start-of-data. For 1-2-2 / 1-4-4 /
///   4-4-4 reads this includes the mode-byte (M7-M0) time. Programmers
///   emit these clocks as 0xFF on the wire, which is safe as an M byte
///   (top nibble ≠ 0xA, so continuous-read mode is not enabled on
///   Winbond-family chips).
/// - `native_4ba`: true if the opcode is the native 4-byte-address variant
///   (chip already expects a 4-byte address, no EN4B needed)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpiReadOp {
    /// SPI opcode byte
    pub opcode: u8,
    /// IO mode for the transaction
    pub io_mode: IoMode,
    /// Dummy clock cycles between address and data (includes M byte time)
    pub dummy_cycles: u8,
    /// True if `opcode` is a native 4-byte-address variant
    pub native_4ba: bool,
}

impl SpiReadOp {
    /// Default `READ` (0x03), single I/O, 3-byte address, no dummy.
    pub const fn sio_read() -> Self {
        Self {
            opcode: opcodes::READ,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            native_4ba: false,
        }
    }

    /// Default `READ_4B` (0x13), single I/O, 4-byte address, no dummy.
    pub const fn sio_read_4b() -> Self {
        Self {
            opcode: opcodes::READ_4B,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            native_4ba: true,
        }
    }

    /// Address width implied by this read op.
    pub const fn address_width(&self) -> AddressWidth {
        if self.native_4ba {
            AddressWidth::FourByte
        } else {
            AddressWidth::ThreeByte
        }
    }
}

/// Fine-grained chip capabilities for read-op selection.
///
/// This struct mirrors the relevant subset of `chip::Features` plus QPI mode
/// state. It lets `select_read_op` stay free of direct chip-struct coupling
/// (keeping `protocol` usable without the chip module).
#[derive(Debug, Clone, Copy, Default)]
pub struct ChipReadCapabilities {
    /// Chip supports `FAST_READ` (0x0B / 0x0C)
    pub fast_read: bool,
    /// Chip supports 1-1-2 fast read (0x3B / 0x3C)
    pub dout: bool,
    /// Chip supports 1-2-2 fast read (0xBB / 0xBC)
    pub dio: bool,
    /// Chip supports 1-1-4 fast read (0x6B / 0x6C)
    pub qout: bool,
    /// Chip supports 1-4-4 fast read (0xEB / 0xEC)
    pub qio: bool,
    /// Chip supports 4-4-4 QPI fast read
    pub qpi_fast_read: bool,
    /// Chip supports native 4BA QPI read (0xEC)
    pub qpi4b: bool,
    /// Chip has a native 4-byte-addr single-IO read (0x0C / 0x13)
    pub native_4ba_read: bool,
    /// Chip currently in QPI mode (4-4-4)
    pub in_qpi_mode: bool,
}

/// Dummy-cycle overrides for each mode. `0` means "use JEDEC default".
#[derive(Debug, Clone, Copy, Default)]
pub struct DummyCycleOverrides {
    /// 1-1-2 dummy cycles (default 8)
    pub dc_112: u8,
    /// 1-2-2 dummy cycles (default 4)
    pub dc_122: u8,
    /// 1-1-4 dummy cycles (default 8)
    pub dc_114: u8,
    /// 1-4-4 dummy cycles (default 6)
    pub dc_144: u8,
    /// QPI fast read dummy cycles (default 8 for 0x0B, 4 for 0xEB)
    pub dc_qpi: u8,
}

/// JEDEC-default dummy cycle counts for each multi-IO read mode.
///
/// These mirror the values used in `read_dual_out_*`, `read_dual_io_*`,
/// `read_quad_out_*`, `read_quad_io_*` helpers below.
pub const DEFAULT_DUMMY_CYCLES_112: u8 = 8;
/// JEDEC default dummy cycles for 1-2-2 dual-I/O read (0xBB)
pub const DEFAULT_DUMMY_CYCLES_122: u8 = 4;
/// JEDEC default dummy cycles for 1-1-4 quad-output read (0x6B)
pub const DEFAULT_DUMMY_CYCLES_114: u8 = 8;
/// JEDEC default dummy cycles for 1-4-4 quad-I/O read (0xEB)
pub const DEFAULT_DUMMY_CYCLES_144: u8 = 6;

fn effective_dc(override_val: u8, default: u8) -> u8 {
    if override_val == 0 {
        default
    } else {
        override_val
    }
}

/// Select the best available read operation based on programmer and chip capabilities.
///
/// Mirrors flashprog's `select_qpi_fast_read` and `select_multi_io_fast_read`
/// in `spi25_prepare.c`. Prefers QPI > quad > dual > single with 4BA-native
/// variants preferred when the address needs 4 bytes.
pub fn select_read_op(
    master_features: SpiFeatures,
    chip: ChipReadCapabilities,
    dc: DummyCycleOverrides,
    use_4byte: bool,
) -> SpiReadOp {
    // QPI mode: must use 4-4-4 framing
    if chip.in_qpi_mode && master_features.contains(SpiFeatures::QPI) {
        // Prefer native 4BA QPI (0xEC) if supported and addr >= 16 MiB
        if use_4byte && chip.qpi4b {
            return SpiReadOp {
                opcode: opcodes::QIOR_4B,
                io_mode: IoMode::Qpi,
                dummy_cycles: effective_dc(dc.dc_qpi, DEFAULT_DUMMY_CYCLES_144),
                native_4ba: true,
            };
        }
        // Fast-read in QPI via 0xEB
        if chip.qpi_fast_read {
            return SpiReadOp {
                opcode: opcodes::QIOR,
                io_mode: IoMode::Qpi,
                dummy_cycles: effective_dc(dc.dc_qpi, DEFAULT_DUMMY_CYCLES_144),
                native_4ba: false,
            };
        }
        // Fall back to plain FAST_READ in QPI (8 dummies)
        return SpiReadOp {
            opcode: opcodes::FAST_READ,
            io_mode: IoMode::Qpi,
            dummy_cycles: effective_dc(dc.dc_qpi, 8),
            native_4ba: false,
        };
    }

    // Non-QPI: priority-ordered candidates.
    struct Cand {
        chip_cap: bool,
        master_feat: SpiFeatures,
        io_mode: IoMode,
        opcode_3b: u8,
        opcode_4b: u8,
        default_dc: u8,
        dc_override: u8,
    }

    let candidates = [
        // 1-4-4 Quad I/O
        Cand {
            chip_cap: chip.qio,
            master_feat: SpiFeatures::QUAD_IO,
            io_mode: IoMode::QuadIo,
            opcode_3b: opcodes::QIOR,
            opcode_4b: opcodes::QIOR_4B,
            default_dc: DEFAULT_DUMMY_CYCLES_144,
            dc_override: dc.dc_144,
        },
        // 1-1-4 Quad Output
        Cand {
            chip_cap: chip.qout,
            master_feat: SpiFeatures::QUAD_IN,
            io_mode: IoMode::QuadOut,
            opcode_3b: opcodes::QOR,
            opcode_4b: opcodes::QOR_4B,
            default_dc: DEFAULT_DUMMY_CYCLES_114,
            dc_override: dc.dc_114,
        },
        // 1-2-2 Dual I/O
        Cand {
            chip_cap: chip.dio,
            master_feat: SpiFeatures::DUAL_IO,
            io_mode: IoMode::DualIo,
            opcode_3b: opcodes::DIOR,
            opcode_4b: opcodes::DIOR_4B,
            default_dc: DEFAULT_DUMMY_CYCLES_122,
            dc_override: dc.dc_122,
        },
        // 1-1-2 Dual Output
        Cand {
            chip_cap: chip.dout,
            master_feat: SpiFeatures::DUAL_IN,
            io_mode: IoMode::DualOut,
            opcode_3b: opcodes::DOR,
            opcode_4b: opcodes::DOR_4B,
            default_dc: DEFAULT_DUMMY_CYCLES_112,
            dc_override: dc.dc_112,
        },
    ];

    for c in candidates {
        if c.chip_cap && master_features.contains(c.master_feat) {
            let opcode = if use_4byte { c.opcode_4b } else { c.opcode_3b };
            return SpiReadOp {
                opcode,
                io_mode: c.io_mode,
                dummy_cycles: effective_dc(c.dc_override, c.default_dc),
                native_4ba: use_4byte,
            };
        }
    }

    // Single I/O fallback.
    if use_4byte && chip.native_4ba_read {
        return SpiReadOp::sio_read_4b();
    }
    if chip.fast_read && master_features.contains(SpiFeatures::FOUR_BYTE_ADDR) && use_4byte {
        return SpiReadOp {
            opcode: opcodes::FAST_READ_4B,
            io_mode: IoMode::Single,
            dummy_cycles: 8,
            native_4ba: true,
        };
    }
    if chip.fast_read && !use_4byte {
        return SpiReadOp {
            opcode: opcodes::FAST_READ,
            io_mode: IoMode::Single,
            dummy_cycles: 8,
            native_4ba: false,
        };
    }
    SpiReadOp::sio_read()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn all_mio_caps() -> ChipReadCapabilities {
        ChipReadCapabilities {
            fast_read: true,
            dout: true,
            dio: true,
            qout: true,
            qio: true,
            qpi_fast_read: false,
            qpi4b: false,
            native_4ba_read: false,
            in_qpi_mode: false,
        }
    }

    #[test]
    fn select_quad_io_when_both_sides_support() {
        let master = SpiFeatures::FOUR_BYTE_ADDR
            | SpiFeatures::DUAL_IN
            | SpiFeatures::DUAL_IO
            | SpiFeatures::QUAD_IN
            | SpiFeatures::QUAD_IO;
        let op =
            select_read_op(master, all_mio_caps(), DummyCycleOverrides::default(), false);
        assert_eq!(op.io_mode, IoMode::QuadIo);
        assert_eq!(op.opcode, opcodes::QIOR);
        assert_eq!(op.dummy_cycles, DEFAULT_DUMMY_CYCLES_144);
        assert!(!op.native_4ba);
    }

    #[test]
    fn select_quad_io_4b_when_4byte_addressing() {
        let master = SpiFeatures::FOUR_BYTE_ADDR
            | SpiFeatures::QUAD_IN
            | SpiFeatures::QUAD_IO;
        let op = select_read_op(master, all_mio_caps(), DummyCycleOverrides::default(), true);
        assert_eq!(op.io_mode, IoMode::QuadIo);
        assert_eq!(op.opcode, opcodes::QIOR_4B);
        assert!(op.native_4ba);
    }

    #[test]
    fn fallback_to_dual_io_when_master_lacks_quad() {
        let master = SpiFeatures::DUAL_IN | SpiFeatures::DUAL_IO;
        let op =
            select_read_op(master, all_mio_caps(), DummyCycleOverrides::default(), false);
        assert_eq!(op.io_mode, IoMode::DualIo);
        assert_eq!(op.opcode, opcodes::DIOR);
        assert_eq!(op.dummy_cycles, DEFAULT_DUMMY_CYCLES_122);
    }

    #[test]
    fn fallback_to_dual_out_when_chip_lacks_dio() {
        let master = SpiFeatures::DUAL_IN | SpiFeatures::DUAL_IO;
        let mut caps = all_mio_caps();
        caps.dio = false;
        caps.qio = false;
        caps.qout = false;
        let op = select_read_op(master, caps, DummyCycleOverrides::default(), false);
        assert_eq!(op.io_mode, IoMode::DualOut);
        assert_eq!(op.opcode, opcodes::DOR);
    }

    #[test]
    fn fallback_to_single_when_no_multiio() {
        let master = SpiFeatures::empty();
        let caps = ChipReadCapabilities {
            fast_read: true,
            ..Default::default()
        };
        let op = select_read_op(master, caps, DummyCycleOverrides::default(), false);
        assert_eq!(op.io_mode, IoMode::Single);
        assert_eq!(op.opcode, opcodes::FAST_READ);
    }

    #[test]
    fn dummy_cycle_overrides_apply() {
        let master = SpiFeatures::QUAD_IN | SpiFeatures::QUAD_IO;
        let dc = DummyCycleOverrides {
            dc_144: 10,
            ..Default::default()
        };
        let op = select_read_op(master, all_mio_caps(), dc, false);
        assert_eq!(op.io_mode, IoMode::QuadIo);
        assert_eq!(op.dummy_cycles, 10);
    }

    #[test]
    fn qpi_mode_uses_qpi_framing() {
        let master = SpiFeatures::QPI
            | SpiFeatures::QUAD_IN
            | SpiFeatures::QUAD_IO;
        let mut caps = all_mio_caps();
        caps.in_qpi_mode = true;
        caps.qpi_fast_read = true;
        let op = select_read_op(master, caps, DummyCycleOverrides::default(), false);
        assert_eq!(op.io_mode, IoMode::Qpi);
        assert_eq!(op.opcode, opcodes::QIOR);
    }

    #[test]
    fn qpi_mode_uses_4b_opcode_when_supported_and_addr_4byte() {
        let master = SpiFeatures::QPI;
        let mut caps = all_mio_caps();
        caps.in_qpi_mode = true;
        caps.qpi_fast_read = true;
        caps.qpi4b = true;
        let op = select_read_op(master, caps, DummyCycleOverrides::default(), true);
        assert_eq!(op.io_mode, IoMode::Qpi);
        assert_eq!(op.opcode, opcodes::QIOR_4B);
        assert!(op.native_4ba);
    }

    #[test]
    fn sio_read_defaults() {
        let op = SpiReadOp::sio_read();
        assert_eq!(op.opcode, opcodes::READ);
        assert_eq!(op.io_mode, IoMode::Single);
        assert_eq!(op.dummy_cycles, 0);
        assert!(!op.native_4ba);
        assert_eq!(op.address_width(), AddressWidth::ThreeByte);
    }

    #[test]
    fn sio_read_4b_defaults() {
        let op = SpiReadOp::sio_read_4b();
        assert_eq!(op.opcode, opcodes::READ_4B);
        assert!(op.native_4ba);
        assert_eq!(op.address_width(), AddressWidth::FourByte);
    }
}

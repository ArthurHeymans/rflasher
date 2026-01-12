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

/// Send the Write Enable command
#[maybe_async]
pub async fn write_enable<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::WREN);
    master.execute(&mut cmd).await
}

/// Send the Write Disable command
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
    let max_polls = if poll_delay_us > 0 {
        timeout_us / poll_delay_us
    } else {
        timeout_us // Fall back to polling once per microsecond
    };

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
/// Automatically sends WREN before writing.
#[maybe_async]
pub async fn write_status1<M: SpiMaster + ?Sized>(master: &mut M, value: u8) -> Result<()> {
    write_enable(master).await?;
    let data = [value];
    let mut cmd = SpiCommand::write_reg(opcodes::WRSR, &data);
    master.execute(&mut cmd).await?;
    // Status register write typically takes 5-200ms, poll every 10ms
    wait_ready(master, 10_000, 500_000).await
}

/// Write status registers 1 and 2 together
///
/// Some chips require writing both registers in a single command.
#[maybe_async]
pub async fn write_status12<M: SpiMaster + ?Sized>(master: &mut M, sr1: u8, sr2: u8) -> Result<()> {
    write_enable(master).await?;
    let data = [sr1, sr2];
    let mut cmd = SpiCommand::write_reg(opcodes::WRSR, &data);
    master.execute(&mut cmd).await?;
    // Status register write typically takes 5-200ms, poll every 10ms
    wait_ready(master, 10_000, 500_000).await
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
    wait_ready(master, 10, 10_000).await
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
    wait_ready(master, 10, 10_000).await
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
    wait_ready(master, 1_000_000, 200_000_000).await
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

/// Read data using Dual Output mode (1-1-2) with 3-byte address
///
/// Uses opcode 0x3B with 8 dummy cycles.
#[maybe_async]
pub async fn read_dual_out_3b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand {
            opcode: opcodes::DOR,
            address: Some(addr + offset as u32),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::DualOut,
            dummy_cycles: 8, // 8 dummy cycles for dual output read
            write_data: &[],
            read_buf: chunk,
        };
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
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
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand {
            opcode: opcodes::DIOR,
            address: Some(addr + offset as u32),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::DualIo,
            dummy_cycles: 4, // 4 dummy cycles (including mode byte) for 1-2-2
            write_data: &[],
            read_buf: chunk,
        };
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
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
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand {
            opcode: opcodes::QOR,
            address: Some(addr + offset as u32),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::QuadOut,
            dummy_cycles: 8, // 8 dummy cycles for quad output read
            write_data: &[],
            read_buf: chunk,
        };
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
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
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand {
            opcode: opcodes::QIOR,
            address: Some(addr + offset as u32),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::QuadIo,
            dummy_cycles: 6, // 6 dummy cycles (including mode byte) for 1-4-4
            write_data: &[],
            read_buf: chunk,
        };
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
}

/// Read data using Dual Output mode (1-1-2) with 4-byte address
#[maybe_async]
pub async fn read_dual_out_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand {
            opcode: opcodes::DOR_4B,
            address: Some(addr + offset as u32),
            address_width: AddressWidth::FourByte,
            io_mode: IoMode::DualOut,
            dummy_cycles: 8,
            write_data: &[],
            read_buf: chunk,
        };
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
}

/// Read data using Dual I/O mode (1-2-2) with 4-byte address
#[maybe_async]
pub async fn read_dual_io_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand {
            opcode: opcodes::DIOR_4B,
            address: Some(addr + offset as u32),
            address_width: AddressWidth::FourByte,
            io_mode: IoMode::DualIo,
            dummy_cycles: 4,
            write_data: &[],
            read_buf: chunk,
        };
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
}

/// Read data using Quad Output mode (1-1-4) with 4-byte address
#[maybe_async]
pub async fn read_quad_out_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand {
            opcode: opcodes::QOR_4B,
            address: Some(addr + offset as u32),
            address_width: AddressWidth::FourByte,
            io_mode: IoMode::QuadOut,
            dummy_cycles: 8,
            write_data: &[],
            read_buf: chunk,
        };
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
}

/// Read data using Quad I/O mode (1-4-4) with 4-byte address
#[maybe_async]
pub async fn read_quad_io_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand {
            opcode: opcodes::QIOR_4B,
            address: Some(addr + offset as u32),
            address_width: AddressWidth::FourByte,
            io_mode: IoMode::QuadIo,
            dummy_cycles: 6,
            write_data: &[],
            read_buf: chunk,
        };
        master.execute(&mut cmd).await?;
        offset += chunk_len;
    }

    Ok(())
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
    wait_ready(master, 10_000, 500_000).await
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

/// Select the best available read mode based on programmer and chip capabilities
///
/// Returns the read function to use and the corresponding IO mode.
pub fn select_read_mode(
    master_features: SpiFeatures,
    chip_has_dual: bool,
    chip_has_quad: bool,
    use_4byte: bool,
) -> (IoMode, u8) {
    // Prefer quad I/O if both programmer and chip support it
    if chip_has_quad && master_features.contains(SpiFeatures::QUAD_IO) {
        if use_4byte {
            return (IoMode::QuadIo, opcodes::QIOR_4B);
        } else {
            return (IoMode::QuadIo, opcodes::QIOR);
        }
    }

    // Fall back to quad output
    if chip_has_quad && master_features.contains(SpiFeatures::QUAD_IN) {
        if use_4byte {
            return (IoMode::QuadOut, opcodes::QOR_4B);
        } else {
            return (IoMode::QuadOut, opcodes::QOR);
        }
    }

    // Try dual I/O
    if chip_has_dual && master_features.contains(SpiFeatures::DUAL_IO) {
        if use_4byte {
            return (IoMode::DualIo, opcodes::DIOR_4B);
        } else {
            return (IoMode::DualIo, opcodes::DIOR);
        }
    }

    // Try dual output
    if chip_has_dual && master_features.contains(SpiFeatures::DUAL_IN) {
        if use_4byte {
            return (IoMode::DualOut, opcodes::DOR_4B);
        } else {
            return (IoMode::DualOut, opcodes::DOR);
        }
    }

    // Fall back to single I/O
    if use_4byte {
        (IoMode::Single, opcodes::READ_4B)
    } else {
        (IoMode::Single, opcodes::READ)
    }
}

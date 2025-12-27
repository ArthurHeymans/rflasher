//! SPI25 protocol implementation
//!
//! This module implements the common SPI flash command sequences
//! as defined by JEDEC.

use crate::error::{Error, Result};
use crate::programmer::SpiMaster;
use crate::spi::{opcodes, AddressWidth, SpiCommand};

/// Read the JEDEC ID from a flash chip
///
/// Returns (manufacturer_id, device_id) on success.
pub fn read_jedec_id<M: SpiMaster + ?Sized>(master: &mut M) -> Result<(u8, u16)> {
    let mut buf = [0u8; 3];
    let mut cmd = SpiCommand::read_reg(opcodes::RDID, &mut buf);
    master.execute(&mut cmd)?;

    let manufacturer = buf[0];
    let device = ((buf[1] as u16) << 8) | (buf[2] as u16);

    Ok((manufacturer, device))
}

/// Read the status register 1
pub fn read_status1<M: SpiMaster + ?Sized>(master: &mut M) -> Result<u8> {
    let mut buf = [0u8; 1];
    let mut cmd = SpiCommand::read_reg(opcodes::RDSR, &mut buf);
    master.execute(&mut cmd)?;
    Ok(buf[0])
}

/// Read the status register 2
pub fn read_status2<M: SpiMaster + ?Sized>(master: &mut M) -> Result<u8> {
    let mut buf = [0u8; 1];
    let mut cmd = SpiCommand::read_reg(opcodes::RDSR2, &mut buf);
    master.execute(&mut cmd)?;
    Ok(buf[0])
}

/// Read the status register 3
pub fn read_status3<M: SpiMaster + ?Sized>(master: &mut M) -> Result<u8> {
    let mut buf = [0u8; 1];
    let mut cmd = SpiCommand::read_reg(opcodes::RDSR3, &mut buf);
    master.execute(&mut cmd)?;
    Ok(buf[0])
}

/// Send the Write Enable command
pub fn write_enable<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::WREN);
    master.execute(&mut cmd)
}

/// Send the Write Disable command
pub fn write_disable<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::WRDI);
    master.execute(&mut cmd)
}

/// Wait for the WIP (Write In Progress) bit to clear
///
/// Returns Error::Timeout if the operation doesn't complete within
/// the specified number of iterations.
pub fn wait_ready<M: SpiMaster + ?Sized>(master: &mut M, timeout_us: u32) -> Result<()> {
    let poll_interval_us = 100;
    let max_polls = timeout_us / poll_interval_us;

    for _ in 0..max_polls {
        let status = read_status1(master)?;
        if status & opcodes::SR1_WIP == 0 {
            return Ok(());
        }
        master.delay_us(poll_interval_us);
    }

    Err(Error::Timeout)
}

/// Write the status register 1
///
/// Automatically sends WREN before writing.
pub fn write_status1<M: SpiMaster + ?Sized>(master: &mut M, value: u8) -> Result<()> {
    write_enable(master)?;
    let data = [value];
    let mut cmd = SpiCommand::write_reg(opcodes::WRSR, &data);
    master.execute(&mut cmd)?;
    wait_ready(master, 100_000) // 100ms timeout
}

/// Write status registers 1 and 2 together
///
/// Some chips require writing both registers in a single command.
pub fn write_status12<M: SpiMaster + ?Sized>(master: &mut M, sr1: u8, sr2: u8) -> Result<()> {
    write_enable(master)?;
    let data = [sr1, sr2];
    let mut cmd = SpiCommand::write_reg(opcodes::WRSR, &data);
    master.execute(&mut cmd)?;
    wait_ready(master, 100_000)
}

/// Read data from flash using 3-byte addressing
pub fn read_3b<M: SpiMaster + ?Sized>(master: &mut M, addr: u32, buf: &mut [u8]) -> Result<()> {
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand::read_3b(opcodes::READ, addr + offset as u32, chunk);
        master.execute(&mut cmd)?;
        offset += chunk_len;
    }

    Ok(())
}

/// Read data from flash using 4-byte addressing
pub fn read_4b<M: SpiMaster + ?Sized>(master: &mut M, addr: u32, buf: &mut [u8]) -> Result<()> {
    let max_len = master.max_read_len();
    let mut offset = 0;

    while offset < buf.len() {
        let chunk_len = core::cmp::min(max_len, buf.len() - offset);
        let chunk = &mut buf[offset..offset + chunk_len];
        let mut cmd = SpiCommand::read_4b(opcodes::READ_4B, addr + offset as u32, chunk);
        master.execute(&mut cmd)?;
        offset += chunk_len;
    }

    Ok(())
}

/// Program a single page (up to page_size bytes)
///
/// The data must not cross a page boundary.
pub fn program_page_3b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    data: &[u8],
    timeout_us: u32,
) -> Result<()> {
    write_enable(master)?;

    let mut cmd = SpiCommand::write_3b(opcodes::PP, addr, data);
    master.execute(&mut cmd)?;

    wait_ready(master, timeout_us)
}

/// Program a single page using 4-byte addressing
pub fn program_page_4b<M: SpiMaster + ?Sized>(
    master: &mut M,
    addr: u32,
    data: &[u8],
    timeout_us: u32,
) -> Result<()> {
    write_enable(master)?;

    let mut cmd = SpiCommand::write_4b(opcodes::PP_4B, addr, data);
    master.execute(&mut cmd)?;

    wait_ready(master, timeout_us)
}

/// Erase a sector/block at the given address
pub fn erase_block<M: SpiMaster + ?Sized>(
    master: &mut M,
    opcode: u8,
    addr: u32,
    use_4byte: bool,
    timeout_us: u32,
) -> Result<()> {
    write_enable(master)?;

    let mut cmd = if use_4byte {
        SpiCommand::erase_4b(opcode, addr)
    } else {
        SpiCommand::erase_3b(opcode, addr)
    };
    master.execute(&mut cmd)?;

    wait_ready(master, timeout_us)
}

/// Erase the entire chip
pub fn chip_erase<M: SpiMaster + ?Sized>(master: &mut M, timeout_us: u32) -> Result<()> {
    write_enable(master)?;

    let mut cmd = SpiCommand::simple(opcodes::CE_C7);
    master.execute(&mut cmd)?;

    wait_ready(master, timeout_us)
}

/// Enter 4-byte address mode
pub fn enter_4byte_mode<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::EN4B);
    master.execute(&mut cmd)
}

/// Exit 4-byte address mode
pub fn exit_4byte_mode<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::EX4B);
    master.execute(&mut cmd)
}

/// Send software reset sequence
pub fn software_reset<M: SpiMaster + ?Sized>(master: &mut M) -> Result<()> {
    let mut cmd = SpiCommand::simple(opcodes::RSTEN);
    master.execute(&mut cmd)?;
    master.delay_us(50);
    let mut cmd = SpiCommand::simple(opcodes::RST);
    master.execute(&mut cmd)?;
    master.delay_us(100);
    Ok(())
}

/// Read SFDP (Serial Flash Discoverable Parameters)
pub fn read_sfdp<M: SpiMaster + ?Sized>(master: &mut M, addr: u32, buf: &mut [u8]) -> Result<()> {
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
        master.execute(&mut cmd)?;
        offset += chunk_len;
    }

    Ok(())
}

/// Check if the Write Enable Latch is set
pub fn check_wel<M: SpiMaster + ?Sized>(master: &mut M) -> Result<bool> {
    let status = read_status1(master)?;
    Ok(status & opcodes::SR1_WEL != 0)
}

/// Check if a write or erase operation is in progress
pub fn is_busy<M: SpiMaster + ?Sized>(master: &mut M) -> Result<bool> {
    let status = read_status1(master)?;
    Ok(status & opcodes::SR1_WIP != 0)
}

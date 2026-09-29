//! `rflasher_programmers::dummy` - In-memory flash emulator for testing
//!
//! This crate provides a dummy flash programmer that emulates a flash chip
//! in memory. It's useful for testing and development without real hardware.
//!
//! The emulator is deliberately unforgiving in the ways real SPI NOR chips
//! are, so that protocol bugs show up in software instead of on hardware:
//!
//! - **Write enable and busy state.** Program, erase and status writes only
//!   take effect after WREN (the WEL bit in SR1). WEL clears after every such
//!   command. A command that starts an internal operation raises WIP for
//!   [`DummyConfig::busy_polls`] status reads; issuing another modifying
//!   command while busy is an error (a real chip would ignore it, silently
//!   losing the operation).
//! - **Silently ignored commands.** Without WEL, or against a range covered by
//!   the block-protect bits (BP0-2, TB), program and erase commands change
//!   nothing and report no error, exactly like the hardware. Only reading the
//!   flash back reveals it.
//! - **Page wrap-around.** A page program never leaves its page: data past the
//!   end wraps to the start of the same page.
//! - **Address width.** Opcodes that take a 3-byte address must be sent with
//!   one (or with four bytes while the chip is in 4-byte mode), opcodes with a
//!   native 4-byte address must always carry four. Anything else is an error,
//!   as a real chip would mis-decode the command.

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::vec;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use rflasher_core::error::{Error, Result};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{AddressWidth, SpiCommand, opcodes};

/// Configuration for the dummy flash
#[derive(Debug, Clone)]
pub struct DummyConfig {
    /// JEDEC manufacturer ID
    pub manufacturer_id: u8,
    /// JEDEC device ID
    pub device_id: u16,
    /// Flash size in bytes
    pub size: usize,
    /// Page size for programming
    pub page_size: usize,
    /// Sector size for smallest erase
    pub sector_size: usize,
    /// Number of status reads that report WIP after a program, erase or
    /// status write. Exercises the busy-polling of the code under test.
    pub busy_polls: u32,
}

impl Default for DummyConfig {
    fn default() -> Self {
        Self {
            manufacturer_id: 0xEF, // Winbond
            device_id: 0x4018,     // W25Q128FV
            size: 16 * 1024 * 1024,
            page_size: 256,
            sector_size: 4096,
            busy_polls: 2,
        }
    }
}

/// Dummy flash programmer
///
/// Emulates a flash chip in memory for testing purposes.
#[cfg(feature = "alloc")]
pub struct DummyFlash {
    config: DummyConfig,
    data: Vec<u8>,
    /// SR1 without the volatile WIP/WEL bits (block protection etc.)
    status_reg1: u8,
    status_reg2: u8,
    status_reg3: u8,
    write_enabled: bool,
    /// Remaining status reads that will still report WIP
    busy_polls: u32,
    in_4byte_mode: bool,
}

/// How an opcode's address phase must look
#[cfg(feature = "alloc")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum AddressKind {
    /// 3 bytes, or 4 while the chip is in 4-byte address mode
    Legacy,
    /// Always 4 bytes (native 4-byte-address opcodes)
    Native4,
}

#[cfg(feature = "alloc")]
impl DummyFlash {
    /// Create a new dummy flash with the given configuration
    pub fn new(config: DummyConfig) -> Self {
        let data = vec![0xFF; config.size];
        Self {
            config,
            data,
            status_reg1: 0,
            status_reg2: 0,
            status_reg3: 0,
            write_enabled: false,
            busy_polls: 0,
            in_4byte_mode: false,
        }
    }

    /// Create a new dummy flash with default configuration (W25Q128FV)
    pub fn new_default() -> Self {
        Self::new(DummyConfig::default())
    }

    /// Create a dummy flash with pre-filled data
    pub fn with_data(config: DummyConfig, initial_data: &[u8]) -> Self {
        let mut flash = Self::new(config);
        let len = core::cmp::min(initial_data.len(), flash.data.len());
        flash.data[..len].copy_from_slice(&initial_data[..len]);
        flash
    }

    /// Get a reference to the flash data
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Get a mutable reference to the flash data
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Get the configuration
    pub fn config(&self) -> &DummyConfig {
        &self.config
    }

    /// Whether the chip is currently in 4-byte address mode
    pub fn in_4byte_mode(&self) -> bool {
        self.in_4byte_mode
    }

    /// Set the status register 1 block-protect configuration directly
    /// (BP0-2, TB); other bits are preserved.
    pub fn set_block_protect(&mut self, bits: u8) {
        const MASK: u8 = opcodes::SR1_BP0 | opcodes::SR1_BP1 | opcodes::SR1_BP2 | opcodes::SR1_TB;
        self.status_reg1 = (self.status_reg1 & !MASK) | (bits & MASK);
    }

    /// The byte range protected by the block-protect bits, if any.
    ///
    /// BP = 1..=6 protects 1/64 .. 1/2 of the chip, BP = 7 all of it, from
    /// the top (TB = 0) or the bottom (TB = 1).
    fn protected_range(&self) -> Option<core::ops::Range<usize>> {
        let bp = (self.status_reg1 >> 2) & 0x07;
        let size = self.data.len();
        let len = match bp {
            0 => return None,
            7 => size,
            n => size >> (7 - u32::from(n)),
        };
        Some(if self.status_reg1 & opcodes::SR1_TB != 0 {
            0..len
        } else {
            size - len..size
        })
    }

    fn is_protected(&self, start: usize, len: usize) -> bool {
        self.protected_range()
            .is_some_and(|p| start < p.end && start + len > p.start)
    }

    /// Validate the address phase of `cmd` and return the address
    fn address(&self, cmd: &SpiCommand<'_>, kind: AddressKind) -> Result<usize> {
        let addr = cmd.address.unwrap_or(0);
        let expected = match kind {
            AddressKind::Native4 => AddressWidth::FourByte,
            AddressKind::Legacy if self.in_4byte_mode => AddressWidth::FourByte,
            AddressKind::Legacy => AddressWidth::ThreeByte,
        };
        // In 4-byte mode a legacy opcode also accepts the 3-byte form only
        // when the chip is not in that mode; the reverse mismatch is what
        // breaks real chips (the address shifts by a byte).
        if cmd.address_width != expected {
            return Err(Error::SpiTransferFailed);
        }
        if expected == AddressWidth::ThreeByte && addr > 0x00FF_FFFF {
            return Err(Error::SpiTransferFailed);
        }
        Ok(addr as usize)
    }

    /// Reject commands that a real chip would ignore while busy.
    fn ensure_idle(&self) -> Result<()> {
        if self.busy_polls > 0 {
            Err(Error::SpiTransferFailed)
        } else {
            Ok(())
        }
    }

    /// Start the internal operation that follows program/erase/WRSR
    fn start_operation(&mut self) {
        self.write_enabled = false;
        self.busy_polls = self.config.busy_polls;
    }

    fn handle_read(&mut self, cmd: &mut SpiCommand<'_>, kind: AddressKind) -> Result<()> {
        let addr = self.address(cmd, kind)?;
        let len = cmd.read_buf.len();

        if addr + len > self.data.len() {
            return Err(Error::AddressOutOfBounds);
        }

        cmd.read_buf.copy_from_slice(&self.data[addr..addr + len]);
        Ok(())
    }

    fn handle_page_program(&mut self, cmd: &SpiCommand<'_>, kind: AddressKind) -> Result<()> {
        self.ensure_idle()?;
        let addr = self.address(cmd, kind)?;
        let data = cmd.write_data;

        if addr >= self.data.len() {
            return Err(Error::AddressOutOfBounds);
        }
        if !self.write_enabled {
            return Ok(());
        }

        let page = self.config.page_size;
        let page_start = addr - addr % page;
        let protected = self.is_protected(page_start, page);
        if !protected {
            // Programming can only clear bits, and wraps inside the page.
            for (i, &byte) in data.iter().enumerate() {
                let offset = (addr % page + i) % page;
                self.data[page_start + offset] &= byte;
            }
        }

        self.start_operation();
        Ok(())
    }

    fn handle_sector_erase(
        &mut self,
        cmd: &SpiCommand<'_>,
        kind: AddressKind,
        erase_size: usize,
    ) -> Result<()> {
        self.ensure_idle()?;
        let addr = self.address(cmd, kind)?;

        // The chip erases the block containing the address.
        let aligned_addr = addr & !(erase_size - 1);
        if aligned_addr + erase_size > self.data.len() {
            return Err(Error::AddressOutOfBounds);
        }
        if !self.write_enabled {
            return Ok(());
        }

        if !self.is_protected(aligned_addr, erase_size) {
            self.data[aligned_addr..aligned_addr + erase_size].fill(0xFF);
        }

        self.start_operation();
        Ok(())
    }

    fn handle_chip_erase(&mut self) -> Result<()> {
        self.ensure_idle()?;
        if !self.write_enabled {
            return Ok(());
        }

        // Chip erase is ignored entirely if any block is protected.
        if self.protected_range().is_none() {
            self.data.fill(0xFF);
        }

        self.start_operation();
        Ok(())
    }

    /// SR1 as read on the wire: stored bits plus live WIP and WEL
    fn read_status1(&mut self) -> u8 {
        let mut status = self.status_reg1 & !(opcodes::SR1_WIP | opcodes::SR1_WEL);
        if self.write_enabled {
            status |= opcodes::SR1_WEL;
        }
        if self.busy_polls > 0 {
            self.busy_polls -= 1;
            status |= opcodes::SR1_WIP;
        }
        status
    }
}

#[cfg(feature = "alloc")]
impl SpiMaster for DummyFlash {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::FOUR_BYTE_ADDR | SpiFeatures::DUAL | SpiFeatures::QUAD
    }

    fn max_read_len(&self) -> usize {
        4096
    }

    fn max_write_len(&self) -> usize {
        self.config.page_size
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
        // Note: DummyFlash accepts all I/O modes since it's an in-memory emulator.
        // The io_mode field is ignored because we just simulate the flash behavior
        // without actually transferring data on physical wires.
        use AddressKind::{Legacy, Native4};

        match cmd.opcode {
            // JEDEC ID
            opcodes::RDID => {
                if cmd.read_buf.len() >= 3 {
                    cmd.read_buf[0] = self.config.manufacturer_id;
                    cmd.read_buf[1] = (self.config.device_id >> 8) as u8;
                    cmd.read_buf[2] = self.config.device_id as u8;
                }
                Ok(())
            }

            // Status register read
            opcodes::RDSR => {
                if !cmd.read_buf.is_empty() {
                    cmd.read_buf[0] = self.read_status1();
                }
                Ok(())
            }
            opcodes::RDSR2 => {
                if !cmd.read_buf.is_empty() {
                    cmd.read_buf[0] = self.status_reg2;
                }
                Ok(())
            }
            opcodes::RDSR3 => {
                if !cmd.read_buf.is_empty() {
                    cmd.read_buf[0] = self.status_reg3;
                }
                Ok(())
            }

            // Status register write (volatile WIP/WEL bits are not storable)
            opcodes::WRSR => {
                self.ensure_idle()?;
                if self.write_enabled {
                    if let Some(&sr1) = cmd.write_data.first() {
                        self.status_reg1 = sr1 & !(opcodes::SR1_WIP | opcodes::SR1_WEL);
                    }
                    if let Some(&sr2) = cmd.write_data.get(1) {
                        self.status_reg2 = sr2;
                    }
                    self.start_operation();
                }
                Ok(())
            }

            // Write enable/disable
            opcodes::WREN => {
                self.ensure_idle()?;
                self.write_enabled = true;
                Ok(())
            }
            opcodes::WRDI => {
                self.write_enabled = false;
                Ok(())
            }

            // Read commands, including the dual/quad ones the emulator
            // advertises support for (`io_mode` only affects the wires).
            opcodes::READ
            | opcodes::FAST_READ
            | opcodes::DOR
            | opcodes::DIOR
            | opcodes::QOR
            | opcodes::QIOR => self.handle_read(cmd, Legacy),
            opcodes::READ_4B
            | opcodes::FAST_READ_4B
            | opcodes::DOR_4B
            | opcodes::DIOR_4B
            | opcodes::QOR_4B
            | opcodes::QIOR_4B => self.handle_read(cmd, Native4),

            // Page program
            opcodes::PP => self.handle_page_program(cmd, Legacy),
            opcodes::PP_4B => self.handle_page_program(cmd, Native4),

            // Erase commands
            opcodes::SE_20 => self.handle_sector_erase(cmd, Legacy, 4 * 1024),
            opcodes::SE_21 => self.handle_sector_erase(cmd, Native4, 4 * 1024),
            opcodes::BE_52 => self.handle_sector_erase(cmd, Legacy, 32 * 1024),
            opcodes::BE_5C => self.handle_sector_erase(cmd, Native4, 32 * 1024),
            opcodes::BE_D8 => self.handle_sector_erase(cmd, Legacy, 64 * 1024),
            opcodes::BE_DC => self.handle_sector_erase(cmd, Native4, 64 * 1024),
            opcodes::CE_60 | opcodes::CE_C7 => self.handle_chip_erase(),

            // 4-byte address mode
            opcodes::EN4B => {
                self.in_4byte_mode = true;
                Ok(())
            }
            opcodes::EX4B => {
                self.in_4byte_mode = false;
                Ok(())
            }

            // Software reset
            opcodes::RSTEN | opcodes::RST => Ok(()),

            // Unknown opcode
            _ => Err(Error::OpcodeNotSupported),
        }
    }

    async fn delay_us(&mut self, _us: u32) {
        // No delay needed for in-memory operations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::future::block_on;
    use rflasher_core::protocol;

    /// A small chip, so tests can address all of it cheaply
    fn small_flash() -> DummyFlash {
        DummyFlash::new(DummyConfig {
            size: 128 * 1024,
            ..DummyConfig::default()
        })
    }

    fn run(flash: &mut DummyFlash, mut cmd: SpiCommand<'_>) -> Result<()> {
        block_on(flash.execute(&mut cmd))
    }

    fn program(flash: &mut DummyFlash, addr: u32, data: &[u8]) {
        block_on(protocol::write_enable(flash)).unwrap();
        run(flash, SpiCommand::write_3b(opcodes::PP, addr, data)).unwrap();
        block_on(protocol::wait_ready(flash, 1, 1000)).unwrap();
    }

    fn read(flash: &mut DummyFlash, addr: u32, len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        run(flash, SpiCommand::read_3b(opcodes::READ, addr, &mut buf)).unwrap();
        buf
    }

    #[test]
    fn test_read_jedec_id() {
        let mut flash = DummyFlash::new_default();
        let (mfr, dev) = block_on(protocol::read_jedec_id(&mut flash)).unwrap();
        assert_eq!(mfr, 0xEF);
        assert_eq!(dev, 0x4018);
    }

    #[test]
    fn test_read_write() {
        let mut flash = DummyFlash::new_default();

        // Write some data
        let data = [0x12, 0x34, 0x56, 0x78];
        program(&mut flash, 0x1000, &data);

        // Read it back
        assert_eq!(read(&mut flash, 0x1000, 4), data);
    }

    #[test]
    fn test_erase() {
        let mut flash = DummyFlash::new_default();

        // Write some data
        let data = [0x00u8; 256];
        program(&mut flash, 0, &data);
        assert_eq!(read(&mut flash, 0, 256), data);

        // Erase the sector
        block_on(protocol::write_enable(&mut flash)).unwrap();
        run(&mut flash, SpiCommand::erase_3b(opcodes::SE_20, 0)).unwrap();
        block_on(protocol::wait_ready(&mut flash, 1, 1000)).unwrap();

        // Verify it's erased
        assert!(read(&mut flash, 0, 256).iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn status_reports_wel_and_a_busy_period_after_each_operation() {
        let mut flash = small_flash();
        let status = |flash: &mut DummyFlash| block_on(protocol::read_status1(flash)).unwrap();

        assert_eq!(
            status(&mut flash) & (opcodes::SR1_WEL | opcodes::SR1_WIP),
            0
        );
        block_on(protocol::write_enable(&mut flash)).unwrap();
        assert_eq!(status(&mut flash) & opcodes::SR1_WEL, opcodes::SR1_WEL);

        run(&mut flash, SpiCommand::write_3b(opcodes::PP, 0, &[0x00])).unwrap();
        // WEL clears when the operation starts, and WIP stays up for the
        // configured number of status reads.
        assert_eq!(status(&mut flash), opcodes::SR1_WIP);
        assert_eq!(status(&mut flash), opcodes::SR1_WIP);
        assert_eq!(status(&mut flash), 0);
    }

    #[test]
    fn modifying_commands_while_busy_are_an_error() {
        let mut flash = small_flash();
        block_on(protocol::write_enable(&mut flash)).unwrap();
        run(&mut flash, SpiCommand::write_3b(opcodes::PP, 0, &[0x00])).unwrap();
        // Still busy: a client that skips WIP polling would lose these.
        assert!(run(&mut flash, SpiCommand::simple(opcodes::WREN)).is_err());
        assert!(run(&mut flash, SpiCommand::erase_3b(opcodes::SE_20, 0)).is_err());
    }

    #[test]
    fn program_and_erase_without_wel_are_silently_ignored() {
        let mut flash = small_flash();
        run(&mut flash, SpiCommand::write_3b(opcodes::PP, 0, &[0x00])).unwrap();
        assert_eq!(read(&mut flash, 0, 1), [0xFF]);

        program(&mut flash, 0, &[0x00]);
        run(&mut flash, SpiCommand::erase_3b(opcodes::SE_20, 0)).unwrap();
        assert_eq!(read(&mut flash, 0, 1), [0x00]);
    }

    #[test]
    fn page_program_wraps_inside_its_page() {
        let mut flash = small_flash();
        // Four bytes starting two bytes before the end of page 0.
        program(&mut flash, 254, &[1, 2, 3, 4]);
        assert_eq!(read(&mut flash, 254, 2), [1, 2]);
        // The last two bytes landed at the *start of the same page*,
        // not in the next one.
        assert_eq!(read(&mut flash, 0, 2), [3, 4]);
        assert_eq!(read(&mut flash, 256, 2), [0xFF, 0xFF]);
    }

    #[test]
    fn block_protect_makes_program_and_erase_no_ops() {
        let mut flash = small_flash();
        program(&mut flash, 0x1_F000, &[0x00]); // top sector, before protecting
        // BP = 7: everything protected.
        flash.set_block_protect(opcodes::SR1_BP0 | opcodes::SR1_BP1 | opcodes::SR1_BP2);

        program(&mut flash, 0, &[0x00]);
        assert_eq!(read(&mut flash, 0, 1), [0xFF]);

        block_on(protocol::write_enable(&mut flash)).unwrap();
        run(&mut flash, SpiCommand::erase_3b(opcodes::SE_20, 0x1_F000)).unwrap();
        block_on(protocol::wait_ready(&mut flash, 1, 1000)).unwrap();
        assert_eq!(read(&mut flash, 0x1_F000, 1), [0x00]);

        // BP = 1 protects only the top 1/64 (2 KiB): the bottom stays writable.
        flash.set_block_protect(opcodes::SR1_BP0);
        program(&mut flash, 0, &[0x0F]);
        assert_eq!(read(&mut flash, 0, 1), [0x0F]);
    }

    #[test]
    fn dual_and_quad_reads_return_the_same_data() {
        let mut flash = small_flash();
        program(&mut flash, 0x100, &[1, 2, 3, 4]);
        for opcode in [
            opcodes::FAST_READ,
            opcodes::DOR,
            opcodes::DIOR,
            opcodes::QOR,
            opcodes::QIOR,
        ] {
            let mut buf = [0u8; 4];
            run(&mut flash, SpiCommand::read_3b(opcode, 0x100, &mut buf)).unwrap();
            assert_eq!(buf, [1, 2, 3, 4], "opcode {opcode:#04x}");
        }
        for opcode in [opcodes::READ_4B, opcodes::DOR_4B, opcodes::QIOR_4B] {
            let mut buf = [0u8; 4];
            run(&mut flash, SpiCommand::read_4b(opcode, 0x100, &mut buf)).unwrap();
            assert_eq!(buf, [1, 2, 3, 4], "opcode {opcode:#04x}");
        }
    }

    #[test]
    fn address_width_must_match_opcode_and_mode() {
        let mut flash = small_flash();
        let mut buf = [0u8; 1];

        // Legacy opcode with a 4-byte address while the chip expects 3.
        assert!(run(&mut flash, SpiCommand::read_4b(opcodes::READ, 0, &mut buf)).is_err());
        // Native 4-byte opcode carrying only 3 address bytes.
        assert!(
            run(
                &mut flash,
                SpiCommand::read_3b(opcodes::READ_4B, 0, &mut buf)
            )
            .is_err()
        );
        // Native opcodes work regardless of the mode.
        run(
            &mut flash,
            SpiCommand::read_4b(opcodes::READ_4B, 0, &mut buf),
        )
        .unwrap();

        // In 4-byte mode the legacy opcodes need four address bytes.
        run(&mut flash, SpiCommand::simple(opcodes::EN4B)).unwrap();
        assert!(flash.in_4byte_mode());
        assert!(run(&mut flash, SpiCommand::read_3b(opcodes::READ, 0, &mut buf)).is_err());
        run(&mut flash, SpiCommand::read_4b(opcodes::READ, 0, &mut buf)).unwrap();
    }
}

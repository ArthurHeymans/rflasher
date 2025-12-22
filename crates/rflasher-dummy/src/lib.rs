//! rflasher-dummy - In-memory flash emulator for testing
//!
//! This crate provides a dummy flash programmer that emulates a flash chip
//! in memory. It's useful for testing and development without real hardware.

#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "alloc")]
use alloc::vec;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use rflasher_core::error::{Error, Result};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{opcodes, SpiCommand};

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
}

impl Default for DummyConfig {
    fn default() -> Self {
        Self {
            manufacturer_id: 0xEF, // Winbond
            device_id: 0x4018,     // W25Q128FV
            size: 16 * 1024 * 1024,
            page_size: 256,
            sector_size: 4096,
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
    status_reg1: u8,
    status_reg2: u8,
    status_reg3: u8,
    write_enabled: bool,
    in_4byte_mode: bool,
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

    fn get_address(&self, cmd: &SpiCommand<'_>) -> Option<u32> {
        cmd.address
    }

    fn handle_read(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
        let addr = self.get_address(cmd).unwrap_or(0) as usize;
        let len = cmd.read_buf.len();

        if addr + len > self.data.len() {
            return Err(Error::AddressOutOfBounds);
        }

        cmd.read_buf.copy_from_slice(&self.data[addr..addr + len]);
        Ok(())
    }

    fn handle_page_program(&mut self, cmd: &SpiCommand<'_>) -> Result<()> {
        if !self.write_enabled {
            return Err(Error::WriteProtected);
        }

        let addr = self.get_address(cmd).unwrap_or(0) as usize;
        let data = cmd.write_data;

        if addr + data.len() > self.data.len() {
            return Err(Error::AddressOutOfBounds);
        }

        // Flash programming: can only change 1 -> 0
        for (i, &byte) in data.iter().enumerate() {
            self.data[addr + i] &= byte;
        }

        self.write_enabled = false;
        Ok(())
    }

    fn handle_sector_erase(&mut self, cmd: &SpiCommand<'_>, erase_size: usize) -> Result<()> {
        if !self.write_enabled {
            return Err(Error::WriteProtected);
        }

        let addr = self.get_address(cmd).unwrap_or(0) as usize;

        // Align address to erase boundary
        let aligned_addr = addr & !(erase_size - 1);

        if aligned_addr + erase_size > self.data.len() {
            return Err(Error::AddressOutOfBounds);
        }

        // Erase sets all bytes to 0xFF
        for byte in &mut self.data[aligned_addr..aligned_addr + erase_size] {
            *byte = 0xFF;
        }

        self.write_enabled = false;
        Ok(())
    }

    fn handle_chip_erase(&mut self) -> Result<()> {
        if !self.write_enabled {
            return Err(Error::WriteProtected);
        }

        for byte in &mut self.data {
            *byte = 0xFF;
        }

        self.write_enabled = false;
        Ok(())
    }
}

#[cfg(feature = "alloc")]
impl SpiMaster for DummyFlash {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::FOUR_BYTE_ADDR | SpiFeatures::DUAL_OUTPUT | SpiFeatures::QUAD_OUTPUT
    }

    fn max_read_len(&self) -> usize {
        4096
    }

    fn max_write_len(&self) -> usize {
        self.config.page_size
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
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
                    cmd.read_buf[0] = self.status_reg1;
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

            // Status register write
            opcodes::WRSR => {
                if self.write_enabled {
                    if !cmd.write_data.is_empty() {
                        self.status_reg1 = cmd.write_data[0];
                    }
                    if cmd.write_data.len() >= 2 {
                        self.status_reg2 = cmd.write_data[1];
                    }
                    self.write_enabled = false;
                }
                Ok(())
            }

            // Write enable/disable
            opcodes::WREN => {
                self.write_enabled = true;
                Ok(())
            }
            opcodes::WRDI => {
                self.write_enabled = false;
                Ok(())
            }

            // Read commands
            opcodes::READ | opcodes::FAST_READ => self.handle_read(cmd),
            opcodes::READ_4B | opcodes::FAST_READ_4B => self.handle_read(cmd),

            // Page program
            opcodes::PP => self.handle_page_program(cmd),
            opcodes::PP_4B => self.handle_page_program(cmd),

            // Erase commands
            opcodes::SE_20 | opcodes::SE_21 => {
                self.handle_sector_erase(cmd, 4 * 1024)
            }
            opcodes::BE_52 | opcodes::BE_5C => {
                self.handle_sector_erase(cmd, 32 * 1024)
            }
            opcodes::BE_D8 | opcodes::BE_DC => {
                self.handle_sector_erase(cmd, 64 * 1024)
            }
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

    fn delay_us(&mut self, _us: u32) {
        // No delay needed for in-memory operations
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rflasher_core::protocol;

    #[test]
    fn test_read_jedec_id() {
        let mut flash = DummyFlash::new_default();
        let (mfr, dev) = protocol::read_jedec_id(&mut flash).unwrap();
        assert_eq!(mfr, 0xEF);
        assert_eq!(dev, 0x4018);
    }

    #[test]
    fn test_read_write() {
        let mut flash = DummyFlash::new_default();

        // Write some data
        let data = [0x12, 0x34, 0x56, 0x78];
        protocol::write_enable(&mut flash).unwrap();
        let mut cmd = SpiCommand::write_3b(opcodes::PP, 0x1000, &data);
        flash.execute(&mut cmd).unwrap();

        // Read it back
        let mut buf = [0u8; 4];
        let mut cmd = SpiCommand::read_3b(opcodes::READ, 0x1000, &mut buf);
        flash.execute(&mut cmd).unwrap();

        assert_eq!(buf, data);
    }

    #[test]
    fn test_erase() {
        let mut flash = DummyFlash::new_default();

        // Write some data
        let data = [0x00u8; 256];
        protocol::write_enable(&mut flash).unwrap();
        let mut cmd = SpiCommand::write_3b(opcodes::PP, 0, &data);
        flash.execute(&mut cmd).unwrap();

        // Verify it's written
        let mut buf = [0xFFu8; 256];
        let mut cmd = SpiCommand::read_3b(opcodes::READ, 0, &mut buf);
        flash.execute(&mut cmd).unwrap();
        assert_eq!(buf, data);

        // Erase the sector
        protocol::write_enable(&mut flash).unwrap();
        let mut cmd = SpiCommand::erase_3b(opcodes::SE_20, 0);
        flash.execute(&mut cmd).unwrap();

        // Verify it's erased
        let mut buf = [0x00u8; 256];
        let mut cmd = SpiCommand::read_3b(opcodes::READ, 0, &mut buf);
        flash.execute(&mut cmd).unwrap();
        assert!(buf.iter().all(|&b| b == 0xFF));
    }
}

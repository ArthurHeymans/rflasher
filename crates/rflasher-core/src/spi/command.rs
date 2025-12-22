//! SPI command structure

use super::{AddressWidth, IoMode};

/// A single SPI transaction
///
/// Designed to avoid allocation - uses slices for data.
/// The lifetime parameter `'a` ties the command to the buffers it references.
pub struct SpiCommand<'a> {
    /// The opcode byte
    pub opcode: u8,

    /// Address (if any)
    pub address: Option<u32>,

    /// Address width
    pub address_width: AddressWidth,

    /// I/O mode
    pub io_mode: IoMode,

    /// Number of dummy cycles after address
    pub dummy_cycles: u8,

    /// Data to write after opcode/address/dummy
    pub write_data: &'a [u8],

    /// Buffer to read into (mutable)
    pub read_buf: &'a mut [u8],
}

impl<'a> SpiCommand<'a> {
    /// Create a simple command with no address or data (e.g., WREN, WRDI)
    pub fn simple(opcode: u8) -> Self {
        Self {
            opcode,
            address: None,
            address_width: AddressWidth::None,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: &[],
            read_buf: &mut [],
        }
    }

    /// Create a read register command with no address (e.g., RDSR)
    pub fn read_reg(opcode: u8, buf: &'a mut [u8]) -> Self {
        Self {
            opcode,
            address: None,
            address_width: AddressWidth::None,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: &[],
            read_buf: buf,
        }
    }

    /// Create a write register command with no address (e.g., WRSR)
    pub fn write_reg(opcode: u8, data: &'a [u8]) -> Self {
        Self {
            opcode,
            address: None,
            address_width: AddressWidth::None,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: data,
            read_buf: &mut [],
        }
    }

    /// Create a read command with 3-byte address (e.g., READ)
    pub fn read_3b(opcode: u8, addr: u32, buf: &'a mut [u8]) -> Self {
        Self {
            opcode,
            address: Some(addr),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: &[],
            read_buf: buf,
        }
    }

    /// Create a read command with 4-byte address
    pub fn read_4b(opcode: u8, addr: u32, buf: &'a mut [u8]) -> Self {
        Self {
            opcode,
            address: Some(addr),
            address_width: AddressWidth::FourByte,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: &[],
            read_buf: buf,
        }
    }

    /// Create a write command with 3-byte address (e.g., PP)
    pub fn write_3b(opcode: u8, addr: u32, data: &'a [u8]) -> Self {
        Self {
            opcode,
            address: Some(addr),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: data,
            read_buf: &mut [],
        }
    }

    /// Create a write command with 4-byte address
    pub fn write_4b(opcode: u8, addr: u32, data: &'a [u8]) -> Self {
        Self {
            opcode,
            address: Some(addr),
            address_width: AddressWidth::FourByte,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: data,
            read_buf: &mut [],
        }
    }

    /// Create an erase command with 3-byte address
    pub fn erase_3b(opcode: u8, addr: u32) -> Self {
        Self {
            opcode,
            address: Some(addr),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: &[],
            read_buf: &mut [],
        }
    }

    /// Create an erase command with 4-byte address
    pub fn erase_4b(opcode: u8, addr: u32) -> Self {
        Self {
            opcode,
            address: Some(addr),
            address_width: AddressWidth::FourByte,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: &[],
            read_buf: &mut [],
        }
    }

    /// Set the I/O mode for this command
    pub fn with_io_mode(mut self, mode: IoMode) -> Self {
        self.io_mode = mode;
        self
    }

    /// Set the number of dummy cycles
    pub fn with_dummy_cycles(mut self, cycles: u8) -> Self {
        self.dummy_cycles = cycles;
        self
    }

    /// Returns true if this command has a read phase
    pub fn has_read(&self) -> bool {
        !self.read_buf.is_empty()
    }

    /// Returns true if this command has a write phase
    pub fn has_write(&self) -> bool {
        !self.write_data.is_empty()
    }

    /// Returns true if this command has an address phase
    pub fn has_address(&self) -> bool {
        self.address.is_some()
    }

    /// Calculate the total number of bytes to transfer (for timing/buffer allocation)
    pub fn total_bytes(&self) -> usize {
        let mut total = 1; // opcode
        total += self.address_width.bytes() as usize;
        // Dummy cycles are in clock cycles, not bytes - depends on I/O mode
        total += (self.dummy_cycles as usize) / 8;
        total += self.write_data.len();
        total += self.read_buf.len();
        total
    }
}

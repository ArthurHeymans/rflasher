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

    /// Calculate the number of wire bytes occupied by the dummy phase.
    ///
    /// Dummy cycles are clock cycles transferred over the address-phase lines
    /// (1 line for 1-1-x, 2 lines for 1-2-2, 4 lines for 1-4-4 / 4-4-4).
    pub fn dummy_bytes(&self) -> usize {
        (u16::from(self.dummy_cycles) * u16::from(self.io_mode.addr_lines())).div_ceil(8) as usize
    }

    /// Calculate the total number of bytes to transfer (for timing/buffer allocation)
    pub fn total_bytes(&self) -> usize {
        let mut total = 1; // opcode
        total += self.address_width.bytes() as usize;
        total += self.dummy_bytes();
        total += self.write_data.len();
        total += self.read_buf.len();
        total
    }

    /// Calculate the header length (opcode + address + dummy bytes)
    ///
    /// This is useful for pre-allocating buffers in programmer implementations.
    pub fn header_len(&self) -> usize {
        1 + self.address_width.bytes() as usize + self.dummy_bytes()
    }

    /// Encode the command header (opcode + address + dummy bytes) into a buffer
    ///
    /// This is a convenience method for programmer implementations to avoid
    /// duplicating the address encoding logic.
    ///
    /// # Arguments
    /// * `buf` - Buffer to write the header into. Must be at least `header_len()` bytes.
    ///
    /// # Returns
    /// The number of bytes written to the buffer.
    ///
    /// # Panics
    /// Panics if the buffer is too small.
    pub fn encode_header(&self, buf: &mut [u8]) -> usize {
        let header_len = self.header_len();
        assert!(
            buf.len() >= header_len,
            "buffer too small: need {} bytes, got {}",
            header_len,
            buf.len()
        );

        let mut offset = 0;

        // Opcode
        buf[offset] = self.opcode;
        offset += 1;

        // Address (if present)
        if let Some(addr) = self.address {
            self.address_width.encode(addr, &mut buf[offset..]);
            offset += self.address_width.bytes() as usize;
        }

        // Dummy bytes (filled with 0xFF)
        let dummy_bytes = self.dummy_bytes();
        buf[offset..offset + dummy_bytes].fill(0xFF);
        offset += dummy_bytes;

        offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dummy_bytes_accounts_for_io_mode_bus_width() {
        let mut buf = [0u8; 4];

        // Single I/O (1 line): 8 dummy cycles = 1 byte
        let single = SpiCommand::read_3b(0x0B, 0x123456, &mut buf).with_dummy_cycles(8);
        assert_eq!(single.dummy_bytes(), 1);
        assert_eq!(single.header_len(), 5);

        // DualOut (1-1-2): address/dummy phase is 1 line, 8 cycles = 1 byte
        let dual_out = SpiCommand {
            opcode: 0x3B,
            address: Some(0x123456),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::DualOut,
            dummy_cycles: 8,
            write_data: &[],
            read_buf: &mut buf,
        };
        assert_eq!(dual_out.dummy_bytes(), 1);

        // DualIo (1-2-2): address/dummy phase is 2 lines, 4 cycles = 1 byte
        let dual_io = SpiCommand {
            opcode: 0xBB,
            address: Some(0x123456),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::DualIo,
            dummy_cycles: 4,
            write_data: &[],
            read_buf: &mut buf,
        };
        assert_eq!(dual_io.dummy_bytes(), 1);

        // QuadIo (1-4-4): address/dummy phase is 4 lines, 6 cycles = 3 bytes
        let quad_io = SpiCommand {
            opcode: 0xEB,
            address: Some(0x123456),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::QuadIo,
            dummy_cycles: 6,
            write_data: &[],
            read_buf: &mut buf,
        };
        assert_eq!(quad_io.dummy_bytes(), 3);
        assert_eq!(quad_io.header_len(), 1 + 3 + 3);

        let mut hdr = [0u8; 7];
        assert_eq!(quad_io.encode_header(&mut hdr), 7);
        assert_eq!(hdr, [0xEB, 0x12, 0x34, 0x56, 0xFF, 0xFF, 0xFF]);
    }
}

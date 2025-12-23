//! Serprog protocol constants and types
//!
//! Based on the Serial Flasher Protocol Specification version 1.

/// Protocol version we support
pub const SERPROG_PROTOCOL_VERSION: u16 = 1;

/// ACK response byte
pub const S_ACK: u8 = 0x06;
/// NAK response byte
pub const S_NAK: u8 = 0x15;

// Command opcodes
/// No operation
pub const S_CMD_NOP: u8 = 0x00;
/// Query interface version
pub const S_CMD_Q_IFACE: u8 = 0x01;
/// Query supported commands bitmap
pub const S_CMD_Q_CMDMAP: u8 = 0x02;
/// Query programmer name
pub const S_CMD_Q_PGMNAME: u8 = 0x03;
/// Query serial buffer size
pub const S_CMD_Q_SERBUF: u8 = 0x04;
/// Query supported bustypes
pub const S_CMD_Q_BUSTYPE: u8 = 0x05;
/// Query connected address lines
pub const S_CMD_Q_CHIPSIZE: u8 = 0x06;
/// Query operation buffer size
pub const S_CMD_Q_OPBUF: u8 = 0x07;
/// Query maximum write-n length
pub const S_CMD_Q_WRNMAXLEN: u8 = 0x08;
/// Read a single byte
pub const S_CMD_R_BYTE: u8 = 0x09;
/// Read n bytes
pub const S_CMD_R_NBYTES: u8 = 0x0A;
/// Initialize operation buffer
pub const S_CMD_O_INIT: u8 = 0x0B;
/// Write to opbuf: Write byte with address
pub const S_CMD_O_WRITEB: u8 = 0x0C;
/// Write to opbuf: Write-N
pub const S_CMD_O_WRITEN: u8 = 0x0D;
/// Write opbuf: delay
pub const S_CMD_O_DELAY: u8 = 0x0E;
/// Execute operation buffer
pub const S_CMD_O_EXEC: u8 = 0x0F;
/// Special no-operation that returns NAK+ACK (for synchronization)
pub const S_CMD_SYNCNOP: u8 = 0x10;
/// Query maximum read-n length
pub const S_CMD_Q_RDNMAXLEN: u8 = 0x11;
/// Set used bustype(s)
pub const S_CMD_S_BUSTYPE: u8 = 0x12;
/// Perform SPI operation
pub const S_CMD_O_SPIOP: u8 = 0x13;
/// Set SPI clock frequency
pub const S_CMD_S_SPI_FREQ: u8 = 0x14;
/// Enable/disable output drivers
pub const S_CMD_S_PIN_STATE: u8 = 0x15;
/// Set SPI chip select to use
pub const S_CMD_S_SPI_CS: u8 = 0x16;
/// Set SPI mode (half/full duplex)
pub const S_CMD_S_SPI_MODE: u8 = 0x17;
/// Set CS mode (auto/selected/deselected)
pub const S_CMD_S_CS_MODE: u8 = 0x18;

/// Number of bytes in the command map bitmap
pub const CMDMAP_SIZE: usize = 32;

/// Bus type flags
pub mod bus {
    /// Parallel bus
    pub const PARALLEL: u8 = 1 << 0;
    /// LPC bus
    pub const LPC: u8 = 1 << 1;
    /// FWH bus
    pub const FWH: u8 = 1 << 2;
    /// SPI bus
    pub const SPI: u8 = 1 << 3;
    /// Non-SPI buses (PARALLEL | LPC | FWH)
    pub const NONSPI: u8 = PARALLEL | LPC | FWH;
}

/// SPI modes
pub mod spi_mode {
    /// Half duplex (default)
    pub const HALF_DUPLEX: u8 = 0x00;
    /// Full duplex
    pub const FULL_DUPLEX: u8 = 0x01;
}

/// CS modes
pub mod cs_mode {
    /// Auto mode - CS selected before O_SPIOP and deselected after (default)
    pub const AUTO: u8 = 0x00;
    /// CS selected until another mode is set
    pub const SELECTED: u8 = 0x01;
    /// CS deselected until another mode is set
    pub const DESELECTED: u8 = 0x02;
}

/// Supported commands bitmap
#[derive(Debug, Clone)]
pub struct CommandMap {
    /// Raw bitmap of supported commands
    pub bitmap: [u8; CMDMAP_SIZE],
}

impl CommandMap {
    /// Create an empty command map
    pub fn new() -> Self {
        Self {
            bitmap: [0; CMDMAP_SIZE],
        }
    }

    /// Check if a command is supported
    pub fn is_supported(&self, cmd: u8) -> bool {
        let byte_idx = (cmd / 8) as usize;
        let bit_idx = cmd % 8;
        if byte_idx >= CMDMAP_SIZE {
            return false;
        }
        (self.bitmap[byte_idx] & (1 << bit_idx)) != 0
    }

    /// Set a command as supported
    pub fn set_supported(&mut self, cmd: u8) {
        let byte_idx = (cmd / 8) as usize;
        let bit_idx = cmd % 8;
        if byte_idx < CMDMAP_SIZE {
            self.bitmap[byte_idx] |= 1 << bit_idx;
        }
    }
}

impl Default for CommandMap {
    fn default() -> Self {
        Self::new()
    }
}

/// Programmer capabilities discovered during initialization
#[derive(Debug, Clone)]
pub struct ProgrammerInfo {
    /// Programmer name (up to 16 characters)
    pub name: [u8; 16],
    /// Supported bus types
    pub bustypes: u8,
    /// Serial buffer size
    pub serbuf_size: u16,
    /// Operation buffer size
    pub opbuf_size: u16,
    /// Maximum write-n length (0 = 2^24)
    pub max_write_n: u32,
    /// Maximum read-n length (0 = 2^24)
    pub max_read_n: u32,
    /// Supported commands bitmap
    pub cmdmap: CommandMap,
}

impl Default for ProgrammerInfo {
    fn default() -> Self {
        Self {
            name: [0; 16],
            bustypes: 0,
            serbuf_size: 16,
            opbuf_size: 300,
            max_write_n: 0,
            max_read_n: 0,
            cmdmap: CommandMap::new(),
        }
    }
}

impl ProgrammerInfo {
    /// Get the programmer name as a string
    pub fn name_str(&self) -> &str {
        // Find null terminator or end
        let len = self.name.iter().position(|&c| c == 0).unwrap_or(16);
        core::str::from_utf8(&self.name[..len]).unwrap_or("(invalid)")
    }

    /// Get the effective max write length
    pub fn effective_max_write(&self) -> usize {
        if self.max_write_n == 0 {
            (1 << 24) - 1
        } else {
            self.max_write_n as usize
        }
    }

    /// Get the effective max read length
    pub fn effective_max_read(&self) -> usize {
        if self.max_read_n == 0 {
            (1 << 24) - 1
        } else {
            self.max_read_n as usize
        }
    }

    /// Check if SPI bus is supported
    pub fn supports_spi(&self) -> bool {
        (self.bustypes & bus::SPI) != 0
    }

    /// Check if a command is supported
    pub fn supports_cmd(&self, cmd: u8) -> bool {
        self.cmdmap.is_supported(cmd)
    }
}

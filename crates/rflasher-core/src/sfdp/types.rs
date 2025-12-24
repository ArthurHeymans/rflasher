//! SFDP type definitions
//!
//! Types representing SFDP structures as defined by JEDEC JESD216H.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

/// SFDP signature magic value ("SFDP" in little-endian)
pub const SFDP_SIGNATURE: u32 = 0x50444653;

/// Maximum number of parameter headers to support
pub const MAX_PARAMETER_HEADERS: usize = 16;

/// Maximum parameter table size in bytes (256 DWORDs * 4)
pub const MAX_PARAMETER_TABLE_SIZE: usize = 1024;

// ============================================================================
// Parameter IDs (MSB << 8 | LSB)
// ============================================================================

/// Basic Flash Parameter Table ID
pub const PARAM_ID_BASIC: u16 = 0xFF00;
/// Sector Map Parameter Table ID
pub const PARAM_ID_SECTOR_MAP: u16 = 0xFF81;
/// 4-byte Address Instruction Table ID
pub const PARAM_ID_4BYTE_ADDR: u16 = 0xFF84;
/// xSPI Profile 1.0 Parameter Table ID
pub const PARAM_ID_XSPI_1_0: u16 = 0xFF05;
/// Status/Control/Config Register Map ID
pub const PARAM_ID_SCCR_MAP: u16 = 0xFF87;

// ============================================================================
// Fast Read Parameters
// ============================================================================

/// Parameters for a fast read command
///
/// Contains the opcode, number of mode clocks, and number of dummy/wait cycles
/// needed for a specific fast read mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FastReadParams {
    /// Instruction opcode (0x00 if not supported)
    pub opcode: u8,
    /// Number of mode clock cycles
    pub mode_clocks: u8,
    /// Number of dummy/wait clock cycles before valid output
    pub dummy_clocks: u8,
}

impl FastReadParams {
    /// Create new fast read parameters
    pub const fn new(opcode: u8, mode_clocks: u8, dummy_clocks: u8) -> Self {
        Self {
            opcode,
            mode_clocks,
            dummy_clocks,
        }
    }

    /// Check if this fast read mode is supported
    pub fn is_supported(&self) -> bool {
        self.opcode != 0x00
    }

    /// Parse from DWORD half (instruction in high byte, mode/dummy in lower bits)
    ///
    /// Layout: [31:24] instruction, [23:21] mode clocks, [20:16] dummy clocks
    pub fn from_high_half(dword: u32) -> Self {
        let opcode = ((dword >> 24) & 0xFF) as u8;
        let mode_clocks = ((dword >> 21) & 0x07) as u8;
        let dummy_clocks = ((dword >> 16) & 0x1F) as u8;

        if opcode == 0x00 {
            Self::default()
        } else {
            Self::new(opcode, mode_clocks, dummy_clocks)
        }
    }

    /// Parse from DWORD low half
    ///
    /// Layout: [15:8] instruction, [7:5] mode clocks, [4:0] dummy clocks
    pub fn from_low_half(dword: u32) -> Self {
        let opcode = ((dword >> 8) & 0xFF) as u8;
        let mode_clocks = ((dword >> 5) & 0x07) as u8;
        let dummy_clocks = (dword & 0x1F) as u8;

        if opcode == 0x00 {
            Self::default()
        } else {
            Self::new(opcode, mode_clocks, dummy_clocks)
        }
    }
}

// ============================================================================
// SFDP Revision
// ============================================================================

/// SFDP revision information
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SfdpRevision {
    /// Major revision number
    pub major: u8,
    /// Minor revision number
    pub minor: u8,
}

impl SfdpRevision {
    /// Create a new revision
    pub const fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }

    /// Check if this revision is at least the specified version
    pub fn at_least(&self, major: u8, minor: u8) -> bool {
        self.major > major || (self.major == major && self.minor >= minor)
    }

    /// JESD216 (original, 9 DWORDs)
    pub const JESD216: Self = Self::new(1, 0);
    /// JESD216A (added 4-byte address table)
    pub const JESD216A: Self = Self::new(1, 5);
    /// JESD216B (16 DWORDs, added QE requirements)
    pub const JESD216B: Self = Self::new(1, 6);
    /// JESD216C (added xSPI)
    pub const JESD216C: Self = Self::new(1, 7);
    /// JESD216D (20 DWORDs)
    pub const JESD216D: Self = Self::new(1, 8);
    /// JESD216F (23 DWORDs)
    pub const JESD216F: Self = Self::new(1, 9);
}

impl core::fmt::Display for SfdpRevision {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

// ============================================================================
// SFDP Header
// ============================================================================

/// SFDP header structure (first 8 bytes at address 0x00)
#[derive(Debug, Clone, Copy, Default)]
pub struct SfdpHeader {
    /// SFDP signature (should be 0x50444653)
    pub signature: u32,
    /// SFDP revision
    pub revision: SfdpRevision,
    /// Number of parameter headers (0-based, so actual count is nph + 1)
    pub nph: u8,
    /// Access protocol (0xFF for legacy)
    pub access_protocol: u8,
}

impl SfdpHeader {
    /// Parse SFDP header from raw bytes
    ///
    /// Expects 8 bytes in little-endian format.
    pub fn parse(data: &[u8; 8]) -> Self {
        Self {
            signature: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            revision: SfdpRevision {
                minor: data[4],
                major: data[5],
            },
            nph: data[6],
            access_protocol: data[7],
        }
    }

    /// Check if the signature is valid
    pub fn is_valid(&self) -> bool {
        self.signature == SFDP_SIGNATURE
    }

    /// Get the number of parameter headers
    pub fn num_param_headers(&self) -> usize {
        (self.nph as usize) + 1
    }
}

// ============================================================================
// Parameter Header
// ============================================================================

/// Parameter header structure (8 bytes each, starting at address 0x08)
#[derive(Debug, Clone, Copy, Default)]
pub struct ParameterHeader {
    /// Parameter ID (MSB << 8 | LSB)
    pub id: u16,
    /// Parameter table revision
    pub revision: SfdpRevision,
    /// Parameter table length in DWORDs
    pub length_dwords: u8,
    /// Parameter table pointer (24-bit byte address)
    pub table_pointer: u32,
}

impl ParameterHeader {
    /// Parse a parameter header from raw bytes
    ///
    /// Expects 8 bytes in little-endian format.
    pub fn parse(data: &[u8; 8]) -> Self {
        Self {
            id: ((data[7] as u16) << 8) | (data[0] as u16),
            revision: SfdpRevision {
                minor: data[1],
                major: data[2],
            },
            length_dwords: data[3],
            table_pointer: u32::from_le_bytes([data[4], data[5], data[6], 0]),
        }
    }

    /// Get the table length in bytes
    pub fn length_bytes(&self) -> usize {
        (self.length_dwords as usize) * 4
    }

    /// Check if this is the Basic Flash Parameter Table
    pub fn is_basic(&self) -> bool {
        self.id == PARAM_ID_BASIC
    }

    /// Check if this is a JEDEC-defined table (MSB >= 0x80)
    pub fn is_jedec(&self) -> bool {
        (self.id >> 8) >= 0x80
    }
}

// ============================================================================
// Address Mode
// ============================================================================

/// Flash addressing mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddressMode {
    /// 3-byte addressing only (up to 16 MiB)
    #[default]
    ThreeByteOnly,
    /// 3-byte default, can switch to 4-byte
    ThreeOrFourByte,
    /// 4-byte addressing only (required for > 16 MiB)
    FourByteOnly,
}

impl AddressMode {
    /// Parse from BFPT DWORD 1 bits [18:17]
    pub fn from_bfpt(value: u8) -> Self {
        match value & 0x03 {
            0b00 => Self::ThreeByteOnly,
            0b01 => Self::ThreeOrFourByte,
            0b10 => Self::FourByteOnly,
            _ => Self::ThreeByteOnly, // Reserved, treat as 3-byte
        }
    }

    /// Check if 4-byte addressing is required
    pub fn requires_4byte(&self) -> bool {
        matches!(self, Self::FourByteOnly)
    }

    /// Check if 4-byte addressing is supported
    pub fn supports_4byte(&self) -> bool {
        !matches!(self, Self::ThreeByteOnly)
    }
}

// ============================================================================
// Erase Type
// ============================================================================

/// Erase type from SFDP (up to 4 types supported)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SfdpEraseType {
    /// Erase opcode
    pub opcode: u8,
    /// Erase size in bytes (0 if not supported)
    pub size: u32,
}

impl SfdpEraseType {
    /// Check if this erase type is valid/supported
    pub fn is_valid(&self) -> bool {
        self.size > 0 && self.opcode != 0xFF
    }

    /// Parse from size exponent (N where size = 2^N) and opcode
    pub fn from_raw(size_exp: u8, opcode: u8) -> Self {
        if size_exp == 0 || opcode == 0xFF {
            Self::default()
        } else {
            Self {
                opcode,
                size: 1u32 << size_exp,
            }
        }
    }
}

// ============================================================================
// Write Enable Requirement
// ============================================================================

/// Write enable instruction required before volatile status register write
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WriteEnableForVolatileSr {
    /// Use WREN (0x06) instruction
    #[default]
    Wren,
    /// Use EWSR (0x50) instruction
    Ewsr,
}

// ============================================================================
// Quad Enable (QE) Requirements
// ============================================================================

/// Quad Enable (QE) bit location and method
///
/// Different manufacturers use different methods to enable quad I/O mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(non_camel_case_types)]
pub enum QuadEnableRequirement {
    /// No QE bit; device does not have a QE bit
    #[default]
    None,
    /// QE is bit 1 of SR2; write SR1 and SR2 with 0x01 (2 bytes)
    Sr2Bit1_WriteCmd01,
    /// QE is bit 6 of SR1; write SR1 with 0x01 (1 byte)
    Sr1Bit6_WriteCmd01,
    /// QE is bit 7 of SR2; write SR2 with 0x3E, read with 0x3F
    Sr2Bit7_WriteCmdSpecial,
    /// QE is bit 1 of SR2; write SR2 with 0x31
    Sr2Bit1_WriteCmd31,
    /// QE is bit 1 of SR2; write SR1 and SR2 with 0x01; status read with 0x05/0x35
    Sr2Bit1_WriteCmd01_StatusSplit,
}

impl QuadEnableRequirement {
    /// Parse from BFPT DWORD 15 bits [22:20]
    pub fn from_bfpt(value: u8) -> Self {
        match value & 0x07 {
            0b000 => Self::None,
            0b001 => Self::Sr2Bit1_WriteCmd01,
            0b010 => Self::Sr1Bit6_WriteCmd01,
            0b011 => Self::Sr2Bit7_WriteCmdSpecial,
            0b100 => Self::Sr2Bit1_WriteCmd31,
            0b101 => Self::Sr2Bit1_WriteCmd01_StatusSplit,
            _ => Self::None,
        }
    }

    /// Check if quad enable is needed
    pub fn is_needed(&self) -> bool {
        !matches!(self, Self::None)
    }
}

// ============================================================================
// 4-Byte Address Entry Methods
// ============================================================================

/// Methods to enter 4-byte address mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FourByteEntryMethods {
    /// Bit field of supported methods
    pub methods: u8,
}

impl FourByteEntryMethods {
    /// Enter with instruction 0xB7, exit with 0xE9
    pub const INSTR_B7_E9: u8 = 0x01;
    /// Enter with WREN + instruction 0xB7, exit with WREN + 0xE9
    pub const WREN_INSTR_B7_E9: u8 = 0x02;
    /// 8-bit volatile extended address register with 0xC5/0xC8
    pub const EXT_ADDR_REG: u8 = 0x04;
    /// 8-bit volatile bank register
    pub const BANK_REG: u8 = 0x08;
    /// Always operates in 4-byte mode
    pub const ALWAYS_4BYTE: u8 = 0x10;

    /// Parse from BFPT DWORD 16 bits [31:24]
    pub fn from_bfpt(value: u8) -> Self {
        Self { methods: value }
    }

    /// Check if a specific method is supported
    pub fn supports(&self, method: u8) -> bool {
        (self.methods & method) != 0
    }

    /// Check if always in 4-byte mode
    pub fn always_4byte(&self) -> bool {
        self.supports(Self::ALWAYS_4BYTE)
    }
}

// ============================================================================
// Soft Reset Support
// ============================================================================

/// Soft reset sequence support
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SoftResetSupport {
    /// Bit field of supported reset methods
    pub methods: u8,
}

impl SoftResetSupport {
    /// 0x66 (Reset Enable) + 0x99 (Reset)
    pub const RESET_66_99: u8 = 0x10;
    /// 0xF0 instruction
    pub const INSTR_F0: u8 = 0x08;
    /// Exit 0-4-4 mode required before reset
    pub const EXIT_044_REQUIRED: u8 = 0x01;

    /// Parse from BFPT DWORD 16 bits [13:8]
    pub fn from_bfpt(value: u8) -> Self {
        Self {
            methods: value & 0x3F,
        }
    }

    /// Check if 66/99 reset sequence is supported
    pub fn supports_66_99(&self) -> bool {
        (self.methods & Self::RESET_66_99) != 0
    }
}

// ============================================================================
// Basic Flash Parameter Table (BFPT)
// ============================================================================

/// Parsed Basic Flash Parameter Table
///
/// Contains the key information extracted from the BFPT.
#[derive(Debug, Clone, Default)]
pub struct BasicFlashParams {
    /// Parameter table revision
    pub revision: SfdpRevision,
    /// Flash density in bytes
    pub density_bytes: u64,
    /// Page size in bytes (for programming)
    pub page_size: u32,
    /// Address mode support
    pub address_mode: AddressMode,
    /// Erase types (up to 4)
    pub erase_types: [SfdpEraseType; 4],
    /// Write enable for volatile status register
    pub volatile_sr_write_enable: WriteEnableForVolatileSr,
    /// Status register is volatile
    pub status_reg_volatile: bool,
    /// Write granularity (true = 64+ bytes, false = 1 byte)
    pub write_granularity_64: bool,

    // Fast read support (from DWORD 1)
    /// Supports 1-1-2 fast read
    pub fast_read_112: bool,
    /// Supports 1-2-2 fast read
    pub fast_read_122: bool,
    /// Supports 1-1-4 fast read
    pub fast_read_114: bool,
    /// Supports 1-4-4 fast read
    pub fast_read_144: bool,
    /// Supports 2-2-2 fast read
    pub fast_read_222: bool,
    /// Supports 4-4-4 fast read
    pub fast_read_444: bool,
    /// Supports DTR clocking
    pub dtr_clocking: bool,

    // Advanced features (JESD216B+, DWORD 15-16)
    /// Quad enable requirements
    pub quad_enable: QuadEnableRequirement,
    /// 4-byte address entry methods
    pub four_byte_entry: FourByteEntryMethods,
    /// Soft reset support
    pub soft_reset: SoftResetSupport,

    /// 4KB erase opcode (0xFF if not supported)
    pub erase_4k_opcode: u8,

    // Fast read instruction parameters (JESD216, DWORDs 3, 4, 6, 7)
    /// 1S-1S-4S fast read parameters (DWORD 3 high)
    pub fast_read_114_params: FastReadParams,
    /// 1S-4S-4S fast read parameters (DWORD 3 low)
    pub fast_read_144_params: FastReadParams,
    /// 1S-2S-2S fast read parameters (DWORD 4 high)
    pub fast_read_122_params: FastReadParams,
    /// 1S-1S-2S fast read parameters (DWORD 4 low)
    pub fast_read_112_params: FastReadParams,
    /// 2S-2S-2S fast read parameters (DWORD 6 high)
    pub fast_read_222_params: FastReadParams,
    /// 4S-4S-4S fast read parameters (DWORD 7 high)
    pub fast_read_444_params: FastReadParams,
}

impl BasicFlashParams {
    /// Get the smallest supported erase size
    pub fn min_erase_size(&self) -> Option<u32> {
        self.erase_types
            .iter()
            .filter(|e| e.is_valid())
            .map(|e| e.size)
            .min()
    }

    /// Get the largest supported erase size
    pub fn max_erase_size(&self) -> Option<u32> {
        self.erase_types
            .iter()
            .filter(|e| e.is_valid())
            .map(|e| e.size)
            .max()
    }

    /// Get erase type for a specific size
    pub fn erase_for_size(&self, size: u32) -> Option<&SfdpEraseType> {
        self.erase_types
            .iter()
            .find(|e| e.is_valid() && e.size == size)
    }

    /// Check if 4-byte addressing is required based on density
    pub fn requires_4byte_addr(&self) -> bool {
        self.density_bytes > 16 * 1024 * 1024 || self.address_mode.requires_4byte()
    }

    /// Get valid erase types sorted by size (smallest first)
    #[cfg(feature = "alloc")]
    pub fn sorted_erase_types(&self) -> Vec<SfdpEraseType> {
        let mut types: Vec<_> = self
            .erase_types
            .iter()
            .filter(|e| e.is_valid())
            .copied()
            .collect();
        types.sort_by_key(|e| e.size);
        types
    }
}

// ============================================================================
// 4-Byte Address Instruction Table
// ============================================================================

/// 4-Byte Address Instruction Table support flags (DWORD 1)
///
/// Indicates which commands are supported using native 4-byte addressing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FourByteAddrInstructions {
    /// Bit field of supported instructions
    pub flags: u32,
}

impl FourByteAddrInstructions {
    // Read instructions
    /// Support for 1S-1S-1S READ command (0x13)
    pub const READ_1S_1S_1S: u32 = 1 << 0;
    /// Support for 1S-1S-1S FAST_READ command (0x0C)
    pub const FAST_READ_1S_1S_1S: u32 = 1 << 1;
    /// Support for 1S-1S-2S FAST_READ command (0x3C)
    pub const FAST_READ_1S_1S_2S: u32 = 1 << 2;
    /// Support for 1S-2S-2S FAST_READ command (0xBC)
    pub const FAST_READ_1S_2S_2S: u32 = 1 << 3;
    /// Support for 1S-1S-4S FAST_READ command (0x6C)
    pub const FAST_READ_1S_1S_4S: u32 = 1 << 4;
    /// Support for 1S-4S-4S FAST_READ command (0xEC)
    pub const FAST_READ_1S_4S_4S: u32 = 1 << 5;
    /// Support for 1S-1S-1S PAGE_PROGRAM command (0x12)
    pub const PAGE_PROGRAM_1S_1S_1S: u32 = 1 << 6;
    /// Support for 1S-1S-4S PAGE_PROGRAM command (0x34)
    pub const PAGE_PROGRAM_1S_1S_4S: u32 = 1 << 7;
    /// Support for 1S-4S-4S PAGE_PROGRAM command (0x3E)
    pub const PAGE_PROGRAM_1S_4S_4S: u32 = 1 << 8;
    /// Support for Erase Type 1 with 4-byte address
    pub const ERASE_TYPE_1: u32 = 1 << 9;
    /// Support for Erase Type 2 with 4-byte address
    pub const ERASE_TYPE_2: u32 = 1 << 10;
    /// Support for Erase Type 3 with 4-byte address
    pub const ERASE_TYPE_3: u32 = 1 << 11;
    /// Support for Erase Type 4 with 4-byte address
    pub const ERASE_TYPE_4: u32 = 1 << 12;
    /// Support for 1S-1D-1D DTR_Read command (0x0E)
    pub const DTR_READ_1S_1D_1D: u32 = 1 << 13;
    /// Support for 1S-2D-2D DTR_Read command (0xBE)
    pub const DTR_READ_1S_2D_2D: u32 = 1 << 14;
    /// Support for 1S-4D-4D DTR_Read command (0xEE)
    pub const DTR_READ_1S_4D_4D: u32 = 1 << 15;
    /// Support for volatile individual sector lock read command (0xE0)
    pub const VOLATILE_SECTOR_LOCK_READ: u32 = 1 << 16;
    /// Support for volatile individual sector lock write command (0xE1)
    pub const VOLATILE_SECTOR_LOCK_WRITE: u32 = 1 << 17;
    /// Support for non-volatile individual sector lock read command (0xE2)
    pub const NONVOLATILE_SECTOR_LOCK_READ: u32 = 1 << 18;
    /// Support for non-volatile individual sector lock write command (0xE3)
    pub const NONVOLATILE_SECTOR_LOCK_WRITE: u32 = 1 << 19;
    /// Support for 1S-1S-8S FAST_READ command (0x7C)
    pub const FAST_READ_1S_1S_8S: u32 = 1 << 20;
    /// Support for 1S-8S-8S FAST_READ command (0xCC)
    pub const FAST_READ_1S_8S_8S: u32 = 1 << 21;
    /// Support for 1S-8D-8D DTR_READ command (0xFD)
    pub const DTR_READ_1S_8D_8D: u32 = 1 << 22;
    /// Support for 1S-1S-8S PAGE_PROGRAM command (0x84)
    pub const PAGE_PROGRAM_1S_1S_8S: u32 = 1 << 23;
    /// Support for 1S-8S-8S PAGE_PROGRAM command (0x8E)
    pub const PAGE_PROGRAM_1S_8S_8S: u32 = 1 << 24;

    /// Parse from DWORD 1 of 4-byte address instruction table
    pub fn from_dword1(dword: u32) -> Self {
        Self { flags: dword }
    }

    /// Check if a specific instruction is supported
    pub fn supports(&self, flag: u32) -> bool {
        (self.flags & flag) != 0
    }

    /// Check if native 4-byte read is supported
    pub fn supports_4ba_read(&self) -> bool {
        self.supports(Self::READ_1S_1S_1S)
    }

    /// Check if native 4-byte fast read is supported
    pub fn supports_4ba_fast_read(&self) -> bool {
        self.supports(Self::FAST_READ_1S_1S_1S)
    }

    /// Check if native 4-byte page program is supported
    pub fn supports_4ba_page_program(&self) -> bool {
        self.supports(Self::PAGE_PROGRAM_1S_1S_1S)
    }

    /// Check if any 4-byte erase is supported
    pub fn supports_any_4ba_erase(&self) -> bool {
        self.supports(Self::ERASE_TYPE_1)
            || self.supports(Self::ERASE_TYPE_2)
            || self.supports(Self::ERASE_TYPE_3)
            || self.supports(Self::ERASE_TYPE_4)
    }
}

/// 4-Byte Address Erase Instructions (DWORD 2)
///
/// Contains the 4-byte address erase opcodes for each erase type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FourByteAddrEraseOpcodes {
    /// Erase opcode for type 1 (0x00 if not supported)
    pub erase_type_1: u8,
    /// Erase opcode for type 2 (0x00 if not supported)
    pub erase_type_2: u8,
    /// Erase opcode for type 3 (0x00 if not supported)
    pub erase_type_3: u8,
    /// Erase opcode for type 4 (0x00 if not supported)
    pub erase_type_4: u8,
}

impl FourByteAddrEraseOpcodes {
    /// Parse from DWORD 2 of 4-byte address instruction table
    pub fn from_dword2(dword: u32) -> Self {
        Self {
            erase_type_1: (dword & 0xFF) as u8,
            erase_type_2: ((dword >> 8) & 0xFF) as u8,
            erase_type_3: ((dword >> 16) & 0xFF) as u8,
            erase_type_4: ((dword >> 24) & 0xFF) as u8,
        }
    }

    /// Get the 4-byte erase opcode for a given erase type index (0-3)
    pub fn opcode_for_type(&self, type_index: usize) -> Option<u8> {
        match type_index {
            0 => Some(self.erase_type_1).filter(|&o| o != 0),
            1 => Some(self.erase_type_2).filter(|&o| o != 0),
            2 => Some(self.erase_type_3).filter(|&o| o != 0),
            3 => Some(self.erase_type_4).filter(|&o| o != 0),
            _ => None,
        }
    }
}

/// Complete 4-Byte Address Instruction Table
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FourByteAddrTable {
    /// Table revision
    pub revision: SfdpRevision,
    /// Supported 4-byte address instructions
    pub instructions: FourByteAddrInstructions,
    /// 4-byte address erase opcodes
    pub erase_opcodes: FourByteAddrEraseOpcodes,
}

impl FourByteAddrTable {
    /// Standard 4-byte address read opcode
    pub const READ_4B_OPCODE: u8 = 0x13;
    /// Standard 4-byte address fast read opcode
    pub const FAST_READ_4B_OPCODE: u8 = 0x0C;
    /// Standard 4-byte address page program opcode
    pub const PAGE_PROGRAM_4B_OPCODE: u8 = 0x12;
    /// Standard 4-byte address dual output fast read opcode
    pub const FAST_READ_DUAL_OUT_4B_OPCODE: u8 = 0x3C;
    /// Standard 4-byte address dual I/O fast read opcode
    pub const FAST_READ_DUAL_IO_4B_OPCODE: u8 = 0xBC;
    /// Standard 4-byte address quad output fast read opcode
    pub const FAST_READ_QUAD_OUT_4B_OPCODE: u8 = 0x6C;
    /// Standard 4-byte address quad I/O fast read opcode
    pub const FAST_READ_QUAD_IO_4B_OPCODE: u8 = 0xEC;

    /// Get the 4-byte read opcode if supported
    pub fn read_opcode(&self) -> Option<u8> {
        if self.instructions.supports_4ba_read() {
            Some(Self::READ_4B_OPCODE)
        } else {
            None
        }
    }

    /// Get the 4-byte fast read opcode if supported
    pub fn fast_read_opcode(&self) -> Option<u8> {
        if self.instructions.supports_4ba_fast_read() {
            Some(Self::FAST_READ_4B_OPCODE)
        } else {
            None
        }
    }

    /// Get the 4-byte page program opcode if supported
    pub fn page_program_opcode(&self) -> Option<u8> {
        if self.instructions.supports_4ba_page_program() {
            Some(Self::PAGE_PROGRAM_4B_OPCODE)
        } else {
            None
        }
    }
}

// ============================================================================
// Complete SFDP Info
// ============================================================================

/// Complete SFDP information parsed from a flash chip
#[derive(Debug, Clone, Default)]
pub struct SfdpInfo {
    /// SFDP header
    pub header: SfdpHeader,
    /// Basic Flash Parameter Table
    pub basic_params: BasicFlashParams,
    /// Number of parameter headers found
    pub num_param_headers: usize,
    /// 4-Byte Address Instruction Table (if present)
    pub four_byte_addr_table: Option<FourByteAddrTable>,
}

impl SfdpInfo {
    /// Get the flash size in bytes
    pub fn total_size(&self) -> u64 {
        self.basic_params.density_bytes
    }

    /// Get the page size in bytes
    pub fn page_size(&self) -> u32 {
        self.basic_params.page_size
    }

    /// Check if SFDP data appears valid
    pub fn is_valid(&self) -> bool {
        self.header.is_valid() && self.basic_params.density_bytes > 0
    }
}

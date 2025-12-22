//! Standard JEDEC SPI flash opcodes
//!
//! This module defines the standard SPI flash command opcodes as specified
//! by JEDEC JESD216 (SFDP) and common manufacturer conventions.

// ============================================================================
// Write control
// ============================================================================

/// Write Enable - required before any write/erase operation
pub const WREN: u8 = 0x06;
/// Write Disable - clears WEL bit in status register
pub const WRDI: u8 = 0x04;
/// Enable Write Status Register (legacy SST command)
pub const EWSR: u8 = 0x50;

// ============================================================================
// Status register operations
// ============================================================================

/// Read Status Register 1
pub const RDSR: u8 = 0x05;
/// Read Status Register 2
pub const RDSR2: u8 = 0x35;
/// Read Status Register 3
pub const RDSR3: u8 = 0x15;
/// Write Status Register 1
pub const WRSR: u8 = 0x01;
/// Write Status Register 2
pub const WRSR2: u8 = 0x31;
/// Write Status Register 3
pub const WRSR3: u8 = 0x11;

// ============================================================================
// Identification
// ============================================================================

/// Read JEDEC ID (manufacturer + device ID)
pub const RDID: u8 = 0x9F;
/// Read Electronic Manufacturer & Device ID (legacy)
pub const REMS: u8 = 0x90;
/// Read Electronic Signature / Release from Deep Power Down
pub const RES: u8 = 0xAB;
/// Read Unique ID (Winbond, others)
pub const RDUID: u8 = 0x4B;

// ============================================================================
// Read commands - 3-byte address
// ============================================================================

/// Read Data (up to ~33 MHz)
pub const READ: u8 = 0x03;
/// Fast Read (with dummy byte, up to max frequency)
pub const FAST_READ: u8 = 0x0B;

// ============================================================================
// Read commands - 4-byte address
// ============================================================================

/// Read Data with 4-byte address
pub const READ_4B: u8 = 0x13;
/// Fast Read with 4-byte address
pub const FAST_READ_4B: u8 = 0x0C;

// ============================================================================
// Dual/Quad read - 3-byte address
// ============================================================================

/// Dual Output Read (1-1-2)
pub const DOR: u8 = 0x3B;
/// Dual I/O Read (1-2-2)
pub const DIOR: u8 = 0xBB;
/// Quad Output Read (1-1-4)
pub const QOR: u8 = 0x6B;
/// Quad I/O Read (1-4-4)
pub const QIOR: u8 = 0xEB;

// ============================================================================
// Dual/Quad read - 4-byte address
// ============================================================================

/// Dual Output Read with 4-byte address
pub const DOR_4B: u8 = 0x3C;
/// Dual I/O Read with 4-byte address
pub const DIOR_4B: u8 = 0xBC;
/// Quad Output Read with 4-byte address
pub const QOR_4B: u8 = 0x6C;
/// Quad I/O Read with 4-byte address
pub const QIOR_4B: u8 = 0xEC;

// ============================================================================
// Page Program
// ============================================================================

/// Page Program with 3-byte address
pub const PP: u8 = 0x02;
/// Page Program with 4-byte address
pub const PP_4B: u8 = 0x12;
/// Quad Page Program with 3-byte address
pub const QPP: u8 = 0x32;
/// Quad Page Program with 4-byte address
pub const QPP_4B: u8 = 0x34;

// ============================================================================
// Erase commands - 3-byte address
// ============================================================================

/// Sector Erase 4KB with 3-byte address
pub const SE_20: u8 = 0x20;
/// Block Erase 32KB with 3-byte address
pub const BE_52: u8 = 0x52;
/// Block Erase 64KB with 3-byte address
pub const BE_D8: u8 = 0xD8;
/// Chip Erase (entire chip)
pub const CE_60: u8 = 0x60;
/// Chip Erase (alternate opcode)
pub const CE_C7: u8 = 0xC7;

// ============================================================================
// Erase commands - 4-byte address
// ============================================================================

/// Sector Erase 4KB with 4-byte address
pub const SE_21: u8 = 0x21;
/// Block Erase 32KB with 4-byte address
pub const BE_5C: u8 = 0x5C;
/// Block Erase 64KB with 4-byte address
pub const BE_DC: u8 = 0xDC;

// ============================================================================
// 4-byte address mode control
// ============================================================================

/// Enter 4-Byte Address Mode
pub const EN4B: u8 = 0xB7;
/// Exit 4-Byte Address Mode
pub const EX4B: u8 = 0xE9;
/// Read Extended Address Register
pub const RDEAR: u8 = 0xC8;
/// Write Extended Address Register
pub const WREAR: u8 = 0xC5;

// ============================================================================
// Power management
// ============================================================================

/// Deep Power Down
pub const DP: u8 = 0xB9;
/// Release from Deep Power Down (same as RES)
pub const RDP: u8 = 0xAB;

// ============================================================================
// Security register operations
// ============================================================================

/// Erase Security Register
pub const ERSR: u8 = 0x44;
/// Program Security Register
pub const PRSR: u8 = 0x42;
/// Read Security Register
pub const RDSR_SEC: u8 = 0x48;

// ============================================================================
// QPI mode control
// ============================================================================

/// Enter QPI Mode (Winbond)
pub const EQIO: u8 = 0x38;
/// Reset QPI Mode / Exit QPI Mode
pub const RSTQIO: u8 = 0xFF;

// ============================================================================
// Software Reset
// ============================================================================

/// Reset Enable
pub const RSTEN: u8 = 0x66;
/// Reset Device
pub const RST: u8 = 0x99;

// ============================================================================
// SFDP (Serial Flash Discoverable Parameters)
// ============================================================================

/// Read SFDP (JEDEC JESD216)
pub const RDSFDP: u8 = 0x5A;

// ============================================================================
// Suspend/Resume
// ============================================================================

/// Erase/Program Suspend
pub const SUSPEND: u8 = 0x75;
/// Erase/Program Resume
pub const RESUME: u8 = 0x7A;

// ============================================================================
// Status register bit definitions
// ============================================================================

/// Status Register 1: Write In Progress / Busy
pub const SR1_WIP: u8 = 0x01;
/// Status Register 1: Write Enable Latch
pub const SR1_WEL: u8 = 0x02;
/// Status Register 1: Block Protect bit 0
pub const SR1_BP0: u8 = 0x04;
/// Status Register 1: Block Protect bit 1
pub const SR1_BP1: u8 = 0x08;
/// Status Register 1: Block Protect bit 2
pub const SR1_BP2: u8 = 0x10;
/// Status Register 1: Top/Bottom Protect
pub const SR1_TB: u8 = 0x20;
/// Status Register 1: Sector/Block Protect
pub const SR1_SEC: u8 = 0x40;
/// Status Register 1: Status Register Protect 0
pub const SR1_SRP0: u8 = 0x80;

/// Status Register 2: Status Register Protect 1
pub const SR2_SRP1: u8 = 0x01;
/// Status Register 2: Quad Enable
pub const SR2_QE: u8 = 0x02;
/// Status Register 2: Suspend Status
pub const SR2_SUS: u8 = 0x80;

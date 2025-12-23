//! Intel ICH/PCH SPI controller register definitions
//!
//! This module contains register offsets and bit definitions for the various
//! Intel SPI controller generations, ported from flashprog's ichspi.c.
//!
//! # Register Layout by Generation
//!
//! - ICH7: Original SPI controller, software sequencing only
//! - ICH8/ICH9: Hardware sequencing introduced, dual flash support
//! - PCH100+: New register layout at different offsets

// ============================================================================
// ICH7 Register Definitions
// ============================================================================

/// ICH7 SPI Status register (16 bits)
pub const ICH7_REG_SPIS: usize = 0x00;
/// ICH7 SPI Control register (16 bits)
pub const ICH7_REG_SPIC: usize = 0x02;
/// ICH7 SPI Address register (32 bits)
pub const ICH7_REG_SPIA: usize = 0x04;
/// ICH7 SPI Data registers (64 bytes starting here)
pub const ICH7_REG_SPID0: usize = 0x08;
/// ICH7 Pre-opcode register (16 bits)
pub const ICH7_REG_PREOP: usize = 0x54;
/// ICH7 Opcode Type register (16 bits)
pub const ICH7_REG_OPTYPE: usize = 0x56;
/// ICH7 Opcode Menu register (64 bits)
pub const ICH7_REG_OPMENU: usize = 0x58;

// ICH7 SPIS bits
/// SPI Cycle In Progress
pub const SPIS_SCIP: u16 = 0x0001;
/// SPI Cycle Grant
pub const SPIS_GRANT: u16 = 0x0002;
/// Cycle Done Status
pub const SPIS_CDS: u16 = 0x0004;
/// Flash Cycle Error
pub const SPIS_FCERR: u16 = 0x0008;
/// Reserved bits mask
pub const SPIS_RESERVED_MASK: u16 = 0x7ff0;

// ICH7 SPIC bits
/// SPI Cycle Go
pub const SPIC_SCGO: u16 = 0x0002;
/// Atomic Cycle Sequence
pub const SPIC_ACS: u16 = 0x0004;
/// Sequence Prefix Opcode Pointer
pub const SPIC_SPOP: u16 = 0x0008;
/// Data Cycle
pub const SPIC_DS: u16 = 0x4000;

// ============================================================================
// ICH9 Register Definitions
// ============================================================================

/// ICH9 Hardware Sequencing Flash Status (16 bits)
pub const ICH9_REG_HSFS: usize = 0x04;
/// ICH9 Hardware Sequencing Flash Control (16 bits)
pub const ICH9_REG_HSFC: usize = 0x06;
/// ICH9 Flash Address register (32 bits)
pub const ICH9_REG_FADDR: usize = 0x08;
/// ICH9 Flash Data registers (64 bytes starting here)
pub const ICH9_REG_FDATA0: usize = 0x10;
/// ICH9 Flash Region Access Permissions (32 bits)
pub const ICH9_REG_FRAP: usize = 0x50;
/// ICH9 Flash Region 0 (32 bits each, 5 regions)
pub const ICH9_REG_FREG0: usize = 0x54;
/// ICH9 Protected Range 0 (32 bits each, 5 ranges)
pub const ICH9_REG_PR0: usize = 0x74;
/// ICH9 Software Sequencing Flash Status (8 bits)
pub const ICH9_REG_SSFS: usize = 0x90;
/// ICH9 Software Sequencing Flash Control (24 bits)
pub const ICH9_REG_SSFC: usize = 0x91;
/// ICH9 Pre-opcode register (16 bits)
pub const ICH9_REG_PREOP: usize = 0x94;
/// ICH9 Opcode Type register (16 bits)
pub const ICH9_REG_OPTYPE: usize = 0x96;
/// ICH9 Opcode Menu register (64 bits)
pub const ICH9_REG_OPMENU: usize = 0x98;
/// ICH9 BIOS Base Address Configuration (32 bits)
pub const ICH9_REG_BBAR: usize = 0xA0;
/// ICH8 Vendor Specific Component Capabilities (32 bits)
pub const ICH8_REG_VSCC: usize = 0xC1;
/// ICH9 Lower Vendor Specific Component Capabilities (32 bits)
pub const ICH9_REG_LVSCC: usize = 0xC4;
/// ICH9 Upper Vendor Specific Component Capabilities (32 bits)
pub const ICH9_REG_UVSCC: usize = 0xC8;
/// ICH9 Flash Partition Boundary (32 bits)
pub const ICH9_REG_FPB: usize = 0xD0;

// HSFS bits
/// Flash Cycle Done
pub const HSFS_FDONE_OFF: u16 = 0;
pub const HSFS_FDONE: u16 = 1 << HSFS_FDONE_OFF;
/// Flash Cycle Error
pub const HSFS_FCERR_OFF: u16 = 1;
pub const HSFS_FCERR: u16 = 1 << HSFS_FCERR_OFF;
/// Access Error Log
pub const HSFS_AEL_OFF: u16 = 2;
pub const HSFS_AEL: u16 = 1 << HSFS_AEL_OFF;
/// Block/Sector Erase Size
pub const HSFS_BERASE_OFF: u16 = 3;
pub const HSFS_BERASE: u16 = 0x3 << HSFS_BERASE_OFF;
/// SPI Cycle In Progress
pub const HSFS_SCIP_OFF: u16 = 5;
pub const HSFS_SCIP: u16 = 1 << HSFS_SCIP_OFF;
/// Flash Descriptor Override Pin-Strap Status
pub const HSFS_FDOPSS_OFF: u16 = 13;
pub const HSFS_FDOPSS: u16 = 1 << HSFS_FDOPSS_OFF;
/// Flash Descriptor Valid
pub const HSFS_FDV_OFF: u16 = 14;
pub const HSFS_FDV: u16 = 1 << HSFS_FDV_OFF;
/// Flash Configuration Lock-Down
pub const HSFS_FLOCKDN_OFF: u16 = 15;
pub const HSFS_FLOCKDN: u16 = 1 << HSFS_FLOCKDN_OFF;

// PCH100+ specific HSFS bits
/// Flash Configuration Lock-Down (Sunrise Point)
pub const HSFS_WRSDIS_OFF: u16 = 11;
pub const HSFS_WRSDIS: u16 = 1 << HSFS_WRSDIS_OFF;
/// PRR3 PRR4 Lock-Down
pub const HSFS_PRR34_LOCKDN_OFF: u16 = 12;
pub const HSFS_PRR34_LOCKDN: u16 = 1 << HSFS_PRR34_LOCKDN_OFF;

// HSFC bits
/// Flash Cycle Go
pub const HSFC_FGO_OFF: u16 = 0;
pub const HSFC_FGO: u16 = 1 << HSFC_FGO_OFF;
/// Flash Cycle (ICH9)
pub const HSFC_FCYCLE_OFF: u16 = 1;
pub const HSFC_FCYCLE: u16 = 0x3 << HSFC_FCYCLE_OFF;
/// Flash Data Byte Count
pub const HSFC_FDBC_OFF: u16 = 8;
pub const HSFC_FDBC: u16 = 0x3f << HSFC_FDBC_OFF;
/// SPI SMI# Enable
pub const HSFC_SME_OFF: u16 = 15;
pub const HSFC_SME: u16 = 1 << HSFC_SME_OFF;

// PCH100+ specific HSFC bits
/// Flash Cycle (PCH100) - 4 bits instead of 2
pub const PCH100_HSFC_FCYCLE_OFF: u16 = 17 - 16; // Offset within HSFC
pub const PCH100_HSFC_FCYCLE: u16 = 0xf << PCH100_HSFC_FCYCLE_OFF;
/// Write Enable Type
pub const HSFC_WET_OFF: u16 = 21 - 16;
pub const HSFC_WET: u16 = 1 << HSFC_WET_OFF;

// FADDR masks
/// ICH9 Flash Address mask (25 bits)
pub const ICH9_FADDR_FLA: u32 = 0x01ffffff;
/// PCH100 Flash Address mask (27 bits)
pub const PCH100_FADDR_FLA: u32 = 0x07ffffff;

// SSFS bits (Software Sequencing)
/// SPI Cycle In Progress
pub const SSFS_SCIP_OFF: u32 = 0;
pub const SSFS_SCIP: u32 = 1 << SSFS_SCIP_OFF;
/// Cycle Done Status
pub const SSFS_FDONE_OFF: u32 = 2;
pub const SSFS_FDONE: u32 = 1 << SSFS_FDONE_OFF;
/// Flash Cycle Error
pub const SSFS_FCERR_OFF: u32 = 3;
pub const SSFS_FCERR: u32 = 1 << SSFS_FCERR_OFF;
/// Access Error Log
pub const SSFS_AEL_OFF: u32 = 4;
pub const SSFS_AEL: u32 = 1 << SSFS_AEL_OFF;
/// Reserved bits mask
pub const SSFS_RESERVED_MASK: u32 = 0x000000e2;

// SSFC bits (offset by 8 since SSFS+SSFC are combined to 32 bits)
/// SPI Cycle Go
pub const SSFC_SCGO_OFF: u32 = 1 + 8;
pub const SSFC_SCGO: u32 = 1 << SSFC_SCGO_OFF;
/// Atomic Cycle Sequence
pub const SSFC_ACS_OFF: u32 = 2 + 8;
pub const SSFC_ACS: u32 = 1 << SSFC_ACS_OFF;
/// Sequence Prefix Opcode Pointer
pub const SSFC_SPOP_OFF: u32 = 3 + 8;
pub const SSFC_SPOP: u32 = 1 << SSFC_SPOP_OFF;
/// Cycle Opcode Pointer
pub const SSFC_COP_OFF: u32 = 4 + 8;
pub const SSFC_COP: u32 = 0x7 << SSFC_COP_OFF;
/// Data Byte Count
pub const SSFC_DBC_OFF: u32 = 8 + 8;
pub const SSFC_DBC: u32 = 0x3f << SSFC_DBC_OFF;
/// Data Cycle
pub const SSFC_DS_OFF: u32 = 14 + 8;
pub const SSFC_DS: u32 = 1 << SSFC_DS_OFF;
/// SPI SMI# Enable
pub const SSFC_SME_OFF: u32 = 15 + 8;
pub const SSFC_SME: u32 = 1 << SSFC_SME_OFF;
/// SPI Cycle Frequency
pub const SSFC_SCF_OFF: u32 = 16 + 8;
pub const SSFC_SCF: u32 = 0x7 << SSFC_SCF_OFF;
/// 20 MHz clock
pub const SSFC_SCF_20MHZ: u32 = 0x00000000;
/// 33 MHz clock
pub const SSFC_SCF_33MHZ: u32 = 0x01000000;
/// Reserved bits mask
pub const SSFC_RESERVED_MASK: u32 = 0xf8008100;

// BBAR bits
/// Bottom of System Flash mask
pub const BBAR_MASK: u32 = 0x00ffff00;

// FPB bits (Flash Partition Boundary)
/// Block/Sector Erase Size
pub const FPB_FPBA_OFF: u32 = 0;
pub const FPB_FPBA: u32 = 0x1fff << FPB_FPBA_OFF;

// Protected Range bits
/// Write protection enable
pub const PR_WP_OFF: u32 = 31;
/// Read protection enable
pub const PR_RP_OFF: u32 = 15;

// ============================================================================
// PCH100 (Sunrise Point and later) Register Definitions
// ============================================================================

/// PCH100 Discrete Lock Bits (32 bits)
pub const PCH100_REG_DLOCK: usize = 0x0C;
/// PCH100 Protected Range 0 (32 bits each, 6 ranges including GPR0)
pub const PCH100_REG_FPR0: usize = 0x84;
/// PCH100 Global Protected Range 0
pub const PCH100_REG_GPR0: usize = 0x98;
/// PCH100 Software Sequencing Flash Status/Control (32 bits)
pub const PCH100_REG_SSFSC: usize = 0xA0;
/// PCH100 Pre-opcode register (16 bits)
pub const PCH100_REG_PREOP: usize = 0xA4;
/// PCH100 Opcode Type register (16 bits)
pub const PCH100_REG_OPTYPE: usize = 0xA6;
/// PCH100 Opcode Menu register (64 bits)
pub const PCH100_REG_OPMENU: usize = 0xA8;

// DLOCK bits
/// BMWAG Lock-Down
pub const DLOCK_BMWAG_LOCKDN_OFF: u32 = 0;
pub const DLOCK_BMWAG_LOCKDN: u32 = 1 << DLOCK_BMWAG_LOCKDN_OFF;
/// BMRAG Lock-Down
pub const DLOCK_BMRAG_LOCKDN_OFF: u32 = 1;
pub const DLOCK_BMRAG_LOCKDN: u32 = 1 << DLOCK_BMRAG_LOCKDN_OFF;
/// SBMWAG Lock-Down
pub const DLOCK_SBMWAG_LOCKDN_OFF: u32 = 2;
pub const DLOCK_SBMWAG_LOCKDN: u32 = 1 << DLOCK_SBMWAG_LOCKDN_OFF;
/// SBMRAG Lock-Down
pub const DLOCK_SBMRAG_LOCKDN_OFF: u32 = 3;
pub const DLOCK_SBMRAG_LOCKDN: u32 = 1 << DLOCK_SBMRAG_LOCKDN_OFF;
/// PR0 Lock-Down
pub const DLOCK_PR0_LOCKDN_OFF: u32 = 8;
pub const DLOCK_PR0_LOCKDN: u32 = 1 << DLOCK_PR0_LOCKDN_OFF;
/// SSEQ Lock-Down
pub const DLOCK_SSEQ_LOCKDN_OFF: u32 = 16;
pub const DLOCK_SSEQ_LOCKDN: u32 = 1 << DLOCK_SSEQ_LOCKDN_OFF;

// C740 (Emmitsburg) and later - new access permission registers
/// BIOS Master Write Access Permissions
pub const BIOS_BM_WAP: usize = 0x11C;
/// BIOS Master Read Access Permissions
pub const BIOS_BM_RAP: usize = 0x118;

// Apollo Lake specific
/// Flash Region 12 (Apollo Lake)
pub const APL_REG_FREG12: usize = 0xE0;

// ============================================================================
// SPI Opcode Types
// ============================================================================

/// Read without address
pub const SPI_OPCODE_TYPE_READ_NO_ADDRESS: u8 = 0;
/// Write without address
pub const SPI_OPCODE_TYPE_WRITE_NO_ADDRESS: u8 = 1;
/// Read with address
pub const SPI_OPCODE_TYPE_READ_WITH_ADDRESS: u8 = 2;
/// Write with address
pub const SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS: u8 = 3;

// ============================================================================
// Flash Region helpers
// ============================================================================

/// Extract base address from FREG register value
#[inline]
pub const fn freg_base(freg: u32) -> u32 {
    (freg & 0x7fff) << 12
}

/// Extract limit address from FREG register value
#[inline]
pub const fn freg_limit(freg: u32) -> u32 {
    ((freg >> 16) & 0x7fff) << 12 | 0xfff
}

// ============================================================================
// PCI Configuration Space
// ============================================================================

// LPC Bridge (Bus 0, Device 31, Function 0)
/// PCI config offset for RCBA (Root Complex Base Address) - ICH7-ICH10
pub const PCI_REG_RCBA: u8 = 0xF0;

// SPI controller offset within RCBA
/// Offset to SPI registers within RCBA (ICH7)
pub const RCBA_SPI_OFFSET_ICH7: u32 = 0x3020;
/// Offset to SPI registers within RCBA (ICH8+)
pub const RCBA_SPI_OFFSET_ICH9: u32 = 0x3800;

// For PCH (100 Series and later), SPI is at a fixed MMIO address
// or accessed via P2SB (Primary to Sideband) bridge

/// SPIBAR register in LPC/eSPI controller config space (PCH100+)
pub const PCI_REG_SPIBAR: u8 = 0x10; // BAR0

// LPC Bridge BIOS_CNTL register
/// BIOS Control Register offset
pub const PCI_REG_BIOS_CNTL: u8 = 0xDC;

// BIOS_CNTL bits
/// BIOS Write Enable
pub const BIOS_CNTL_BWE: u8 = 1 << 0;
/// BIOS Lock Enable
pub const BIOS_CNTL_BLE: u8 = 1 << 1;
/// SPI Read Configuration Enable
pub const BIOS_CNTL_SRC: u8 = 0x3 << 2;
/// Top Swap Status
pub const BIOS_CNTL_TSS: u8 = 1 << 4;
/// SMM BIOS Write Protect Disable
pub const BIOS_CNTL_SMM_BWP: u8 = 1 << 5;

// ============================================================================
// Access Protection
// ============================================================================

/// Region access protection status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessProtection {
    /// No protection - read and write allowed
    None,
    /// Read protected
    ReadProtected,
    /// Write protected
    WriteProtected,
    /// Both read and write protected
    Locked,
}

impl AccessProtection {
    /// Create from read/write permission bits
    pub fn from_permissions(can_read: bool, can_write: bool) -> Self {
        match (can_read, can_write) {
            (true, true) => Self::None,
            (true, false) => Self::WriteProtected,
            (false, true) => Self::ReadProtected,
            (false, false) => Self::Locked,
        }
    }

    /// Check if writes are allowed
    pub fn can_write(self) -> bool {
        matches!(self, Self::None | Self::ReadProtected)
    }

    /// Check if reads are allowed
    pub fn can_read(self) -> bool {
        matches!(self, Self::None | Self::WriteProtected)
    }
}

// ============================================================================
// JEDEC opcodes (common SPI flash commands)
// ============================================================================

/// Write Enable
pub const JEDEC_WREN: u8 = 0x06;
/// Write Disable
pub const JEDEC_WRDI: u8 = 0x04;
/// Enable Write Status Register
pub const JEDEC_EWSR: u8 = 0x50;
/// Read Status Register
pub const JEDEC_RDSR: u8 = 0x05;
/// Write Status Register
pub const JEDEC_WRSR: u8 = 0x01;
/// Read Data
pub const JEDEC_READ: u8 = 0x03;
/// Fast Read
pub const JEDEC_FAST_READ: u8 = 0x0B;
/// Page Program
pub const JEDEC_BYTE_PROGRAM: u8 = 0x02;
/// Sector Erase (4KB typically)
pub const JEDEC_SE: u8 = 0x20;
/// Block Erase (32KB)
pub const JEDEC_BE_52: u8 = 0x52;
/// Block Erase (64KB)
pub const JEDEC_BE_D8: u8 = 0xD8;
/// Chip Erase (0x60)
pub const JEDEC_CE_60: u8 = 0x60;
/// Chip Erase (0xC7)
pub const JEDEC_CE_C7: u8 = 0xC7;
/// Read JEDEC ID
pub const JEDEC_RDID: u8 = 0x9F;
/// Read Electronic Manufacturer Signature
pub const JEDEC_REMS: u8 = 0x90;
/// Read Electronic Signature
pub const JEDEC_RES: u8 = 0xAB;
/// AAI Word Program (SST specific)
pub const JEDEC_AAI_WORD_PROGRAM: u8 = 0xAD;

// ============================================================================
// Hardware Sequencing Cycle Types
// ============================================================================

/// Hardware sequencing cycle type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HwSeqCycle {
    /// Read cycle
    Read = 0,
    /// Reserved
    Reserved = 1,
    /// Write cycle
    Write = 2,
    /// Erase cycle
    Erase = 3,
}

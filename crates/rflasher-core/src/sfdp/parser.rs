//! SFDP parsing implementation
//!
//! This module provides functions to read and parse SFDP data from flash chips.

use crate::error::{Error, Result};
use crate::programmer::SpiMaster;
use crate::protocol;

use super::types::*;

/// Read raw SFDP data from flash
///
/// This is a low-level function that reads SFDP data at the specified address.
pub fn read_sfdp<M: SpiMaster + ?Sized>(master: &mut M, addr: u32, buf: &mut [u8]) -> Result<()> {
    protocol::read_sfdp(master, addr, buf)
}

/// Parse the SFDP header and verify signature
fn parse_header<M: SpiMaster + ?Sized>(master: &mut M) -> Result<SfdpHeader> {
    let mut buf = [0u8; 8];

    log::debug!("Reading SFDP header (8 bytes at address 0x00)...");

    read_sfdp(master, 0x00, &mut buf)?;

    log::debug!(
        "SFDP header bytes: {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
        buf[0],
        buf[1],
        buf[2],
        buf[3],
        buf[4],
        buf[5],
        buf[6],
        buf[7]
    );

    let header = SfdpHeader::parse(&buf);

    if !header.is_valid() {
        log::debug!("SFDP signature invalid (expected 'SFDP')");
        return Err(Error::ChipNotSupported);
    }

    // Check for supported SFDP major version
    if header.revision.major != 1 {
        log::debug!("SFDP major version {} not supported", header.revision.major);
        return Err(Error::ChipNotSupported);
    }

    log::debug!(
        "SFDP header valid: revision {}.{}",
        header.revision.major,
        header.revision.minor
    );

    Ok(header)
}

/// Read and parse a parameter header
fn read_param_header<M: SpiMaster + ?Sized>(
    master: &mut M,
    index: usize,
) -> Result<ParameterHeader> {
    let mut buf = [0u8; 8];
    let addr = 0x08 + (index as u32 * 8);
    read_sfdp(master, addr, &mut buf)?;
    Ok(ParameterHeader::parse(&buf))
}

/// Parse Basic Flash Parameter Table DWORD 1
///
/// Contains fast read support, address mode, write granularity, etc.
fn parse_bfpt_dword1(dword: u32, params: &mut BasicFlashParams) {
    // Bits [1:0] - 4KB erase support
    // 00b = Reserved, 01b = Supported, 10b = Reserved, 11b = Not supported
    let erase_4k_support = (dword & 0x03) == 0x01;

    // Bits [2] - Write granularity (0 = 1 byte, 1 = 64+ bytes)
    params.write_granularity_64 = (dword & (1 << 2)) != 0;

    // Bits [3] - Status register volatile (1 = volatile)
    params.status_reg_volatile = (dword & (1 << 3)) != 0;

    // Bits [4] - Write enable for volatile SR (0 = EWSR, 1 = WREN)
    params.volatile_sr_write_enable = if (dword & (1 << 4)) != 0 {
        WriteEnableForVolatileSr::Wren
    } else {
        WriteEnableForVolatileSr::Ewsr
    };

    // Bits [15:8] - 4KB erase opcode
    params.erase_4k_opcode = if erase_4k_support {
        ((dword >> 8) & 0xFF) as u8
    } else {
        0xFF
    };

    // Bits [16] - Supports 1-1-2 fast read
    params.fast_read_112 = (dword & (1 << 16)) != 0;

    // Bits [18:17] - Address bytes
    params.address_mode = AddressMode::from_bfpt(((dword >> 17) & 0x03) as u8);

    // Bits [19] - DTR clocking supported
    params.dtr_clocking = (dword & (1 << 19)) != 0;

    // Bits [20] - Supports 1-2-2 fast read
    params.fast_read_122 = (dword & (1 << 20)) != 0;

    // Bits [21] - Supports 1-4-4 fast read
    params.fast_read_144 = (dword & (1 << 21)) != 0;

    // Bits [22] - Supports 1-1-4 fast read
    params.fast_read_114 = (dword & (1 << 22)) != 0;
}

/// Parse Basic Flash Parameter Table DWORD 2
///
/// Contains flash density.
fn parse_bfpt_dword2(dword: u32, params: &mut BasicFlashParams) {
    // Bit 31: density format
    // 0 = bits 30:0 contain density in bits
    // 1 = bits 30:0 contain N where density = 2^N bits
    if (dword & (1 << 31)) == 0 {
        // Direct bit count
        let bits = dword & 0x7FFFFFFF;
        params.density_bytes = ((bits as u64) + 1) / 8;
    } else {
        // 2^N format
        let n = dword & 0x7FFFFFFF;
        if n >= 3 {
            // Divide by 8 to convert bits to bytes (subtract 3 from exponent)
            params.density_bytes = 1u64 << (n - 3);
        }
    }
}

/// Parse Basic Flash Parameter Table DWORD 3
///
/// Contains 1-1-4 and 1-4-4 fast read instruction parameters.
fn parse_bfpt_dword3(dword: u32, params: &mut BasicFlashParams) {
    // High half [31:16]: 1S-1S-4S (1-1-4) fast read
    // [31:24] instruction, [23:21] mode clocks, [20:16] dummy clocks
    params.fast_read_114_params = FastReadParams::from_high_half(dword);

    // Low half [15:0]: 1S-4S-4S (1-4-4) fast read
    // [15:8] instruction, [7:5] mode clocks, [4:0] dummy clocks
    params.fast_read_144_params = FastReadParams::from_low_half(dword);
}

/// Parse Basic Flash Parameter Table DWORD 4
///
/// Contains 1-2-2 and 1-1-2 fast read instruction parameters.
fn parse_bfpt_dword4(dword: u32, params: &mut BasicFlashParams) {
    // High half [31:16]: 1S-2S-2S (1-2-2) fast read
    params.fast_read_122_params = FastReadParams::from_high_half(dword);

    // Low half [15:0]: 1S-1S-2S (1-1-2) fast read
    params.fast_read_112_params = FastReadParams::from_low_half(dword);
}

/// Parse Basic Flash Parameter Table DWORD 5
///
/// Contains 2-2-2 and 4-4-4 fast read support.
fn parse_bfpt_dword5(dword: u32, params: &mut BasicFlashParams) {
    // Bit 0 - Supports 2-2-2 fast read
    params.fast_read_222 = (dword & (1 << 0)) != 0;

    // Bit 4 - Supports 4-4-4 fast read
    params.fast_read_444 = (dword & (1 << 4)) != 0;
}

/// Parse Basic Flash Parameter Table DWORD 6
///
/// Contains 2-2-2 fast read instruction parameters.
fn parse_bfpt_dword6(dword: u32, params: &mut BasicFlashParams) {
    // High half [31:16]: 2S-2S-2S (2-2-2) fast read
    params.fast_read_222_params = FastReadParams::from_high_half(dword);
    // Low half [15:0]: Reserved (all 1s)
}

/// Parse Basic Flash Parameter Table DWORD 7
///
/// Contains 4-4-4 fast read instruction parameters.
fn parse_bfpt_dword7(dword: u32, params: &mut BasicFlashParams) {
    // High half [31:16]: 4S-4S-4S (4-4-4) fast read
    params.fast_read_444_params = FastReadParams::from_high_half(dword);
    // Low half [15:0]: Reserved (all 1s)
}

/// Parse Basic Flash Parameter Table DWORDs 8-9
///
/// Contains erase type definitions.
fn parse_bfpt_erase_types(dword8: u32, dword9: u32, params: &mut BasicFlashParams) {
    // DWORD 8:
    // [7:0]   - Erase Type 1 Size (N, size = 2^N bytes)
    // [15:8]  - Erase Type 1 Opcode
    // [23:16] - Erase Type 2 Size
    // [31:24] - Erase Type 2 Opcode

    let et1_size = (dword8 & 0xFF) as u8;
    let et1_opcode = ((dword8 >> 8) & 0xFF) as u8;
    let et2_size = ((dword8 >> 16) & 0xFF) as u8;
    let et2_opcode = ((dword8 >> 24) & 0xFF) as u8;

    params.erase_types[0] = SfdpEraseType::from_raw(et1_size, et1_opcode);
    params.erase_types[1] = SfdpEraseType::from_raw(et2_size, et2_opcode);

    // DWORD 9:
    // [7:0]   - Erase Type 3 Size
    // [15:8]  - Erase Type 3 Opcode
    // [23:16] - Erase Type 4 Size
    // [31:24] - Erase Type 4 Opcode

    let et3_size = (dword9 & 0xFF) as u8;
    let et3_opcode = ((dword9 >> 8) & 0xFF) as u8;
    let et4_size = ((dword9 >> 16) & 0xFF) as u8;
    let et4_opcode = ((dword9 >> 24) & 0xFF) as u8;

    params.erase_types[2] = SfdpEraseType::from_raw(et3_size, et3_opcode);
    params.erase_types[3] = SfdpEraseType::from_raw(et4_size, et4_opcode);
}

/// Parse Basic Flash Parameter Table DWORD 11
///
/// Contains page size and timing information.
fn parse_bfpt_dword11(dword: u32, params: &mut BasicFlashParams) {
    // Bits [7:4] - Page size (N, size = 2^N bytes)
    let page_size_exp = ((dword >> 4) & 0x0F) as u8;
    if page_size_exp > 0 {
        params.page_size = 1u32 << page_size_exp;
    } else {
        // Default to 256 bytes if not specified
        params.page_size = 256;
    }
}

/// Parse Basic Flash Parameter Table DWORD 15
///
/// Contains QE requirements and 4-4-4 mode sequences (JESD216B+).
fn parse_bfpt_dword15(dword: u32, params: &mut BasicFlashParams) {
    // Bits [22:20] - Quad Enable Requirements
    let qer = ((dword >> 20) & 0x07) as u8;
    params.quad_enable = QuadEnableRequirement::from_bfpt(qer);
}

/// Parse Basic Flash Parameter Table DWORD 16
///
/// Contains 4-byte address entry and soft reset support (JESD216B+).
fn parse_bfpt_dword16(dword: u32, params: &mut BasicFlashParams) {
    // Bits [13:8] - Soft Reset support
    let soft_reset_bits = ((dword >> 8) & 0x3F) as u8;
    params.soft_reset = SoftResetSupport::from_bfpt(soft_reset_bits);

    // Bits [31:24] - 4-byte address entry methods
    let entry_bits = ((dword >> 24) & 0xFF) as u8;
    params.four_byte_entry = FourByteEntryMethods::from_bfpt(entry_bits);
}

/// Parse the Basic Flash Parameter Table
fn parse_bfpt<M: SpiMaster + ?Sized>(
    master: &mut M,
    header: &ParameterHeader,
) -> Result<BasicFlashParams> {
    let len = header.length_bytes();
    if len < 36 {
        // Minimum is 9 DWORDs (JESD216)
        return Err(Error::ChipNotSupported);
    }

    // Read the parameter table
    let mut buf = [0u8; 92]; // Up to 23 DWORDs (JESD216F)
    let read_len = core::cmp::min(len, buf.len());
    read_sfdp(master, header.table_pointer, &mut buf[..read_len])?;

    let mut params = BasicFlashParams {
        revision: header.revision,
        ..Default::default()
    };

    // Helper to read a DWORD from the buffer
    let get_dword = |offset: usize| -> u32 {
        if offset + 4 <= read_len {
            u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ])
        } else {
            0
        }
    };

    // Parse mandatory DWORDs (JESD216, 9 DWORDs minimum)
    parse_bfpt_dword1(get_dword(0), &mut params); // DWORD 1
    parse_bfpt_dword2(get_dword(4), &mut params); // DWORD 2
    parse_bfpt_dword3(get_dword(8), &mut params); // DWORD 3 - 1-1-4 and 1-4-4 fast read
    parse_bfpt_dword4(get_dword(12), &mut params); // DWORD 4 - 1-2-2 and 1-1-2 fast read
    parse_bfpt_dword5(get_dword(16), &mut params); // DWORD 5
    parse_bfpt_dword6(get_dword(20), &mut params); // DWORD 6 - 2-2-2 fast read
    parse_bfpt_dword7(get_dword(24), &mut params); // DWORD 7 - 4-4-4 fast read
    parse_bfpt_erase_types(get_dword(28), get_dword(32), &mut params); // DWORD 8-9

    // Parse extended DWORDs if available (JESD216A+, 16+ DWORDs)
    if len >= 44 {
        // DWORD 11
        parse_bfpt_dword11(get_dword(40), &mut params);
    }

    // Parse JESD216B+ additions (DWORDs 15-16)
    if len >= 64 {
        parse_bfpt_dword15(get_dword(56), &mut params); // DWORD 15
        parse_bfpt_dword16(get_dword(60), &mut params); // DWORD 16
    }

    // Validate density
    if params.density_bytes == 0 {
        return Err(Error::ChipNotSupported);
    }

    // Apply default page size if not set
    if params.page_size == 0 {
        params.page_size = 256;
    }

    Ok(params)
}

/// Parse the 4-Byte Address Instruction Table
fn parse_4byte_addr_table<M: SpiMaster + ?Sized>(
    master: &mut M,
    header: &ParameterHeader,
) -> Result<FourByteAddrTable> {
    let len = header.length_bytes();
    if len < 8 {
        // Minimum is 2 DWORDs
        return Err(Error::ChipNotSupported);
    }

    // Read the parameter table (2 DWORDs)
    let mut buf = [0u8; 8];
    let read_len = core::cmp::min(len, buf.len());
    read_sfdp(master, header.table_pointer, &mut buf[..read_len])?;

    let get_dword = |offset: usize| -> u32 {
        if offset + 4 <= read_len {
            u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ])
        } else {
            0
        }
    };

    let table = FourByteAddrTable {
        revision: header.revision,
        instructions: FourByteAddrInstructions::from_dword1(get_dword(0)),
        erase_opcodes: FourByteAddrEraseOpcodes::from_dword2(get_dword(4)),
    };

    Ok(table)
}

/// Probe for SFDP support and parse parameters
///
/// This function reads and parses the SFDP data from a flash chip.
/// Returns `Err(ChipNotSupported)` if the chip doesn't support SFDP
/// or has an unsupported SFDP version.
///
/// # Example
///
/// ```ignore
/// use rflasher_core::sfdp;
///
/// let info = sfdp::probe(&mut master)?;
/// println!("Flash size: {} bytes", info.total_size());
/// println!("Page size: {} bytes", info.page_size());
/// ```
pub fn probe<M: SpiMaster + ?Sized>(master: &mut M) -> Result<SfdpInfo> {
    // Read and parse the SFDP header
    let header = parse_header(master)?;

    let num_headers = header.num_param_headers();
    let mut info = SfdpInfo {
        header,
        num_param_headers: num_headers,
        ..Default::default()
    };

    // Track if we've found the mandatory BFPT
    let mut found_bfpt = false;

    // Parse all parameter tables we understand
    for i in 0..num_headers {
        if i >= MAX_PARAMETER_HEADERS {
            break;
        }

        let param_header = read_param_header(master, i)?;

        match param_header.id {
            // Basic Flash Parameter Table (mandatory)
            PARAM_ID_BASIC => {
                info.basic_params = parse_bfpt(master, &param_header)?;
                found_bfpt = true;
            }
            // 4-Byte Address Instruction Table
            PARAM_ID_4BYTE_ADDR => {
                if let Ok(table) = parse_4byte_addr_table(master, &param_header) {
                    log::debug!(
                        "Found 4-byte address instruction table: rev {}.{}",
                        table.revision.major,
                        table.revision.minor
                    );
                    info.four_byte_addr_table = Some(table);
                }
            }
            // Other tables we might support in the future
            _ => {
                log::trace!(
                    "Skipping parameter table ID 0x{:04X} (rev {}.{})",
                    param_header.id,
                    param_header.revision.major,
                    param_header.revision.minor
                );
            }
        }
    }

    // Validate that we found a valid BFPT
    if !found_bfpt || info.basic_params.density_bytes == 0 {
        return Err(Error::ChipNotSupported);
    }

    Ok(info)
}

/// Check if SFDP is supported without fully parsing
///
/// This is a quick check that only reads the SFDP signature.
pub fn is_supported<M: SpiMaster + ?Sized>(master: &mut M) -> bool {
    let mut buf = [0u8; 4];
    if read_sfdp(master, 0x00, &mut buf).is_err() {
        return false;
    }

    let signature = u32::from_le_bytes(buf);
    signature == SFDP_SIGNATURE
}

// ============================================================================
// Conversion to FlashChip
// ============================================================================

#[cfg(feature = "alloc")]
use alloc::{string::String, vec::Vec};

#[cfg(feature = "alloc")]
use crate::chip::{EraseBlock, Features, FlashChip, WriteGranularity};

/// Convert SFDP info to a FlashChip structure
///
/// This creates a FlashChip populated with data discovered from SFDP.
/// The vendor is set to "SFDP" and name to "Unknown" since SFDP doesn't
/// provide chip identification (use JEDEC ID for that).
#[cfg(feature = "alloc")]
pub fn to_flash_chip(info: &SfdpInfo, jedec_manufacturer: u8, jedec_device: u16) -> FlashChip {
    let params = &info.basic_params;

    // Build feature flags from SFDP data
    let mut features = Features::SFDP;

    if params.fast_read_112 || params.fast_read_122 {
        features |= Features::DUAL_IO;
    }
    if params.fast_read_114 || params.fast_read_144 || params.fast_read_444 {
        features |= Features::QUAD_IO;
    }
    if params.address_mode.supports_4byte() {
        features |= Features::FOUR_BYTE_ADDR;
    }
    if params
        .four_byte_entry
        .supports(FourByteEntryMethods::INSTR_B7_E9)
        || params
            .four_byte_entry
            .supports(FourByteEntryMethods::WREN_INSTR_B7_E9)
    {
        features |= Features::FOUR_BYTE_ENTER;
    }
    if params.soft_reset.supports_66_99() {
        // Mark that soft reset is supported (could add a feature flag)
    }
    if params.volatile_sr_write_enable == WriteEnableForVolatileSr::Wren {
        features |= Features::WRSR_WREN;
    } else {
        features |= Features::WRSR_EWSR;
    }
    if params.quad_enable.is_needed() {
        features |= Features::QE_SR2;
    }

    // Build erase blocks from SFDP data
    let mut erase_blocks: Vec<EraseBlock> = params
        .erase_types
        .iter()
        .filter(|et| et.is_valid())
        .map(|et| EraseBlock::new(et.opcode, et.size))
        .collect();

    // Sort by size (smallest first)
    erase_blocks.sort_by_key(|eb| eb.min_block_size());

    // Set erase feature flags
    for eb in &erase_blocks {
        match eb.uniform_size() {
            Some(4096) => features |= Features::ERASE_4K,
            Some(32768) => features |= Features::ERASE_32K,
            Some(65536) => features |= Features::ERASE_64K,
            _ => {}
        }
    }

    // Determine write granularity
    let write_granularity = if params.write_granularity_64 {
        WriteGranularity::Page
    } else {
        WriteGranularity::Byte
    };

    FlashChip {
        vendor: String::from("SFDP"),
        name: String::from("Unknown"),
        jedec_manufacturer,
        jedec_device,
        total_size: params.density_bytes as u32,
        page_size: params.page_size as u16,
        features,
        voltage_min_mv: 2700, // Default, SFDP doesn't specify voltage
        voltage_max_mv: 3600,
        write_granularity,
        erase_blocks,
        tested: Default::default(),
    }
}

// ============================================================================
// Comparison with database entries
// ============================================================================

/// Mismatch found between SFDP and database entry
#[cfg(feature = "alloc")]
#[derive(Debug, Clone)]
pub enum SfdpMismatch {
    /// Total size differs
    TotalSize {
        /// Size reported by SFDP
        sfdp: u64,
        /// Size in database
        database: u32,
    },
    /// Page size differs
    PageSize {
        /// Page size reported by SFDP
        sfdp: u32,
        /// Page size in database
        database: u16,
    },
    /// Erase block not found in database
    MissingEraseBlock {
        /// Erase block size
        size: u32,
        /// Erase opcode
        opcode: u8,
    },
    /// Database has erase block not in SFDP
    ExtraEraseBlock {
        /// Erase block size
        size: u32,
        /// Erase opcode
        opcode: u8,
    },
    /// Erase block opcode differs
    EraseBlockOpcode {
        /// Erase block size
        size: u32,
        /// Opcode reported by SFDP
        sfdp_opcode: u8,
        /// Opcode in database
        db_opcode: u8,
    },
    /// Address mode differs
    AddressMode {
        /// Whether SFDP says 4-byte addressing is required
        sfdp_requires_4byte: bool,
        /// Whether database says 4-byte addressing is required
        db_requires_4byte: bool,
    },
}

#[cfg(feature = "alloc")]
impl core::fmt::Display for SfdpMismatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TotalSize { sfdp, database } => {
                write!(
                    f,
                    "size: SFDP reports {} bytes, database has {} bytes",
                    sfdp, database
                )
            }
            Self::PageSize { sfdp, database } => {
                write!(
                    f,
                    "page size: SFDP reports {} bytes, database has {} bytes",
                    sfdp, database
                )
            }
            Self::MissingEraseBlock { size, opcode } => {
                write!(
                    f,
                    "SFDP has {}KB erase (opcode 0x{:02X}) not in database",
                    size / 1024,
                    opcode
                )
            }
            Self::ExtraEraseBlock { size, opcode } => {
                write!(
                    f,
                    "database has {}KB erase (opcode 0x{:02X}) not in SFDP",
                    size / 1024,
                    opcode
                )
            }
            Self::EraseBlockOpcode {
                size,
                sfdp_opcode,
                db_opcode,
            } => {
                write!(
                    f,
                    "{}KB erase opcode: SFDP has 0x{:02X}, database has 0x{:02X}",
                    size / 1024,
                    sfdp_opcode,
                    db_opcode
                )
            }
            Self::AddressMode {
                sfdp_requires_4byte,
                db_requires_4byte,
            } => {
                write!(
                    f,
                    "4-byte addressing: SFDP {} required, database {} required",
                    if *sfdp_requires_4byte { "is" } else { "not" },
                    if *db_requires_4byte { "is" } else { "not" }
                )
            }
        }
    }
}

/// Compare SFDP data against a database chip entry
///
/// Returns a list of mismatches found. An empty list means SFDP matches
/// the database entry.
#[cfg(feature = "alloc")]
pub fn compare_with_chip(info: &SfdpInfo, chip: &FlashChip) -> Vec<SfdpMismatch> {
    let mut mismatches = Vec::new();
    let params = &info.basic_params;

    // Compare total size
    if params.density_bytes != chip.total_size as u64 {
        mismatches.push(SfdpMismatch::TotalSize {
            sfdp: params.density_bytes,
            database: chip.total_size,
        });
    }

    // Compare page size
    if params.page_size != chip.page_size as u32 {
        mismatches.push(SfdpMismatch::PageSize {
            sfdp: params.page_size,
            database: chip.page_size,
        });
    }

    // Compare erase blocks
    let sfdp_erase: Vec<_> = params.erase_types.iter().filter(|e| e.is_valid()).collect();

    // Check for SFDP erase types not in database
    for sfdp_et in &sfdp_erase {
        // For uniform erase blocks, compare with SFDP
        if let Some(db_eb) = chip
            .erase_blocks()
            .iter()
            .filter(|eb| eb.is_uniform())
            .find(|eb| eb.uniform_size() == Some(sfdp_et.size))
        {
            // Size matches, check opcode
            if db_eb.opcode != sfdp_et.opcode {
                mismatches.push(SfdpMismatch::EraseBlockOpcode {
                    size: sfdp_et.size,
                    sfdp_opcode: sfdp_et.opcode,
                    db_opcode: db_eb.opcode,
                });
            }
        } else {
            mismatches.push(SfdpMismatch::MissingEraseBlock {
                size: sfdp_et.size,
                opcode: sfdp_et.opcode,
            });
        }
    }

    // Check for database erase types not in SFDP (only uniform blocks)
    for db_eb in chip.erase_blocks().iter().filter(|eb| eb.is_uniform()) {
        if let Some(size) = db_eb.uniform_size() {
            if !sfdp_erase.iter().any(|e| e.size == size) {
                mismatches.push(SfdpMismatch::ExtraEraseBlock {
                    size,
                    opcode: db_eb.opcode,
                });
            }
        }
    }

    // Compare addressing mode
    let sfdp_requires_4byte = params.requires_4byte_addr();
    let db_requires_4byte = chip.requires_4byte_addr();
    if sfdp_requires_4byte != db_requires_4byte {
        mismatches.push(SfdpMismatch::AddressMode {
            sfdp_requires_4byte,
            db_requires_4byte,
        });
    }

    mismatches
}

/// Result of probing SFDP and optionally matching with database
#[cfg(feature = "std")]
#[derive(Debug)]
pub struct SfdpProbeResult {
    /// The SFDP information read from the chip
    pub sfdp: SfdpInfo,
    /// JEDEC manufacturer ID
    pub jedec_manufacturer: u8,
    /// JEDEC device ID
    pub jedec_device: u16,
    /// Matching chip from database (if found)
    pub database_chip: Option<FlashChip>,
    /// Mismatches between SFDP and database (if database chip found)
    pub mismatches: Vec<SfdpMismatch>,
}

#[cfg(feature = "std")]
impl SfdpProbeResult {
    /// Get a FlashChip to use for operations
    ///
    /// Returns the database chip if available and matches well,
    /// otherwise returns a chip constructed from SFDP data.
    #[cfg(feature = "alloc")]
    pub fn to_flash_chip(&self) -> FlashChip {
        if let Some(ref db_chip) = self.database_chip {
            if self.mismatches.is_empty() {
                // Database matches exactly, use it
                return db_chip.clone();
            }
            // Has mismatches - still prefer database but caller should be warned
            return db_chip.clone();
        }

        // No database entry, construct from SFDP
        to_flash_chip(&self.sfdp, self.jedec_manufacturer, self.jedec_device)
    }

    /// Check if there are any concerning mismatches
    ///
    /// Size and page size mismatches are considered critical.
    #[cfg(feature = "alloc")]
    pub fn has_critical_mismatches(&self) -> bool {
        self.mismatches.iter().any(|m| {
            matches!(
                m,
                SfdpMismatch::TotalSize { .. } | SfdpMismatch::PageSize { .. }
            )
        })
    }
}

/// Probe SFDP and match with database
///
/// This function:
/// 1. Reads JEDEC ID from the chip
/// 2. Probes SFDP data
/// 3. Looks up the chip in the database by JEDEC ID
/// 4. Compares SFDP data with database entry (if found)
///
/// Returns a result containing all discovered information.
#[cfg(feature = "std")]
pub fn probe_with_database<M: SpiMaster + ?Sized>(
    master: &mut M,
    db: &crate::chip::ChipDatabase,
) -> Result<SfdpProbeResult> {
    // Read JEDEC ID first
    let (jedec_manufacturer, jedec_device) = protocol::read_jedec_id(master)?;

    // Probe SFDP
    let sfdp = probe(master)?;

    // Look up in database
    let database_chip = db
        .find_by_jedec_id(jedec_manufacturer, jedec_device)
        .cloned();

    // Compare if we have a database entry
    let mismatches = if let Some(ref chip) = database_chip {
        compare_with_chip(&sfdp, chip)
    } else {
        Vec::new()
    };

    Ok(SfdpProbeResult {
        sfdp,
        jedec_manufacturer,
        jedec_device,
        database_chip,
        mismatches,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sfdp_header_parse() {
        // Example SFDP header: "SFDP" signature, rev 1.6, 1 param header, legacy access
        let data = [0x53, 0x46, 0x44, 0x50, 0x06, 0x01, 0x00, 0xFF];
        let header = SfdpHeader::parse(&data);

        assert!(header.is_valid());
        assert_eq!(header.revision.major, 1);
        assert_eq!(header.revision.minor, 6);
        assert_eq!(header.num_param_headers(), 1);
        assert_eq!(header.access_protocol, 0xFF);
    }

    #[test]
    fn test_param_header_parse() {
        // Example parameter header for BFPT: ID=0xFF00, rev 1.6, 16 DWORDs, pointer 0x000080
        let data = [0x00, 0x06, 0x01, 0x10, 0x80, 0x00, 0x00, 0xFF];
        let header = ParameterHeader::parse(&data);

        assert!(header.is_basic());
        assert_eq!(header.id, 0xFF00);
        assert_eq!(header.revision.major, 1);
        assert_eq!(header.revision.minor, 6);
        assert_eq!(header.length_dwords, 16);
        assert_eq!(header.length_bytes(), 64);
        assert_eq!(header.table_pointer, 0x80);
    }

    #[test]
    fn test_density_parsing() {
        let mut params = BasicFlashParams::default();

        // 16 Mbit (2 MiB) in bits format
        parse_bfpt_dword2(0x00FFFFFF, &mut params);
        assert_eq!(params.density_bytes, 2 * 1024 * 1024);

        // 256 Mbit (32 MiB) in 2^N format (N=28 for bits, -3 for bytes = 25)
        params = BasicFlashParams::default();
        parse_bfpt_dword2(0x8000001C, &mut params); // bit 31 set, N=28
        assert_eq!(params.density_bytes, 32 * 1024 * 1024);
    }

    #[test]
    fn test_erase_type_parsing() {
        let mut params = BasicFlashParams::default();

        // DWORD 8 layout (little-endian):
        // [7:0]   - Erase Type 1 Size (0x0C = 12, 2^12 = 4096)
        // [15:8]  - Erase Type 1 Opcode (0x20)
        // [23:16] - Erase Type 2 Size (0x0F = 15, 2^15 = 32768)
        // [31:24] - Erase Type 2 Opcode (0x52)
        let dword8: u32 = 0x520F_200C;

        // DWORD 9 layout:
        // [7:0]   - Erase Type 3 Size (0x10 = 16, 2^16 = 65536)
        // [15:8]  - Erase Type 3 Opcode (0xD8)
        // [23:16] - Erase Type 4 Size (0x12 = 18, 2^18 = 262144)
        // [31:24] - Erase Type 4 Opcode (0xD8)
        let dword9: u32 = 0xD812_D810;

        parse_bfpt_erase_types(dword8, dword9, &mut params);

        assert_eq!(params.erase_types[0].size, 4096);
        assert_eq!(params.erase_types[0].opcode, 0x20);
        assert_eq!(params.erase_types[1].size, 32768);
        assert_eq!(params.erase_types[1].opcode, 0x52);
        assert_eq!(params.erase_types[2].size, 65536);
        assert_eq!(params.erase_types[2].opcode, 0xD8);
        assert_eq!(params.erase_types[3].size, 262144);
        assert_eq!(params.erase_types[3].opcode, 0xD8);
    }

    #[test]
    fn test_address_mode() {
        assert!(!AddressMode::ThreeByteOnly.requires_4byte());
        assert!(!AddressMode::ThreeByteOnly.supports_4byte());

        assert!(!AddressMode::ThreeOrFourByte.requires_4byte());
        assert!(AddressMode::ThreeOrFourByte.supports_4byte());

        assert!(AddressMode::FourByteOnly.requires_4byte());
        assert!(AddressMode::FourByteOnly.supports_4byte());
    }

    /// Complete SFDP table from flashprog's dummyflasher.c
    /// Based on MX25L6436E (rev. 1.8) datasheet - 8 MiB chip
    #[rustfmt::skip]
    const MX25L6436E_SFDP: [u8; 88] = [
        0x53, 0x46, 0x44, 0x50, // @0x00: SFDP signature "SFDP"
        0x00, 0x01, 0x01, 0xFF, // @0x04: revision 1.0, 2 headers (NPH=1)
        0x00, 0x00, 0x01, 0x09, // @0x08: JEDEC SFDP header rev. 1.0, 9 DW long
        0x1C, 0x00, 0x00, 0xFF, // @0x0C: PTP0 = 0x1C
        0xC2, 0x00, 0x01, 0x04, // @0x10: Macronix header rev. 1.0, 4 DW long
        0x48, 0x00, 0x00, 0xFF, // @0x14: PTP1 = 0x48
        0xFF, 0xFF, 0xFF, 0xFF, // @0x18: hole
        0xE5, 0x20, 0xC9, 0xFF, // @0x1C: SFDP parameter table start (DWORD 1)
        0xFF, 0xFF, 0xFF, 0x03, // @0x20: DWORD 2 - density
        0x00, 0xFF, 0x08, 0x6B, // @0x24: DWORD 3
        0x08, 0x3B, 0x00, 0xFF, // @0x28: DWORD 4
        0xEE, 0xFF, 0xFF, 0xFF, // @0x2C: DWORD 5
        0xFF, 0xFF, 0x00, 0x00, // @0x30: DWORD 6
        0xFF, 0xFF, 0x00, 0xFF, // @0x34: DWORD 7
        0x0C, 0x20, 0x0F, 0x52, // @0x38: DWORD 8 - erase types
        0x10, 0xD8, 0x00, 0xFF, // @0x3C: DWORD 9 - erase types
        0xFF, 0xFF, 0xFF, 0xFF, // @0x40: hole
        0xFF, 0xFF, 0xFF, 0xFF, // @0x44: hole
        0x00, 0x36, 0x00, 0x27, // @0x48: Macronix parameter table start
        0xF4, 0x4F, 0xFF, 0xFF, // @0x4C
        0xD9, 0xC8, 0xFF, 0xFF, // @0x50
        0xFF, 0xFF, 0xFF, 0xFF, // @0x54: Macronix parameter table end
    ];

    /// Mock SPI master that returns the MX25L6436E SFDP data
    struct MockSfdpFlash {
        sfdp_data: &'static [u8],
    }

    impl MockSfdpFlash {
        fn new() -> Self {
            Self {
                sfdp_data: &MX25L6436E_SFDP,
            }
        }
    }

    impl crate::programmer::SpiMaster for MockSfdpFlash {
        fn features(&self) -> crate::programmer::SpiFeatures {
            crate::programmer::SpiFeatures::empty()
        }

        fn max_read_len(&self) -> usize {
            256
        }

        fn max_write_len(&self) -> usize {
            256
        }

        fn execute(&mut self, cmd: &mut crate::spi::SpiCommand<'_>) -> crate::error::Result<()> {
            use crate::spi::opcodes;

            match cmd.opcode {
                opcodes::RDSFDP => {
                    // SFDP read: address is in cmd.address, dummy cycles expected
                    if let Some(addr) = cmd.address {
                        let addr = addr as usize;
                        let len = cmd.read_buf.len();
                        if addr < self.sfdp_data.len() {
                            let end = core::cmp::min(addr + len, self.sfdp_data.len());
                            let copy_len = end - addr;
                            cmd.read_buf[..copy_len].copy_from_slice(&self.sfdp_data[addr..end]);
                            // Fill rest with 0xFF if we read past the end
                            if copy_len < len {
                                cmd.read_buf[copy_len..].fill(0xFF);
                            }
                        } else {
                            cmd.read_buf.fill(0xFF);
                        }
                    }
                    Ok(())
                }
                opcodes::RDSR => {
                    // Return not busy
                    if !cmd.read_buf.is_empty() {
                        cmd.read_buf[0] = 0x00;
                    }
                    Ok(())
                }
                _ => Ok(()),
            }
        }

        fn delay_us(&mut self, _us: u32) {}
    }

    #[test]
    fn test_parse_mx25l6436e_sfdp() {
        let mut mock = MockSfdpFlash::new();
        let info = probe(&mut mock).expect("SFDP probe should succeed");

        // Verify SFDP header
        assert!(info.header.is_valid(), "SFDP signature should be valid");
        assert_eq!(info.header.revision.major, 1);
        assert_eq!(info.header.revision.minor, 0);
        assert_eq!(
            info.header.num_param_headers(),
            2,
            "Should have 2 parameter headers"
        );

        // Verify basic flash parameters
        let params = &info.basic_params;

        // MX25L6436E is 64 Mbit = 8 MiB
        // DWORD 2 = 0x03FFFFFF = density in bits - 1
        // (0x03FFFFFF + 1) / 8 = 8388608 bytes = 8 MiB
        assert_eq!(
            params.density_bytes,
            8 * 1024 * 1024,
            "Should be 8 MiB (64 Mbit)"
        );

        // Check erase types from DWORDs 8-9:
        // DWORD 8: 0x520F_200C
        //   Type 1: size=0x0C (4KB), opcode=0x20
        //   Type 2: size=0x0F (32KB), opcode=0x52
        // DWORD 9: 0xFF00_D810
        //   Type 3: size=0x10 (64KB), opcode=0xD8
        //   Type 4: size=0x00 (not used)

        // Find 4KB erase
        let erase_4k = params.erase_types.iter().find(|e| e.size == 4096);
        assert!(erase_4k.is_some(), "Should have 4KB erase");
        assert_eq!(erase_4k.unwrap().opcode, 0x20);

        // Find 32KB erase
        let erase_32k = params.erase_types.iter().find(|e| e.size == 32768);
        assert!(erase_32k.is_some(), "Should have 32KB erase");
        assert_eq!(erase_32k.unwrap().opcode, 0x52);

        // Find 64KB erase
        let erase_64k = params.erase_types.iter().find(|e| e.size == 65536);
        assert!(erase_64k.is_some(), "Should have 64KB erase");
        assert_eq!(erase_64k.unwrap().opcode, 0xD8);

        // Check addressing mode (from DWORD 1 bits [18:17])
        // 0xE5 = 0b11100101 -> bits [2:1] of byte 2 = 0b00 = 3-byte only
        assert_eq!(params.address_mode, AddressMode::ThreeByteOnly);

        // Check fast read support (from DWORD 1)
        // 0xE5 = bit 0 = 1 -> 1-1-2 supported? Need to check bit 16
        // Byte 2 (0xC9) bit 0 = 1 -> 1-1-2 supported
        // Actually DWORD1[16] is in byte 2
        assert!(params.fast_read_112, "Should support 1-1-2 fast read");

        // Verify we don't require 4-byte addressing for this 8MB chip
        assert!(
            !params.requires_4byte_addr(),
            "8 MiB chip should not require 4-byte addressing"
        );
    }

    #[test]
    #[cfg(feature = "alloc")]
    fn test_mx25l6436e_to_flash_chip() {
        let mut mock = MockSfdpFlash::new();
        let info = probe(&mut mock).expect("SFDP probe should succeed");

        // Convert to FlashChip
        let chip = to_flash_chip(&info, 0xC2, 0x2017); // Macronix MX25L6436E JEDEC ID

        assert_eq!(chip.jedec_manufacturer, 0xC2);
        assert_eq!(chip.jedec_device, 0x2017);
        assert_eq!(chip.total_size, 8 * 1024 * 1024);

        // Should have erase blocks
        assert!(!chip.erase_blocks.is_empty());

        // Should have 4KB, 32KB, and 64KB erase
        assert!(
            chip.erase_blocks.iter().any(|eb| eb.is_uniform() && eb.uniform_size() == Some(4096)),
            "Should have 4KB erase block"
        );
        assert!(
            chip.erase_blocks.iter().any(|eb| eb.is_uniform() && eb.uniform_size() == Some(32768)),
            "Should have 32KB erase block"
        );
        assert!(
            chip.erase_blocks.iter().any(|eb| eb.is_uniform() && eb.uniform_size() == Some(65536)),
            "Should have 64KB erase block"
        );

        // Should have SFDP feature flag
        assert!(chip.features.contains(crate::chip::Features::SFDP));
    }

    #[test]
    fn test_fast_read_params_parsing() {
        // Test DWORD 3 parsing (1-1-4 and 1-4-4 fast read)
        // High half: [31:24]=0x6B opcode, [23:21]=0 mode, [20:16]=8 dummy
        // Low half: [15:8]=0xEB opcode, [7:5]=2 mode, [4:0]=4 dummy
        //
        // DWORD 3 from MX25L6436E: 0x6B_08_FF_00 (bytes: 00, FF, 08, 6B)
        // Wait, the data is: 0x00, 0xFF, 0x08, 0x6B at offset 0x24
        // Little endian: 0x6B08FF00
        let dword3: u32 = 0x6B08FF00;

        let mut params = BasicFlashParams::default();
        parse_bfpt_dword3(dword3, &mut params);

        // High half: 1S-1S-4S
        // [31:24] = 0x6B (opcode)
        // [23:21] = 0b000 = 0 mode clocks
        // [20:16] = 0b01000 = 8 dummy clocks
        assert!(params.fast_read_114_params.is_supported());
        assert_eq!(params.fast_read_114_params.opcode, 0x6B);
        assert_eq!(params.fast_read_114_params.mode_clocks, 0);
        assert_eq!(params.fast_read_114_params.dummy_clocks, 8);

        // Low half: 1S-4S-4S
        // [15:8] = 0xFF (not supported indicator for this chip)
        // When opcode is 0x00, the mode is not supported
        // But 0xFF is also commonly used for "not supported"
        // Actually, looking at the data: 0xFF00 -> opcode=0xFF
        // Let's look at other test data

        // Test DWORD 4 parsing (1-2-2 and 1-1-2 fast read)
        // From MX25L6436E: bytes 0x08, 0x3B, 0x00, 0xFF at offset 0x28
        // Little endian: 0xFF003B08
        let dword4: u32 = 0xFF003B08;

        let mut params = BasicFlashParams::default();
        parse_bfpt_dword4(dword4, &mut params);

        // Low half: 1S-1S-2S
        // [15:8] = 0x3B (opcode)
        // [7:5] = 0b000 = 0 mode clocks
        // [4:0] = 0b01000 = 8 dummy clocks
        assert!(params.fast_read_112_params.is_supported());
        assert_eq!(params.fast_read_112_params.opcode, 0x3B);
        assert_eq!(params.fast_read_112_params.mode_clocks, 0);
        assert_eq!(params.fast_read_112_params.dummy_clocks, 8);
    }

    #[test]
    fn test_fast_read_params_from_halves() {
        // Test FastReadParams::from_high_half
        // [31:24]=0xEB, [23:21]=2 mode, [20:16]=4 dummy
        // 0xEB_4_4_xxxx = 0xEB4_40000
        let dword = 0xEB44_0000_u32;
        let params = FastReadParams::from_high_half(dword);
        assert!(params.is_supported());
        assert_eq!(params.opcode, 0xEB);
        assert_eq!(params.mode_clocks, 2);
        assert_eq!(params.dummy_clocks, 4);

        // Test FastReadParams::from_low_half
        // [15:8]=0xBB, [7:5]=1 mode, [4:0]=4 dummy
        let dword = 0x0000_BB24u32; // 0xBB in [15:8], 1 in [7:5], 4 in [4:0]
        let params = FastReadParams::from_low_half(dword);
        assert!(params.is_supported());
        assert_eq!(params.opcode, 0xBB);
        assert_eq!(params.mode_clocks, 1);
        assert_eq!(params.dummy_clocks, 4);

        // Test unsupported (opcode 0x00)
        let dword = 0x0000_0000_u32;
        let params = FastReadParams::from_high_half(dword);
        assert!(!params.is_supported());
    }

    #[test]
    fn test_4byte_addr_instructions_parsing() {
        // Test DWORD 1 parsing
        // Bits set: READ (0), FAST_READ (1), PAGE_PROGRAM (6), ERASE_TYPE_1 (9)
        let dword1: u32 = (1 << 0) | (1 << 1) | (1 << 6) | (1 << 9);
        let instr = FourByteAddrInstructions::from_dword1(dword1);

        assert!(instr.supports_4ba_read());
        assert!(instr.supports_4ba_fast_read());
        assert!(instr.supports_4ba_page_program());
        assert!(instr.supports_any_4ba_erase());
        assert!(instr.supports(FourByteAddrInstructions::ERASE_TYPE_1));
        assert!(!instr.supports(FourByteAddrInstructions::ERASE_TYPE_2));
    }

    #[test]
    fn test_4byte_addr_erase_opcodes_parsing() {
        // Test DWORD 2 parsing
        // Type 1: 0x21, Type 2: 0x5C, Type 3: 0xDC, Type 4: 0xDC
        let dword2: u32 = 0xDC_DC_5C_21;
        let opcodes = FourByteAddrEraseOpcodes::from_dword2(dword2);

        assert_eq!(opcodes.erase_type_1, 0x21);
        assert_eq!(opcodes.erase_type_2, 0x5C);
        assert_eq!(opcodes.erase_type_3, 0xDC);
        assert_eq!(opcodes.erase_type_4, 0xDC);

        assert_eq!(opcodes.opcode_for_type(0), Some(0x21));
        assert_eq!(opcodes.opcode_for_type(1), Some(0x5C));
        assert_eq!(opcodes.opcode_for_type(2), Some(0xDC));
        assert_eq!(opcodes.opcode_for_type(3), Some(0xDC));
        assert_eq!(opcodes.opcode_for_type(4), None);

        // Test with some unsupported (0x00)
        let dword2: u32 = 0x00_DC_00_21;
        let opcodes = FourByteAddrEraseOpcodes::from_dword2(dword2);
        assert_eq!(opcodes.opcode_for_type(0), Some(0x21));
        assert_eq!(opcodes.opcode_for_type(1), None); // 0x00 means not supported
        assert_eq!(opcodes.opcode_for_type(2), Some(0xDC));
        assert_eq!(opcodes.opcode_for_type(3), None); // 0x00 means not supported
    }

    #[test]
    fn test_mx25l6436e_fast_read_params() {
        let mut mock = MockSfdpFlash::new();
        let info = probe(&mut mock).expect("SFDP probe should succeed");

        let params = &info.basic_params;

        // Check that we parsed the fast read parameters from the SFDP data
        // DWORD 3: 0x6B08FF00 (at offset 0x24 relative to table start)
        //   1S-1S-4S: opcode=0x6B, mode=0, dummy=8
        assert!(params.fast_read_114_params.is_supported());
        assert_eq!(params.fast_read_114_params.opcode, 0x6B);
        assert_eq!(params.fast_read_114_params.dummy_clocks, 8);

        // DWORD 4: 0xFF003B08 (at offset 0x28)
        //   1S-1S-2S: opcode=0x3B, mode=0, dummy=8
        assert!(params.fast_read_112_params.is_supported());
        assert_eq!(params.fast_read_112_params.opcode, 0x3B);
        assert_eq!(params.fast_read_112_params.dummy_clocks, 8);
    }
}

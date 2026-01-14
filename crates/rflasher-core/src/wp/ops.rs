//! Write protection operations
//!
//! This module provides functions to read, write, and manipulate
//! write protection settings on SPI flash chips.

use super::ranges::{decode_range, find_bits_for_range};
use super::types::{
    BitWritability, RangeDecoder, RegBitInfo, StatusRegister, WpBits, WpConfig, WpMode, WpRange,
    WpRegBitMap,
};
use crate::error::Error;
use crate::programmer::SpiMaster;
use crate::protocol;
use maybe_async::maybe_async;

/// Write protection result type with detailed error information
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WpError {
    /// Chip does not support write protection
    ChipUnsupported,
    /// Failed to read status registers
    ReadFailed,
    /// Failed to write status registers
    WriteFailed,
    /// Written value did not match (verify failed)
    VerifyFailed,
    /// Requested range is not supported by chip
    RangeUnsupported,
    /// Requested mode is not supported by chip
    ModeUnsupported,
    /// Cannot enumerate available ranges
    RangeListUnavailable,
    /// Write Protect Selection (WPS) bit is set, indicating per-sector mode
    UnsupportedState,
    /// SPI communication error
    SpiError(Error),
}

impl From<Error> for WpError {
    fn from(e: Error) -> Self {
        WpError::SpiError(e)
    }
}

impl core::fmt::Display for WpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WpError::ChipUnsupported => write!(f, "chip does not support write protection"),
            WpError::ReadFailed => write!(f, "failed to read status registers"),
            WpError::WriteFailed => write!(f, "failed to write status registers"),
            WpError::VerifyFailed => write!(f, "verify failed: written value did not match"),
            WpError::RangeUnsupported => write!(f, "requested range is not supported"),
            WpError::ModeUnsupported => write!(f, "requested mode is not supported"),
            WpError::RangeListUnavailable => write!(f, "cannot enumerate available ranges"),
            WpError::UnsupportedState => {
                write!(f, "WPS bit set, per-sector protection mode not supported")
            }
            WpError::SpiError(e) => write!(f, "SPI error: {}", e),
        }
    }
}

/// Result type for write protection operations
pub type WpResult<T> = core::result::Result<T, WpError>;

/// Read a single bit from the appropriate status register
#[maybe_async]
async fn read_bit<M: SpiMaster + ?Sized>(
    master: &mut M,
    bit_info: &RegBitInfo,
) -> WpResult<Option<u8>> {
    let reg = match bit_info.reg {
        Some(r) => r,
        None => return Ok(None),
    };

    if matches!(bit_info.writability, BitWritability::NotPresent) {
        return Ok(None);
    }

    let sr_val = match reg {
        StatusRegister::Status1 => protocol::read_status1(master).await?,
        StatusRegister::Status2 => protocol::read_status2(master).await?,
        StatusRegister::Status3 => protocol::read_status3(master).await?,
        StatusRegister::Config => protocol::read_status2(master).await?, // Often same as SR2
    };

    let bit_val = (sr_val >> bit_info.bit_index) & 1;
    Ok(Some(bit_val))
}

/// Read all write protection bits from the chip
#[maybe_async]
pub async fn read_wp_bits<M: SpiMaster + ?Sized>(
    master: &mut M,
    bit_map: &WpRegBitMap,
) -> WpResult<WpBits> {
    let mut bits = WpBits::empty();

    // Read SRP
    bits.srp = read_bit(master, &bit_map.srp).await?;

    // Read SRL
    bits.srl = read_bit(master, &bit_map.srl).await?;

    // Read CMP
    bits.cmp = read_bit(master, &bit_map.cmp).await?;

    // Read SEC
    bits.sec = read_bit(master, &bit_map.sec).await?;

    // Read TB
    bits.tb = read_bit(master, &bit_map.tb).await?;

    // Read BP bits
    bits.bp_count = bit_map.bp_count();
    for i in 0..bits.bp_count {
        if let Some(val) = read_bit(master, &bit_map.bp[i]).await? {
            bits.bp[i] = val;
        }
    }

    // Check for WPS bit (per-sector protection mode)
    if let Some(1) = read_bit(master, &bit_map.wps).await? {
        return Err(WpError::UnsupportedState);
    }

    Ok(bits)
}

/// Read the current write protection configuration
#[maybe_async]
pub async fn read_wp_config<M: SpiMaster + ?Sized>(
    master: &mut M,
    bit_map: &WpRegBitMap,
    total_size: u32,
    decoder: RangeDecoder,
) -> WpResult<WpConfig> {
    let bits = read_wp_bits(master, bit_map).await?;
    let mode = bits.mode();
    let range = decode_range(&bits, total_size, decoder);

    Ok(WpConfig::new(mode, range))
}

/// Build status register values from WpBits
fn build_register_values(bits: &WpBits, bit_map: &WpRegBitMap) -> (u8, u8, u8) {
    let mut sr1: u8 = 0;
    let mut sr2: u8 = 0;
    let mut sr3: u8 = 0;

    // Helper to set a bit in the appropriate register
    let set_bit = |sr1: &mut u8, sr2: &mut u8, sr3: &mut u8, info: &RegBitInfo, val: u8| {
        if !info.is_present() || !info.is_writable() {
            return;
        }
        let bit = (val & 1) << info.bit_index;
        match info.reg {
            Some(StatusRegister::Status1) => *sr1 |= bit,
            Some(StatusRegister::Status2) | Some(StatusRegister::Config) => *sr2 |= bit,
            Some(StatusRegister::Status3) => *sr3 |= bit,
            None => {}
        }
    };

    // Set SRP
    if let Some(val) = bits.srp {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.srp, val);
    }

    // Set SRL
    if let Some(val) = bits.srl {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.srl, val);
    }

    // Set CMP
    if let Some(val) = bits.cmp {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.cmp, val);
    }

    // Set SEC
    if let Some(val) = bits.sec {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.sec, val);
    }

    // Set TB
    if let Some(val) = bits.tb {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.tb, val);
    }

    // Set BP bits
    for i in 0..bits.bp_count {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.bp[i], bits.bp[i]);
    }

    (sr1, sr2, sr3)
}

/// Read current register values preserving non-WP bits
#[maybe_async]
async fn read_current_registers<M: SpiMaster + ?Sized>(master: &mut M) -> WpResult<(u8, u8, u8)> {
    let sr1 = protocol::read_status1(master).await?;
    let sr2 = protocol::read_status2(master).await.unwrap_or(0);
    let sr3 = protocol::read_status3(master).await.unwrap_or(0);
    Ok((sr1, sr2, sr3))
}

/// Create a mask for bits that should be modified
fn build_register_masks(bit_map: &WpRegBitMap, bits: &WpBits) -> (u8, u8, u8) {
    let mut mask1: u8 = 0;
    let mut mask2: u8 = 0;
    let mut mask3: u8 = 0;

    let add_mask = |m1: &mut u8, m2: &mut u8, m3: &mut u8, info: &RegBitInfo| {
        if !info.is_present() || !info.is_writable() {
            return;
        }
        let bit = 1u8 << info.bit_index;
        match info.reg {
            Some(StatusRegister::Status1) => *m1 |= bit,
            Some(StatusRegister::Status2) | Some(StatusRegister::Config) => *m2 |= bit,
            Some(StatusRegister::Status3) => *m3 |= bit,
            None => {}
        }
    };

    if bits.srp.is_some() {
        add_mask(&mut mask1, &mut mask2, &mut mask3, &bit_map.srp);
    }
    if bits.srl.is_some() {
        add_mask(&mut mask1, &mut mask2, &mut mask3, &bit_map.srl);
    }
    if bits.cmp.is_some() {
        add_mask(&mut mask1, &mut mask2, &mut mask3, &bit_map.cmp);
    }
    if bits.sec.is_some() {
        add_mask(&mut mask1, &mut mask2, &mut mask3, &bit_map.sec);
    }
    if bits.tb.is_some() {
        add_mask(&mut mask1, &mut mask2, &mut mask3, &bit_map.tb);
    }
    for i in 0..bits.bp_count {
        add_mask(&mut mask1, &mut mask2, &mut mask3, &bit_map.bp[i]);
    }

    (mask1, mask2, mask3)
}

/// Write protection configuration options
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteOptions {
    /// Use volatile write (doesn't persist across power cycle)
    pub volatile: bool,
}

/// Write WP bits to the chip
#[maybe_async]
pub async fn write_wp_bits<M: SpiMaster + ?Sized>(
    master: &mut M,
    bits: &WpBits,
    bit_map: &WpRegBitMap,
    options: WriteOptions,
) -> WpResult<()> {
    // Read current values
    let (curr_sr1, curr_sr2, _curr_sr3) = read_current_registers(master).await?;

    // Build new values and masks
    let (new_sr1, new_sr2, _new_sr3) = build_register_values(bits, bit_map);
    let (mask1, mask2, _mask3) = build_register_masks(bit_map, bits);

    // Merge new values with current values (preserving non-WP bits)
    let final_sr1 = (curr_sr1 & !mask1) | (new_sr1 & mask1);
    let final_sr2 = (curr_sr2 & !mask2) | (new_sr2 & mask2);

    // Determine which registers need to be written
    let need_sr1 = mask1 != 0;
    let need_sr2 = mask2 != 0;

    if !need_sr1 && !need_sr2 {
        // Nothing to write
        return Ok(());
    }

    // Perform the write
    // TODO: Implement proper volatile write support using EWSR (0x50) instead of WREN
    // For now, both volatile and non-volatile writes use the same mechanism.
    // Volatile writes on some chips require:
    // 1. EWSR (Enable Write Status Register) instead of WREN
    // 2. Or writing to volatile SR copies that reset on power cycle
    let _ = options.volatile; // Acknowledge the option for future use
    if need_sr2 {
        protocol::write_status12(master, final_sr1, final_sr2).await?;
    } else {
        protocol::write_status1(master, final_sr1).await?;
    }

    // Verify the write
    let (verify_sr1, verify_sr2, _) = read_current_registers(master).await?;
    if (verify_sr1 & mask1) != (final_sr1 & mask1) {
        return Err(WpError::VerifyFailed);
    }
    if need_sr2 && (verify_sr2 & mask2) != (final_sr2 & mask2) {
        return Err(WpError::VerifyFailed);
    }

    Ok(())
}

/// Set the write protection mode
#[maybe_async]
pub async fn set_wp_mode<M: SpiMaster + ?Sized>(
    master: &mut M,
    mode: WpMode,
    bit_map: &WpRegBitMap,
    options: WriteOptions,
) -> WpResult<()> {
    // Only Disabled and Hardware modes can be set programmatically
    let (srp, srl) = match mode {
        WpMode::Disabled => (0, 0),
        WpMode::Hardware => (1, 0),
        WpMode::PowerCycle | WpMode::Permanent => {
            return Err(WpError::ModeUnsupported);
        }
    };

    let mut bits = WpBits::empty();

    if bit_map.srp.is_writable() {
        bits.srp = Some(srp);
    } else if srp != 0 {
        return Err(WpError::ModeUnsupported);
    }

    if bit_map.srl.is_writable() {
        bits.srl = Some(srl);
    } else if srl != 0 {
        return Err(WpError::ModeUnsupported);
    }

    write_wp_bits(master, &bits, bit_map, options).await
}

/// Set the protected range
#[maybe_async]
pub async fn set_wp_range<M: SpiMaster + ?Sized>(
    master: &mut M,
    range: &WpRange,
    bit_map: &WpRegBitMap,
    total_size: u32,
    decoder: RangeDecoder,
    options: WriteOptions,
) -> WpResult<()> {
    // Read current bits to use as template
    let current_bits = read_wp_bits(master, bit_map).await?;

    // Create a template with all available bits
    let mut template = WpBits::empty();
    template.bp_count = bit_map.bp_count();
    if bit_map.tb.is_present() {
        template.tb = Some(0);
    }
    if bit_map.sec.is_present() {
        template.sec = Some(0);
    }
    if bit_map.cmp.is_present() {
        template.cmp = Some(0);
    }

    // Find bits that produce the desired range
    let new_bits = find_bits_for_range(range, total_size, &template, decoder)
        .ok_or(WpError::RangeUnsupported)?;

    // Preserve SRP/SRL from current configuration
    let mut write_bits = new_bits;
    write_bits.srp = current_bits.srp;
    write_bits.srl = current_bits.srl;

    write_wp_bits(master, &write_bits, bit_map, options).await
}

/// Write complete write protection configuration
#[maybe_async]
pub async fn write_wp_config<M: SpiMaster + ?Sized>(
    master: &mut M,
    config: &WpConfig,
    bit_map: &WpRegBitMap,
    total_size: u32,
    decoder: RangeDecoder,
    options: WriteOptions,
) -> WpResult<()> {
    // First set the range
    set_wp_range(master, &config.range, bit_map, total_size, decoder, options).await?;

    // Then set the mode
    set_wp_mode(master, config.mode, bit_map, options).await
}

/// Disable all write protection
#[maybe_async]
pub async fn disable_wp<M: SpiMaster + ?Sized>(
    master: &mut M,
    bit_map: &WpRegBitMap,
    options: WriteOptions,
) -> WpResult<()> {
    let mut bits = WpBits::empty();

    // Set all BP bits to 0
    bits.bp_count = bit_map.bp_count();
    for i in 0..bits.bp_count {
        bits.bp[i] = 0;
    }

    // Set SRP/SRL to disabled
    if bit_map.srp.is_writable() {
        bits.srp = Some(0);
    }
    if bit_map.srl.is_writable() {
        bits.srl = Some(0);
    }

    // Clear other protection bits
    if bit_map.cmp.is_writable() {
        bits.cmp = Some(0);
    }
    if bit_map.tb.is_writable() {
        bits.tb = Some(0);
    }
    if bit_map.sec.is_writable() {
        bits.sec = Some(0);
    }

    write_wp_bits(master, &bits, bit_map, options).await
}

#[cfg(feature = "alloc")]
/// Get all available protection ranges for a chip
pub fn get_available_ranges(
    bit_map: &WpRegBitMap,
    total_size: u32,
    decoder: RangeDecoder,
) -> alloc::vec::Vec<WpRange> {
    // Create a template with all available bits
    let mut template = WpBits::empty();
    template.bp_count = bit_map.bp_count();
    if bit_map.tb.is_present() {
        template.tb = Some(0);
    }
    if bit_map.sec.is_present() {
        template.sec = Some(0);
    }
    if bit_map.cmp.is_present() {
        template.cmp = Some(0);
    }

    super::ranges::get_all_ranges(&template, total_size, decoder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_from_bits() {
        let mut bits = WpBits::empty();

        bits.srp = Some(0);
        bits.srl = Some(0);
        assert_eq!(bits.mode(), WpMode::Disabled);

        bits.srp = Some(1);
        bits.srl = Some(0);
        assert_eq!(bits.mode(), WpMode::Hardware);

        bits.srp = Some(0);
        bits.srl = Some(1);
        assert_eq!(bits.mode(), WpMode::PowerCycle);

        bits.srp = Some(1);
        bits.srl = Some(1);
        assert_eq!(bits.mode(), WpMode::Permanent);
    }

    #[test]
    fn test_build_register_values() {
        let bit_map = WpRegBitMap::winbond_standard();
        let mut bits = WpBits::empty();
        bits.set_bp_value(0b111, 3);
        bits.tb = Some(1);
        bits.sec = Some(0);
        bits.srp = Some(1);

        let (sr1, sr2, _sr3) = build_register_values(&bits, &bit_map);

        // BP0=bit2, BP1=bit3, BP2=bit4, TB=bit5, SEC=bit6, SRP=bit7
        assert_eq!(sr1 & 0b00011100, 0b00011100); // BP0-BP2 set
        assert_eq!(sr1 & 0b00100000, 0b00100000); // TB set
        assert_eq!(sr1 & 0b01000000, 0); // SEC clear
        assert_eq!(sr1 & 0b10000000, 0b10000000); // SRP set
        assert_eq!(sr2, 0); // No SR2 bits set
    }
}

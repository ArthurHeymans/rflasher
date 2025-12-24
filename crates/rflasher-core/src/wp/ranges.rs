//! Write protection range decoding
//!
//! This module implements the algorithms to decode write protection bits
//! (BP, TB, SEC, CMP) into protected address ranges.

use super::types::{RangeDecoder, WpBits, WpRange};

/// A protected range in the flash (legacy type alias for backward compatibility)
pub type ProtectedRange = WpRange;

impl WpRange {
    /// Create a new protected range from start and end addresses
    pub const fn from_start_end(start: u32, end: u32) -> Self {
        Self {
            start,
            len: end.saturating_sub(start),
        }
    }
}

/// Decode the protected range from WP bits using the specified decoder
pub fn decode_range(bits: &WpBits, total_size: u32, decoder: RangeDecoder) -> WpRange {
    match decoder {
        RangeDecoder::Spi25 => decode_range_spi25(bits, total_size),
        RangeDecoder::Spi25_64kBlock => decode_range_spi25_64k_block(bits, total_size),
        RangeDecoder::Spi25BitCmp => decode_range_spi25_bit_cmp(bits, total_size),
        RangeDecoder::Spi25_2xBlock => decode_range_spi25_2x_block(bits, total_size),
    }
}

/// Standard SPI25 range decoding
///
/// This is the most common algorithm used by Winbond, GigaDevice, ISSI, etc.
/// The protected size is calculated as: block_size * 2^(bp - offset)
/// where block_size depends on SEC bit (4K sectors or 64K blocks).
pub fn decode_range_spi25(bits: &WpBits, total_size: u32) -> WpRange {
    decode_range_generic(bits, total_size, false, false)
}

/// SPI25 range decoding with fixed 64K blocks
///
/// Ignores SEC bit and always uses 64K block granularity.
pub fn decode_range_spi25_64k_block(bits: &WpBits, total_size: u32) -> WpRange {
    decode_range_generic(bits, total_size, true, false)
}

/// SPI25 range decoding where CMP inverts BP bits
///
/// Used by some Macronix chips where CMP XORs with the BP value
/// rather than complementing the resulting range.
pub fn decode_range_spi25_bit_cmp(bits: &WpBits, total_size: u32) -> WpRange {
    let mut modified_bits = *bits;

    if bits.cmp == Some(1) {
        // CMP inverts the BP bits
        let bp_val = bits.bp_value();
        let max_bp = (1u8 << bits.bp_count) - 1;
        let inverted = bp_val ^ max_bp;
        modified_bits.set_bp_value(inverted, bits.bp_count);
        modified_bits.cmp = Some(0); // Don't apply CMP again in generic decoder
    }

    decode_range_generic(&modified_bits, total_size, false, false)
}

/// SPI25 range decoding with doubled block coefficient
///
/// Used by chips that have an extra BP bit, where the coefficient
/// is effectively doubled.
pub fn decode_range_spi25_2x_block(bits: &WpBits, total_size: u32) -> WpRange {
    decode_range_generic(bits, total_size, false, true)
}

/// Generic range decoding implementation
///
/// # Algorithm
/// 1. BP=0 means no protection, BP=max means full chip protection
/// 2. Otherwise: protected_size = block_size * 2^(bp - offset)
/// 3. SEC bit selects 4K sectors vs 64K blocks
/// 4. TB bit selects top (high addresses) vs bottom (low addresses)
/// 5. CMP bit complements (inverts) the protected range
fn decode_range_generic(
    bits: &WpBits,
    total_size: u32,
    fixed_64k: bool,
    double_coeff: bool,
) -> WpRange {
    let bp = bits.bp_value();
    let bp_count = bits.bp_count;

    // BP = 0 means no protection
    if bp == 0 {
        let range = WpRange::none();
        return apply_cmp(range, bits.cmp, total_size);
    }

    // BP = all 1s typically means full chip protection
    let max_bp = (1u8 << bp_count).saturating_sub(1);
    if bp == max_bp && bp_count > 0 {
        let range = WpRange::full(total_size);
        return apply_cmp(range, bits.cmp, total_size);
    }

    // Determine block size based on SEC bit
    let block_size: u32 = if fixed_64k {
        64 * 1024
    } else if bits.sec == Some(1) {
        // SEC=1 means 4K sector protection
        4 * 1024
    } else {
        // SEC=0 means 64K block protection
        64 * 1024
    };

    // Calculate protected size
    // For typical chips: size = block_size * 2^(bp - 1)
    // This gives: BP1=1 block, BP2=2 blocks, BP3=4 blocks, etc.
    let exponent = bp.saturating_sub(1);
    let mut coefficient: u32 = 1 << exponent;

    if double_coeff {
        coefficient *= 2;
    }

    let mut protected_size = block_size.saturating_mul(coefficient);

    // Clamp protected size based on SEC bit for sector protection
    // SEC=1 with high BP typically caps at 32K
    if bits.sec == Some(1) && !fixed_64k {
        protected_size = protected_size.min(32 * 1024);
    }

    // Clamp to chip size
    protected_size = protected_size.min(total_size);

    // Calculate range based on TB (Top/Bottom) bit
    let range = if bits.tb == Some(1) {
        // TB=1: protect from bottom (low addresses)
        WpRange::new(0, protected_size)
    } else {
        // TB=0: protect from top (high addresses)
        WpRange::new(total_size.saturating_sub(protected_size), protected_size)
    };

    // Apply CMP (complement) bit
    apply_cmp(range, bits.cmp, total_size)
}

/// Apply the CMP (complement) bit to invert the protected range
fn apply_cmp(range: WpRange, cmp: Option<u8>, total_size: u32) -> WpRange {
    if cmp != Some(1) {
        return range;
    }

    // CMP=1 inverts the protection
    if range.len == 0 {
        // No protection -> full protection
        WpRange::full(total_size)
    } else if range.start == 0 {
        // Bottom protection -> protect everything above
        WpRange::new(range.end(), total_size.saturating_sub(range.end()))
    } else {
        // Top protection -> protect everything below
        WpRange::new(0, range.start)
    }
}

/// Find WP bits that produce the given range
///
/// This function tries all possible combinations of BP/TB/SEC/CMP bits
/// and returns the bits that produce a range matching the target.
/// Returns None if no valid combination exists.
pub fn find_bits_for_range(
    target: &WpRange,
    total_size: u32,
    template: &WpBits,
    decoder: RangeDecoder,
) -> Option<WpBits> {
    let bp_count = template.bp_count;
    let max_bp: u8 = if bp_count > 0 { (1 << bp_count) - 1 } else { 0 };

    // Try all combinations of writable bits
    let tb_values: &[Option<u8>] = if template.tb.is_some() {
        &[Some(0), Some(1)]
    } else {
        &[None]
    };

    let sec_values: &[Option<u8>] = if template.sec.is_some() {
        &[Some(0), Some(1)]
    } else {
        &[None]
    };

    let cmp_values: &[Option<u8>] = if template.cmp.is_some() {
        &[Some(0), Some(1)]
    } else {
        &[None]
    };

    for &tb in tb_values {
        for &sec in sec_values {
            for &cmp in cmp_values {
                for bp in 0..=max_bp {
                    let mut test_bits = *template;
                    test_bits.tb = tb;
                    test_bits.sec = sec;
                    test_bits.cmp = cmp;
                    test_bits.set_bp_value(bp, bp_count);

                    let decoded = decode_range(&test_bits, total_size, decoder);
                    if decoded.start == target.start && decoded.len == target.len {
                        return Some(test_bits);
                    }
                }
            }
        }
    }

    None
}

/// Get all possible protected ranges for a chip
///
/// This enumerates all valid combinations of BP/TB/SEC/CMP bits
/// and returns the unique ranges that can be achieved.
#[cfg(feature = "alloc")]
pub fn get_all_ranges(
    template: &WpBits,
    total_size: u32,
    decoder: RangeDecoder,
) -> alloc::vec::Vec<WpRange> {
    use alloc::vec::Vec;

    let bp_count = template.bp_count;
    let max_bp: u8 = if bp_count > 0 { (1 << bp_count) - 1 } else { 0 };

    let mut ranges = Vec::new();

    let tb_values: &[Option<u8>] = if template.tb.is_some() {
        &[Some(0), Some(1)]
    } else {
        &[None]
    };

    let sec_values: &[Option<u8>] = if template.sec.is_some() {
        &[Some(0), Some(1)]
    } else {
        &[None]
    };

    let cmp_values: &[Option<u8>] = if template.cmp.is_some() {
        &[Some(0), Some(1)]
    } else {
        &[None]
    };

    for &tb in tb_values {
        for &sec in sec_values {
            for &cmp in cmp_values {
                for bp in 0..=max_bp {
                    let mut test_bits = *template;
                    test_bits.tb = tb;
                    test_bits.sec = sec;
                    test_bits.cmp = cmp;
                    test_bits.set_bp_value(bp, bp_count);

                    let range = decode_range(&test_bits, total_size, decoder);

                    // Add if not already present
                    if !ranges
                        .iter()
                        .any(|r: &WpRange| r.start == range.start && r.len == range.len)
                    {
                        ranges.push(range);
                    }
                }
            }
        }
    }

    // Sort by start address, then by length
    ranges.sort_by(|a, b| a.start.cmp(&b.start).then(a.len.cmp(&b.len)));

    ranges
}

/// Decode write protection status for standard BP0-BP2 + TB + SEC + CMP scheme
///
/// This is a legacy function for backward compatibility.
/// Consider using `decode_range` with `WpBits` instead.
pub fn decode_spi25_wp(
    sr1: u8,
    sr2: u8,
    total_size: u32,
    has_tb: bool,
    has_sec: bool,
    has_cmp: bool,
) -> WpRange {
    use crate::spi::opcodes::{SR1_BP0, SR1_BP1, SR1_BP2, SR1_SEC, SR1_TB};

    // Extract bits from status registers
    let mut bits = WpBits::empty();

    // Extract BP bits (bits 2, 3, 4 of SR1)
    let bp0 = (sr1 & SR1_BP0) >> 2;
    let bp1 = (sr1 & SR1_BP1) >> 3;
    let bp2 = (sr1 & SR1_BP2) >> 4;
    bits.bp[0] = bp0;
    bits.bp[1] = bp1;
    bits.bp[2] = bp2;
    bits.bp_count = 3;

    bits.tb = if has_tb {
        Some((sr1 & SR1_TB) >> 5)
    } else {
        None
    };
    bits.sec = if has_sec {
        Some((sr1 & SR1_SEC) >> 6)
    } else {
        None
    };
    bits.cmp = if has_cmp {
        Some((sr2 & 0x40) >> 6)
    } else {
        None
    };

    decode_range_spi25(&bits, total_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_protection() {
        let mut bits = WpBits::empty();
        bits.bp_count = 3;
        bits.tb = Some(0);

        let range = decode_range_spi25(&bits, 16 * 1024 * 1024);
        assert_eq!(range.len, 0);
    }

    #[test]
    fn test_full_protection() {
        let mut bits = WpBits::empty();
        bits.set_bp_value(0b111, 3);
        bits.tb = Some(0);

        let range = decode_range_spi25(&bits, 16 * 1024 * 1024);
        assert_eq!(range.start, 0);
        assert_eq!(range.len, 16 * 1024 * 1024);
    }

    #[test]
    fn test_top_protection() {
        let mut bits = WpBits::empty();
        bits.set_bp_value(1, 3); // BP=1 = 64K
        bits.tb = Some(0); // Top
        bits.sec = Some(0);

        let total = 16 * 1024 * 1024;
        let range = decode_range_spi25(&bits, total);

        // Should protect last 64K
        assert_eq!(range.len, 64 * 1024);
        assert_eq!(range.start, total - 64 * 1024);
    }

    #[test]
    fn test_bottom_protection() {
        let mut bits = WpBits::empty();
        bits.set_bp_value(1, 3); // BP=1 = 64K
        bits.tb = Some(1); // Bottom
        bits.sec = Some(0);

        let range = decode_range_spi25(&bits, 16 * 1024 * 1024);

        // Should protect first 64K
        assert_eq!(range.start, 0);
        assert_eq!(range.len, 64 * 1024);
    }

    #[test]
    fn test_cmp_inverts_range() {
        let mut bits = WpBits::empty();
        bits.set_bp_value(1, 3); // BP=1 = 64K
        bits.tb = Some(1); // Bottom (first 64K protected)
        bits.sec = Some(0);
        bits.cmp = Some(1); // Invert

        let total = 16 * 1024 * 1024;
        let range = decode_range_spi25(&bits, total);

        // With CMP, should protect everything EXCEPT first 64K
        assert_eq!(range.start, 64 * 1024);
        assert_eq!(range.len, total - 64 * 1024);
    }

    #[test]
    fn test_sector_protection() {
        let mut bits = WpBits::empty();
        bits.set_bp_value(1, 3);
        bits.tb = Some(1);
        bits.sec = Some(1); // 4K sectors

        let range = decode_range_spi25(&bits, 16 * 1024 * 1024);

        assert_eq!(range.start, 0);
        assert_eq!(range.len, 4 * 1024);
    }

    #[test]
    fn test_wp_range_overlaps() {
        let range = WpRange::new(1000, 500);

        assert!(range.overlaps(900, 200)); // Overlaps start
        assert!(range.overlaps(1200, 500)); // Overlaps end
        assert!(range.overlaps(1100, 100)); // Fully contained
        assert!(range.overlaps(900, 1000)); // Fully contains
        assert!(!range.overlaps(0, 1000)); // Before
        assert!(!range.overlaps(1500, 100)); // After
    }
}

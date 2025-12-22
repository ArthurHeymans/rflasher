//! Write protection range decoding

/// A protected range in the flash
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtectedRange {
    /// Start address of protected region
    pub start: u32,
    /// End address of protected region (exclusive)
    pub end: u32,
}

impl ProtectedRange {
    /// Create a new protected range
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Create a range representing no protection
    pub const fn none() -> Self {
        Self { start: 0, end: 0 }
    }

    /// Create a range representing full chip protection
    pub const fn full(size: u32) -> Self {
        Self {
            start: 0,
            end: size,
        }
    }

    /// Check if this range protects any part of the chip
    pub const fn is_protected(&self) -> bool {
        self.end > self.start
    }

    /// Get the size of the protected region
    pub const fn size(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// Check if an address is within the protected range
    pub const fn contains(&self, addr: u32) -> bool {
        addr >= self.start && addr < self.end
    }

    /// Check if a range overlaps with the protected region
    pub const fn overlaps(&self, start: u32, len: u32) -> bool {
        let range_end = start + len;
        !(range_end <= self.start || start >= self.end)
    }
}

/// Decode write protection status for standard BP0-BP2 + TB + SEC + CMP scheme
///
/// This is the most common write protection scheme used by Winbond, GigaDevice,
/// and many other manufacturers.
pub fn decode_spi25_wp(
    sr1: u8,
    sr2: u8,
    total_size: u32,
    has_tb: bool,
    has_sec: bool,
    has_cmp: bool,
) -> ProtectedRange {
    use crate::spi::opcodes::{SR1_BP0, SR1_BP1, SR1_BP2, SR1_SEC, SR1_TB};

    let bp = ((sr1 & SR1_BP0) >> 2) | ((sr1 & SR1_BP1) >> 2) | ((sr1 & SR1_BP2) >> 2);

    let tb = has_tb && (sr1 & SR1_TB) != 0;
    let sec = has_sec && (sr1 & SR1_SEC) != 0;
    let cmp = has_cmp && (sr2 & 0x40) != 0; // CMP is usually bit 6 of SR2

    // Calculate protected size based on BP bits
    let protected_size = match bp {
        0 => 0,
        1 => {
            if sec {
                4 * 1024
            } else {
                64 * 1024
            }
        }
        2 => {
            if sec {
                8 * 1024
            } else {
                128 * 1024
            }
        }
        3 => {
            if sec {
                16 * 1024
            } else {
                256 * 1024
            }
        }
        4 => {
            if sec {
                32 * 1024
            } else {
                512 * 1024
            }
        }
        5 => {
            if sec {
                64 * 1024
            } else {
                1024 * 1024
            }
        }
        6 => {
            if sec {
                128 * 1024
            } else {
                2 * 1024 * 1024
            }
        }
        _ => total_size,
    };

    // Clamp to chip size
    let protected_size = core::cmp::min(protected_size, total_size);

    // Calculate the range
    let (start, end) = if tb {
        // Bottom protection
        (0, protected_size)
    } else {
        // Top protection
        (total_size.saturating_sub(protected_size), total_size)
    };

    // Apply CMP bit (inverts the protected range)
    let (start, end) = if cmp {
        if start == 0 && end == 0 {
            (0, total_size)
        } else if start == 0 {
            (end, total_size)
        } else {
            (0, start)
        }
    } else {
        (start, end)
    };

    ProtectedRange::new(start, end)
}

//! Write protection types and structures
//!
//! This module provides types for working with SPI flash write protection,
//! based on the standard status register protection scheme.

/// Maximum number of block protect (BP) bits supported
pub const MAX_BP_BITS: usize = 4;

/// Write protection mode
///
/// This represents the hardware write protection state of a flash chip,
/// based on the SRP (Status Register Protect) and SRL (Status Register Lock) bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub enum WpMode {
    /// Write protection is disabled - status register can be freely modified
    #[default]
    Disabled,
    /// Hardware write protection - WP# pin must be inactive to modify status register
    Hardware,
    /// Power-cycle protection - status register cannot be modified until power cycle
    PowerCycle,
    /// Permanent protection - status register is locked and cannot be modified (OTP)
    Permanent,
}

impl core::fmt::Display for WpMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WpMode::Disabled => write!(f, "disabled"),
            WpMode::Hardware => write!(f, "hardware"),
            WpMode::PowerCycle => write!(f, "power-cycle"),
            WpMode::Permanent => write!(f, "permanent"),
        }
    }
}

/// Write protection bit values
///
/// This structure holds the current values of all write protection-related
/// bits from the flash chip's status registers.
#[derive(Debug, Clone, Copy, Default)]
pub struct WpBits {
    /// Status Register Protect bit value (SRP0/SRP)
    pub srp: Option<u8>,
    /// Status Register Lock bit value (SRL/SRP1)
    pub srl: Option<u8>,
    /// Complement bit value (CMP)
    pub cmp: Option<u8>,
    /// Sector/Block protect bit value (SEC)
    pub sec: Option<u8>,
    /// Top/Bottom bit value (TB)
    pub tb: Option<u8>,
    /// Block Protect bit values (BP0, BP1, BP2, BP3)
    /// The number of valid bits is determined by `bp_count`
    pub bp: [u8; MAX_BP_BITS],
    /// Number of BP bits available (0-4)
    pub bp_count: usize,
}

impl WpBits {
    /// Create empty WpBits with no bits set
    pub const fn empty() -> Self {
        Self {
            srp: None,
            srl: None,
            cmp: None,
            sec: None,
            tb: None,
            bp: [0; MAX_BP_BITS],
            bp_count: 0,
        }
    }

    /// Get the BP bits as a single integer value
    ///
    /// BP0 is the LSB, higher BP bits are in higher positions
    pub fn bp_value(&self) -> u8 {
        let mut val = 0u8;
        for i in 0..self.bp_count {
            if self.bp[i] != 0 {
                val |= 1 << i;
            }
        }
        val
    }

    /// Set BP bits from a single integer value
    pub fn set_bp_value(&mut self, val: u8, count: usize) {
        self.bp_count = count.min(MAX_BP_BITS);
        for i in 0..MAX_BP_BITS {
            self.bp[i] = if i < self.bp_count { (val >> i) & 1 } else { 0 };
        }
    }

    /// Determine the write protection mode from SRP and SRL bits
    pub fn mode(&self) -> WpMode {
        let srp = self.srp.unwrap_or(0);
        let srl = self.srl.unwrap_or(0);

        match (srl, srp) {
            (0, 0) => WpMode::Disabled,
            (0, 1) => WpMode::Hardware,
            (1, 0) => WpMode::PowerCycle,
            (1, 1) => WpMode::Permanent,
            _ => WpMode::Disabled,
        }
    }
}

/// Write protection configuration
///
/// This combines the protection mode with the protected range.
#[derive(Debug, Clone, Copy)]
pub struct WpConfig {
    /// Protection mode
    pub mode: WpMode,
    /// Protected range
    pub range: WpRange,
}

impl Default for WpConfig {
    fn default() -> Self {
        Self {
            mode: WpMode::Disabled,
            range: WpRange::none(),
        }
    }
}

impl WpConfig {
    /// Create a new WpConfig
    pub const fn new(mode: WpMode, range: WpRange) -> Self {
        Self { mode, range }
    }

    /// Check if write protection is active
    pub fn is_protected(&self) -> bool {
        self.range.is_protected()
    }
}

/// A protected range in the flash
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WpRange {
    /// Start address of protected region
    pub start: u32,
    /// Length of protected region in bytes
    pub len: u32,
}

impl WpRange {
    /// Create a new protected range
    pub const fn new(start: u32, len: u32) -> Self {
        Self { start, len }
    }

    /// Create a range representing no protection
    pub const fn none() -> Self {
        Self { start: 0, len: 0 }
    }

    /// Create a range representing full chip protection
    pub const fn full(size: u32) -> Self {
        Self {
            start: 0,
            len: size,
        }
    }

    /// Check if this range protects any part of the chip
    pub const fn is_protected(&self) -> bool {
        self.len > 0
    }

    /// Get the end address (exclusive)
    pub const fn end(&self) -> u32 {
        self.start.saturating_add(self.len)
    }

    /// Check if an address is within the protected range
    pub const fn contains(&self, addr: u32) -> bool {
        addr >= self.start && addr < self.end()
    }

    /// Check if a range overlaps with the protected region
    pub const fn overlaps(&self, start: u32, len: u32) -> bool {
        let range_end = start.saturating_add(len);
        !(range_end <= self.start || start >= self.end())
    }
}

impl core::fmt::Display for WpRange {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.len == 0 {
            write!(f, "none")
        } else {
            write!(
                f,
                "0x{:08x}-0x{:08x} ({} bytes)",
                self.start,
                self.end(),
                self.len
            )
        }
    }
}

/// Which status register a bit is located in
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusRegister {
    /// Status Register 1 (read with RDSR 0x05)
    Status1,
    /// Status Register 2 (read with RDSR2 0x35)
    Status2,
    /// Status Register 3 (read with RDSR3 0x15)
    Status3,
    /// Configuration register (some chips use different names)
    Config,
}

/// Writability of a register bit
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BitWritability {
    /// Bit is not present on this chip
    #[default]
    NotPresent,
    /// Read-only (cannot be modified)
    ReadOnly,
    /// Read-write (can be modified)
    ReadWrite,
    /// One-Time Programmable (can only be programmed once)
    Otp,
}

/// Information about a single register bit
#[derive(Debug, Clone, Copy, Default)]
pub struct RegBitInfo {
    /// Which register the bit is in
    pub reg: Option<StatusRegister>,
    /// Bit index within the register (0-7)
    pub bit_index: u8,
    /// Whether this bit can be written
    pub writability: BitWritability,
}

impl RegBitInfo {
    /// Create a new RegBitInfo for a present, writable bit
    pub const fn new(reg: StatusRegister, bit_index: u8, writability: BitWritability) -> Self {
        Self {
            reg: Some(reg),
            bit_index,
            writability,
        }
    }

    /// Create a RegBitInfo for a bit that is not present
    pub const fn not_present() -> Self {
        Self {
            reg: None,
            bit_index: 0,
            writability: BitWritability::NotPresent,
        }
    }

    /// Check if this bit is present on the chip
    pub const fn is_present(&self) -> bool {
        self.reg.is_some() && !matches!(self.writability, BitWritability::NotPresent)
    }

    /// Check if this bit can be written
    pub const fn is_writable(&self) -> bool {
        matches!(
            self.writability,
            BitWritability::ReadWrite | BitWritability::Otp
        )
    }
}

/// Complete register bit map for write protection
///
/// This structure describes where all write protection-related bits
/// are located in a chip's status registers.
#[derive(Debug, Clone, Copy, Default)]
pub struct WpRegBitMap {
    /// Status Register Protect (SRP0/SRP)
    pub srp: RegBitInfo,
    /// Status Register Lock (SRL/SRP1)
    pub srl: RegBitInfo,
    /// Complement bit (CMP)
    pub cmp: RegBitInfo,
    /// Sector/Block protect (SEC)
    pub sec: RegBitInfo,
    /// Top/Bottom (TB)
    pub tb: RegBitInfo,
    /// Block Protect bits (BP0, BP1, BP2, BP3)
    pub bp: [RegBitInfo; MAX_BP_BITS],
    /// Write Protect Selection (WPS) - indicates per-sector protection mode
    pub wps: RegBitInfo,
}

impl WpRegBitMap {
    /// Create an empty bit map with no bits defined
    pub const fn empty() -> Self {
        Self {
            srp: RegBitInfo::not_present(),
            srl: RegBitInfo::not_present(),
            cmp: RegBitInfo::not_present(),
            sec: RegBitInfo::not_present(),
            tb: RegBitInfo::not_present(),
            bp: [RegBitInfo::not_present(); MAX_BP_BITS],
            wps: RegBitInfo::not_present(),
        }
    }

    /// Standard Winbond-style register layout (most common)
    ///
    /// SR1: SRP0(7), SEC(6), TB(5), BP2(4), BP1(3), BP0(2), WEL(1), BUSY(0)
    /// SR2: SUS(7), CMP(6), LB3(5), LB2(4), LB1(3), R(2), QE(1), SRL(0)
    pub const fn winbond_standard() -> Self {
        Self {
            srp: RegBitInfo::new(StatusRegister::Status1, 7, BitWritability::ReadWrite),
            srl: RegBitInfo::new(StatusRegister::Status2, 0, BitWritability::ReadWrite),
            cmp: RegBitInfo::new(StatusRegister::Status2, 6, BitWritability::ReadWrite),
            sec: RegBitInfo::new(StatusRegister::Status1, 6, BitWritability::ReadWrite),
            tb: RegBitInfo::new(StatusRegister::Status1, 5, BitWritability::ReadWrite),
            bp: [
                RegBitInfo::new(StatusRegister::Status1, 2, BitWritability::ReadWrite),
                RegBitInfo::new(StatusRegister::Status1, 3, BitWritability::ReadWrite),
                RegBitInfo::new(StatusRegister::Status1, 4, BitWritability::ReadWrite),
                RegBitInfo::not_present(),
            ],
            wps: RegBitInfo::not_present(),
        }
    }

    /// Standard Winbond-style layout with BP3 (for larger chips)
    pub const fn winbond_with_bp3() -> Self {
        Self {
            srp: RegBitInfo::new(StatusRegister::Status1, 7, BitWritability::ReadWrite),
            srl: RegBitInfo::new(StatusRegister::Status2, 0, BitWritability::ReadWrite),
            cmp: RegBitInfo::new(StatusRegister::Status2, 6, BitWritability::ReadWrite),
            sec: RegBitInfo::new(StatusRegister::Status1, 6, BitWritability::ReadWrite),
            tb: RegBitInfo::new(StatusRegister::Status1, 5, BitWritability::ReadWrite),
            bp: [
                RegBitInfo::new(StatusRegister::Status1, 2, BitWritability::ReadWrite),
                RegBitInfo::new(StatusRegister::Status1, 3, BitWritability::ReadWrite),
                RegBitInfo::new(StatusRegister::Status1, 4, BitWritability::ReadWrite),
                RegBitInfo::new(StatusRegister::Status2, 2, BitWritability::ReadWrite),
            ],
            wps: RegBitInfo::not_present(),
        }
    }

    /// Get the number of BP bits present
    pub fn bp_count(&self) -> usize {
        self.bp.iter().filter(|b| b.is_present()).count()
    }
}

/// Range decoding function type
///
/// Different chips use different algorithms to decode BP/TB/SEC/CMP bits
/// into a protected range. This enum identifies which algorithm to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub enum RangeDecoder {
    /// Standard SPI25 decoding with variable block sizes
    #[default]
    Spi25,
    /// Fixed 64K block sizes
    Spi25_64kBlock,
    /// CMP bit inverts the BP bits instead of the range (some Macronix)
    Spi25BitCmp,
    /// Double coefficient for chips with extra BP bit
    Spi25_2xBlock,
}

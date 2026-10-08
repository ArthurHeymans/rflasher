//! Chip-specific SPI NOR protection procedures.

/// Procedure for temporarily removing software protection before a mutation.
///
/// This is separate from a range decoder: knowing how to unprotect a chip does
/// not imply that its BP bits can be interpreted as a Winbond protected range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Unlock {
    /// No documented procedure (e.g. an SFDP-only chip). Never guess WRSR bits.
    #[default]
    Unknown,
    /// The chip definition does not require an unlock command.
    None,
    /// Clear SR1 bits 2 through 5, preserving other bits.
    Status,
    /// SR1 BP0/BP1 and SRWD.
    Bp1Srwd,
    /// SR1 BP0 through BP2 and SRWD.
    Bp2Srwd,
    /// SR1 BP0 through BP3 and SRWD.
    Bp3Srwd,
    /// SR1 BP0 through BP4 and SRWD.
    Bp4Srwd,
    /// Atmel AT25DF/DL/DQ and AT26DF individual sector protection.
    At2x,
    /// Atmel AT25F two-bit block protection.
    At25f,
    /// Atmel AT25F512A single-bit block protection.
    At25f512a,
    /// Atmel AT25F512B single-bit block protection and WP# status.
    At25f512b,
    /// Atmel AT25FS010 block protection.
    At25fs010,
    /// Atmel AT25FS040 block protection.
    At25fs040,
    /// Micron N25Q BP bits 2, 3, 4, 6 (bit 5 is TB, not BP).
    N25q,
    /// Intel S33 / Spansion protection with erase/program failure flags.
    Bp2EpSrwd,
    /// SST26 48-bit block-protection register (ULBPR / RBPR / WBPR).
    Sst26_6,
    /// SST26 80-bit block-protection register.
    Sst26_10,
    /// SST26 144-bit block-protection register.
    Sst26_18,
    /// A known protection procedure that is not implemented safely.
    Unsupported,
}

impl Unlock {
    /// SR1 protection indication, register lock, WP# status and preserved bits.
    pub const fn status_masks(self) -> Option<(u8, u8, u8, u8)> {
        Some(match self {
            Self::Status => (0x3c, 0, 0, 0xff),
            Self::Bp1Srwd | Self::At25f => (0x0c, 0x80, 0, 0xff),
            Self::Bp2Srwd | Self::Bp2EpSrwd => (0x1c, 0x80, 0, 0xff),
            Self::Bp3Srwd => (0x3c, 0x80, 0, 0xff),
            Self::Bp4Srwd | Self::At25fs040 => (0x7c, 0x80, 0, 0xff),
            Self::At2x => (0x0c, 0x80, 0x10, 0),
            Self::At25f512a => (0x04, 0x80, 0, 0xff),
            Self::At25f512b => (0x04, 0x80, 0x10, 0xff),
            Self::At25fs010 => (0x6c, 0x80, 0, 0xff),
            Self::N25q => (0x5c, 0x80, 0, 0xff),
            _ => return None,
        })
    }

    /// Documented erase/program failure flags in SR1, not ordinary BP bits.
    pub const fn error_mask(self) -> u8 {
        match self {
            Self::At2x | Self::At25f512b => 0x20,
            Self::Bp2EpSrwd => 0x60,
            _ => 0,
        }
    }
}

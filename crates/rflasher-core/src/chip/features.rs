//! Flash chip feature flags

use bitflags::bitflags;

bitflags! {
    /// Feature flags for flash chips
    ///
    /// These flags describe what capabilities and behaviors a flash chip has.
    ///
    /// Multi-IO read flags are split per JEDEC mode (mirrors flashprog's
    /// `FEATURE_FAST_READ_*` flags) so that programmers can pick the highest
    /// throughput operation actually supported by both ends:
    ///
    /// - `FAST_READ_DOUT` — 1-1-2 dual-output (opcode 0x3B / 0x3C)
    /// - `FAST_READ_DIO`  — 1-2-2 dual-I/O    (opcode 0xBB / 0xBC)
    /// - `FAST_READ_QOUT` — 1-1-4 quad-output (opcode 0x6B / 0x6C)
    /// - `FAST_READ_QIO`  — 1-4-4 quad-I/O    (opcode 0xEB / 0xEC)
    /// - `FAST_READ_QPI4B`— 4-4-4 4-byte read in QPI mode (opcode 0xEC)
    /// - `QPI_35_F5`      — enter/exit QPI with 0x35/0xF5
    /// - `QPI_38_FF`      — enter/exit QPI with 0x38/0xFF
    /// - `SET_READ_PARAMS`— SRP 0xC0 to configure QPI dummy cycles / burst length
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
    #[cfg_attr(feature = "std", serde(transparent))]
    pub struct Features: u32 {
        // Write enable behavior
        /// Use WREN (0x06) before WRSR
        const WRSR_WREN       = 1 << 0;
        /// Use EWSR (0x50) before WRSR (legacy SST)
        const WRSR_EWSR       = 1 << 1;
        /// WRSR writes both SR1 and SR2 with one command
        const WRSR_EXT        = 1 << 2;

        // Read capabilities
        /// Supports Fast Read (0x0B / 0x0C)
        const FAST_READ       = 1 << 3;
        /// Supports Dual Output Fast Read (1-1-2, opcode 0x3B / 0x3C)
        const FAST_READ_DOUT  = 1 << 4;
        /// Supports Dual I/O Fast Read (1-2-2, opcode 0xBB / 0xBC)
        const FAST_READ_DIO   = 1 << 5;

        // 4-byte addressing
        /// Supports 4-byte address mode
        const FOUR_BYTE_ADDR  = 1 << 6;
        /// Can enter 4BA mode with EN4B (0xB7)
        const FOUR_BYTE_ENTER = 1 << 7;
        /// Has native 4BA commands (0x13, 0x12, etc.)
        const FOUR_BYTE_NATIVE = 1 << 8;
        /// Supports extended address register
        const EXT_ADDR_REG    = 1 << 9;

        // Special features
        /// Has OTP (One-Time Programmable) area
        const OTP             = 1 << 10;
        /// Supports Quad Output Fast Read (1-1-4, opcode 0x6B / 0x6C)
        const FAST_READ_QOUT  = 1 << 11;
        /// Has security registers
        const SECURITY_REG    = 1 << 12;
        /// Supports SFDP (Serial Flash Discoverable Parameters)
        const SFDP            = 1 << 13;

        // Write behavior
        /// Byte-granularity writes (can write single bytes)
        const WRITE_BYTE      = 1 << 14;
        /// Supports AAI (Auto Address Increment) word program
        const AAI_WORD        = 1 << 15;
        /// SST26-style per-block protection register (not SR BP bits)
        ///
        /// These chips require WREN + ULBPR (0x98) to globally unlock before
        /// any erase or write can succeed, rather than clearing BP bits in the
        /// status register.  Set for all SST26VF/SST26WF series chips.
        const SST26_BPR       = 1 << 16;

        /// Supports Quad I/O Fast Read (1-4-4, opcode 0xEB / 0xEC)
        const FAST_READ_QIO   = 1 << 17;
        /// Supports the 4BA QPI fast read instruction (0xEC).
        ///
        /// When the chip is in QPI (4-4-4) mode and 4-byte addressing is in
        /// effect, the 0xEC opcode reads with a 4-byte address.
        const FAST_READ_QPI4B = 1 << 18;

        // Status register features
        /// Has status register 2
        const STATUS_REG_2    = 1 << 19;
        /// Has status register 3
        const STATUS_REG_3    = 1 << 20;
        /// QPI entry/exit via 0x35/0xF5 (Winbond, GigaDevice)
        const QPI_35_F5       = 1 << 21;

        // Power management
        /// Supports deep power down
        const DEEP_POWER_DOWN = 1 << 22;

        // Write protection
        /// Top/Bottom protect bit available
        const WP_TB           = 1 << 23;
        /// Sector/Block protect bit available
        const WP_SEC          = 1 << 24;
        /// Complement (CMP) bit available
        const WP_CMP          = 1 << 25;
        /// Has Status Register Lock (SRL) bit
        const WP_SRL          = 1 << 26;
        /// Supports volatile status register writes (EWSR)
        const WP_VOLATILE     = 1 << 27;
        /// Has BP3 (4th block protect bit)
        const WP_BP3          = 1 << 28;
        /// Has Write Protect Selection (WPS) for per-sector mode
        const WP_WPS          = 1 << 29;

        /// QPI entry/exit via 0x38/0xFF (Macronix, ISSI, Spansion)
        const QPI_38_FF       = 1 << 30;
        /// Supports Set Read Parameters (0xC0) for QPI dummy cycles / burst length
        const SET_READ_PARAMS = 1 << 31;

        // ----- Convenience aggregate bundles (mirrors flashprog) ---------

        /// Any dual-IO read capability (FAST_READ + DOUT + DIO)
        const DIO_BUNDLE = Self::FAST_READ.bits()
                         | Self::FAST_READ_DOUT.bits()
                         | Self::FAST_READ_DIO.bits();
        /// Any quad-IO read capability (DIO + QOUT + QIO)
        const QIO_BUNDLE = Self::DIO_BUNDLE.bits()
                         | Self::FAST_READ_QOUT.bits()
                         | Self::FAST_READ_QIO.bits();
        /// All quad-related flags (used to mask off if QE-bit set fails)
        const ANY_QUAD = Self::FAST_READ_QOUT.bits()
                       | Self::FAST_READ_QIO.bits()
                       | Self::FAST_READ_QPI4B.bits()
                       | Self::QPI_35_F5.bits()
                       | Self::QPI_38_FF.bits()
                       | Self::SET_READ_PARAMS.bits();
        /// All QPI entry/exit flags
        const ANY_QPI = Self::QPI_35_F5.bits() | Self::QPI_38_FF.bits();
    }
}

// Note: bitflags types don't derive Default, but `Features::empty()` serves
// the same purpose. We keep the manual impl for ergonomics with #[derive(Default)]
// on structs containing Features.
impl Default for Features {
    fn default() -> Self {
        Features::empty()
    }
}

/// Quad-Enable (QE) bit method
///
/// SPI flash chips that support quad I/O typically gate the IO2/IO3 pins on
/// a non-volatile status register bit (the "QE" bit). The exact bit and the
/// command needed to set it varies by manufacturer. These variants mirror
/// the `QuadEnableRequirement` enum in JESD216 (SFDP BFPT DWORD 15 bits 22:20)
/// and flashprog's `reg_bits.qe` decoded methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub enum QeMethod {
    /// No QE bit; chip is always quad-capable when wired correctly.
    /// Examples: Micron N25Q, Adesto AT45, some non-VCC-locked parts.
    #[default]
    None,
    /// QE is bit 1 of SR2; set via WRSR 0x01 with two data bytes (SR1+SR2).
    /// Examples: Winbond W25Q, GigaDevice GD25, ISSI IS25LP/WP (older).
    Sr2Bit1WriteSr,
    /// QE is bit 1 of SR2; set via dedicated WRSR2 0x31 with one byte.
    /// Examples: many newer Winbond/GigaDevice/Macronix split-status chips.
    Sr2Bit1WriteSr2,
    /// QE is bit 6 of SR1; set via WRSR 0x01.
    /// Examples: Macronix MX25L/MX25U, Cypress.
    Sr1Bit6,
    /// QE is bit 7 of SR2; set via special unlocked write sequence.
    /// Examples: a few legacy Spansion/Cypress parts.
    Sr2Bit7,
}

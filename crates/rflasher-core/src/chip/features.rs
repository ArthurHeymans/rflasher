//! Flash chip feature flags

use bitflags::bitflags;

bitflags! {
    /// Feature flags for flash chips
    ///
    /// These flags describe what capabilities and behaviors a flash chip has.
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
        /// Supports Fast Read (0x0B)
        const FAST_READ       = 1 << 3;
        /// Supports Dual I/O read commands
        const DUAL_IO         = 1 << 4;
        /// Supports Quad I/O read commands
        const QUAD_IO         = 1 << 5;

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
        /// Supports QPI mode (4-4-4)
        const QPI             = 1 << 11;
        /// Has security registers
        const SECURITY_REG    = 1 << 12;
        /// Supports SFDP (Serial Flash Discoverable Parameters)
        const SFDP            = 1 << 13;

        // Write behavior
        /// Byte-granularity writes (can write single bytes)
        const WRITE_BYTE      = 1 << 14;
        /// Supports AAI (Auto Address Increment) word program
        const AAI_WORD        = 1 << 15;

        // Erase behavior
        /// Has 4KB sector erase
        const ERASE_4K        = 1 << 16;
        /// Has 32KB block erase
        const ERASE_32K       = 1 << 17;
        /// Has 64KB block erase
        const ERASE_64K       = 1 << 18;

        // Status register features
        /// Has status register 2
        const STATUS_REG_2    = 1 << 19;
        /// Has status register 3
        const STATUS_REG_3    = 1 << 20;
        /// Quad Enable bit is in SR2
        const QE_SR2          = 1 << 21;

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
    }
}

impl Default for Features {
    fn default() -> Self {
        Features::empty()
    }
}

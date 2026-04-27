//! Flash chip feature flags

use bitflags::bitflags;

bitflags! {
    /// Feature flags for flash chips
    ///
    /// These flags describe what capabilities and behaviors a flash chip has.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
    #[cfg_attr(feature = "std", serde(transparent))]
    pub struct Features: u64 {
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
        /// Has flashprog-style native 4BA commands (0x13 read, 0x0C fast read, 0x12 page program)
        const FOUR_BYTE_NATIVE = 1 << 8;
        /// Supports extended address register (legacy coarse flag)
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
        /// SST26-style per-block protection register (not SR BP bits)
        ///
        /// These chips require WREN + ULBPR (0x98) to globally unlock before
        /// any erase or write can succeed, rather than clearing BP bits in the
        /// status register.  Set for all SST26VF/SST26WF series chips.
        const SST26_BPR       = 1 << 16;

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

        // Detailed 4-byte addressing behavior (flashprog / JESD216 4BA table)
        /// Enter/exit 4BA mode with WREN + 0xB7 / WREN + 0xE9
        const FOUR_BYTE_ENTER_WREN = 1 << 30;
        /// Extended Address Register uses 0xC5/0xC8
        const EXT_ADDR_REG_C5C8    = 1 << 31;
        /// Enter/exit 4BA mode by setting bit 7 of the extended address register
        const FOUR_BYTE_ENTER_EAR7 = 1 << 32;
        /// Extended Address Register uses 0x17/0x16
        const EXT_ADDR_REG_1716    = 1 << 33;
        /// Native 4BA read instruction 0x13
        const FOUR_BYTE_READ       = 1 << 34;
        /// Native 4BA fast-read instruction 0x0C
        const FOUR_BYTE_FAST_READ  = 1 << 35;
        /// Native 4BA page-program instruction 0x12
        const FOUR_BYTE_PROGRAM    = 1 << 36;
        /// Native 4BA dual-output read instruction 0x3C
        const FOUR_BYTE_DUAL_OUT_READ = 1 << 37;
        /// Native 4BA dual-I/O read instruction 0xBC
        const FOUR_BYTE_DUAL_IO_READ  = 1 << 38;
        /// Native 4BA quad-output read instruction 0x6C
        const FOUR_BYTE_QUAD_OUT_READ = 1 << 39;
        /// Native 4BA quad-I/O read instruction 0xEC
        const FOUR_BYTE_QUAD_IO_READ  = 1 << 40;
    }
}

impl Features {
    /// Whether the native 4BA 0x13 read opcode is supported.
    pub fn supports_4ba_read(self) -> bool {
        self.contains(Self::FOUR_BYTE_READ)
    }

    /// Whether the native 4BA 0x0C fast-read opcode is supported.
    pub fn supports_4ba_fast_read(self) -> bool {
        self.contains(Self::FOUR_BYTE_FAST_READ)
    }

    /// Whether the native 4BA 0x12 page-program opcode is supported.
    pub fn supports_4ba_program(self) -> bool {
        self.contains(Self::FOUR_BYTE_PROGRAM)
    }

    /// Whether native 4BA 0x3C dual-output read is supported.
    pub fn supports_4ba_dual_out_read(self) -> bool {
        self.contains(Self::FOUR_BYTE_DUAL_OUT_READ)
    }

    /// Whether native 4BA 0xBC dual-I/O read is supported.
    pub fn supports_4ba_dual_io_read(self) -> bool {
        self.contains(Self::FOUR_BYTE_DUAL_IO_READ)
    }

    /// Whether native 4BA 0x6C quad-output read is supported.
    pub fn supports_4ba_quad_out_read(self) -> bool {
        self.contains(Self::FOUR_BYTE_QUAD_OUT_READ)
    }

    /// Whether native 4BA 0xEC quad-I/O read is supported.
    pub fn supports_4ba_quad_io_read(self) -> bool {
        self.contains(Self::FOUR_BYTE_QUAD_IO_READ)
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

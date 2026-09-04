//! Flash chip feature flags

use bitflags::bitflags;

bitflags! {
    /// Feature flags for flash chips
    ///
    /// These flags describe what capabilities and behaviors a flash chip has.
    ///
    /// Multi-IO read flags are split by JEDEC bus mode so programmers can
    /// select the fastest operation supported by both the chip and controller.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
    #[cfg_attr(feature = "serde", serde(transparent))]
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

        // Fine-grained multi-IO read support
        /// Supports Dual Output Fast Read (1-1-2, opcode 0x3B / 0x3C)
        const FAST_READ_DOUT  = 1 << 41;
        /// Supports Dual I/O Fast Read (1-2-2, opcode 0xBB / 0xBC)
        const FAST_READ_DIO   = 1 << 42;
        /// Supports Quad Output Fast Read (1-1-4, opcode 0x6B / 0x6C)
        const FAST_READ_QOUT  = 1 << 43;
        /// Supports Quad I/O Fast Read (1-4-4, opcode 0xEB / 0xEC)
        const FAST_READ_QIO   = 1 << 44;
        /// Supports 4-byte QPI Fast Read (4-4-4, opcode 0xEC)
        const FAST_READ_QPI4B = 1 << 45;
        /// QPI entry/exit via 0x35/0xF5
        const QPI_35_F5       = 1 << 46;
        /// QPI entry/exit via 0x38/0xFF
        const QPI_38_FF       = 1 << 47;
        /// Supports Set Read Parameters (0xC0)
        const SET_READ_PARAMS = 1 << 48;

        /// Fast-read and dual-read capabilities.
        const DIO_BUNDLE = Self::FAST_READ.bits()
                         | Self::FAST_READ_DOUT.bits()
                         | Self::FAST_READ_DIO.bits();
        /// Fast-read, dual-read, and quad-read capabilities.
        const QIO_BUNDLE = Self::DIO_BUNDLE.bits()
                         | Self::FAST_READ_QOUT.bits()
                         | Self::FAST_READ_QIO.bits();
        /// All capabilities that require quad data lines.
        const ANY_QUAD = Self::FAST_READ_QOUT.bits()
                       | Self::FAST_READ_QIO.bits()
                       | Self::FAST_READ_QPI4B.bits()
                       | Self::QPI_35_F5.bits()
                       | Self::QPI_38_FF.bits()
                       | Self::SET_READ_PARAMS.bits();
        /// All QPI entry/exit mechanisms.
        const ANY_QPI = Self::QPI_35_F5.bits() | Self::QPI_38_FF.bits();
    }
}

impl Features {
    /// Whether any 4-byte mode enter/exit mechanism is supported.
    pub fn supports_4ba_mode_switch(self) -> bool {
        self.intersects(
            Self::FOUR_BYTE_ENTER | Self::FOUR_BYTE_ENTER_WREN | Self::FOUR_BYTE_ENTER_EAR7,
        )
    }

    /// Whether an extended address register can select the high address byte.
    pub fn supports_extended_address_register(self) -> bool {
        self.intersects(Self::EXT_ADDR_REG | Self::EXT_ADDR_REG_C5C8 | Self::EXT_ADDR_REG_1716)
    }

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

/// Method used to enable quad I/O on a flash chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum QeMethod {
    /// The chip has no QE bit.
    #[default]
    None,
    /// QE is SR2 bit 1, written with command 0x01 and two status bytes.
    Sr2Bit1WriteSr,
    /// QE is SR2 bit 1, written with the dedicated 0x31 command.
    Sr2Bit1WriteSr2,
    /// QE is SR1 bit 6, written with command 0x01.
    Sr1Bit6,
    /// QE is SR2 bit 7 and requires the chip-specific unlocked sequence.
    Sr2Bit7,
}

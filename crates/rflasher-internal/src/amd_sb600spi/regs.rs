//! SB600-family SPI controller, LPC bridge, SMBus and ACPI MMIO register
//! definitions.
//!
//! References:
//! - AMD SB800-Series Register Reference Guide (45482 rev 3.04)
//! - AMD SB810/SB850 Databook (44758 rev 2.10)
//! - AMD SP5100 Register Reference Guide (44413 rev 3.03)
//! - flashprog `sb600spi.c` / `chipset_enable.c`

// ---------------------------------------------------------------------------
// SPI controller (SPI_BASE_ADDR MMIO window, 32 bytes)
// ---------------------------------------------------------------------------

/// SPI_Cntrl0: offset 0x00, 32 bits. Byte 0 is SPI_OpCode.
pub(crate) const SPI_CNTRL0: usize = 0x00;
/// SPI_Cntrl0[15:8]: offset 0x01, 8 bits (TxByteCount[3:0], RxByteCount[7:4]).
pub(crate) const SPI_CNTRL0_COUNTS: usize = 0x01;
/// SPI_Cntrl0[23:16]: offset 0x02, 8 bits (ExecuteOpCode, FifoPtrClr, ...).
pub(crate) const SPI_CNTRL0_CMD: usize = 0x02;
/// SPI_RestrictedCmd: offset 0x04, 32 bits (RestrictedCmd0..3).
pub(crate) const SPI_RESTRICTED_CMD: usize = 0x04;
/// SPI_RestrictedCmd2: offset 0x08, 32 bits (RestrictedCmd4, RestrictedCmdWoAddr0..2).
pub(crate) const SPI_RESTRICTED_CMD2: usize = 0x08;
/// SPI_Cntrl1: offset 0x0C, 32 bits.
pub(crate) const SPI_CNTRL1: usize = 0x0c;
/// SPI_Cntrl1[7:0] SPIParameters: offset 0x0C, 8 bits (TX/RX FIFO port).
pub(crate) const SPI_FIFO: usize = 0x0c;
/// SPI_Cntrl1[15:8]: offset 0x0D, 8 bits (FifoPtr, TrackMacLockEn, NormSpeed, FastSpeed).
pub(crate) const SPI_CNTRL1_SPEED: usize = 0x0d;
/// Alt_SPI_CS: offset 0x1D, 8 bits (SB8xx+).
pub(crate) const ALT_SPI_CS: usize = 0x1d;
/// Size of the SB6xx-SB9xx SPI register window.
pub(crate) const SPI_WINDOW: usize = 0x20;

// SPI_Cntrl0 (32-bit) bits
pub(crate) const CNTRL0_EXECUTE_OPCODE: u32 = 1 << 16;
pub(crate) const CNTRL0_FAST_READ_ENABLE: u32 = 1 << 18;
pub(crate) const CNTRL0_SPI_ARB_ENABLE: u32 = 1 << 19;
pub(crate) const CNTRL0_SPI_ACCESS_MAC_ROM_EN: u32 = 1 << 22;
pub(crate) const CNTRL0_SPI_HOST_ACCESS_ROM_EN: u32 = 1 << 23;
pub(crate) const CNTRL0_ARB_WAIT_COUNT_SHIFT: u32 = 24;
pub(crate) const CNTRL0_ARB_WAIT_COUNT_MASK: u32 = 0x7 << CNTRL0_ARB_WAIT_COUNT_SHIFT;
pub(crate) const CNTRL0_SPI_BRIDGE_DISABLE: u32 = 1 << 27;
pub(crate) const CNTRL0_SPI_CLK_GATE: u32 = 1 << 28;
pub(crate) const CNTRL0_SPI_BUSY: u32 = 1 << 31;

// SPI_Cntrl0[23:16] (byte offset 0x02) bits
pub(crate) const CMD_EXECUTE_OPCODE: u8 = 1 << 0;
pub(crate) const CMD_FAST_READ_ENABLE: u8 = 1 << 2;
pub(crate) const CMD_FIFO_PTR_CLR: u8 = 1 << 4;
pub(crate) const CMD_FIFO_PTR_INC: u8 = 1 << 5;
/// Write-1 trigger bits in SPI_Cntrl0[23:16] that must never be written back on RMW.
pub(crate) const CMD_TRIGGERS: u8 = CMD_EXECUTE_OPCODE | CMD_FIFO_PTR_CLR | CMD_FIFO_PTR_INC;

// SPI_Cntrl1[15:8] (byte offset 0x0D) bits
pub(crate) const SPEED_FIFO_PTR_MASK: u8 = 0x07;
pub(crate) const SPEED_NORM_SHIFT: u8 = 4;
pub(crate) const SPEED_NORM_MASK: u8 = 0x03 << SPEED_NORM_SHIFT;
pub(crate) const SPEED_FAST_MASK: u8 = 0x03 << 6;

// ---------------------------------------------------------------------------
// LPC bridge (D14F3) PCI configuration space
// ---------------------------------------------------------------------------

pub(crate) const LPC_ID: u16 = 0x00;
pub(crate) const LPC_PCI_CONTROL: u16 = 0x40;
/// Rom Protect 0..3: offsets 0x50..=0x5C (write-once after reset on SB8xx).
pub(crate) const LPC_ROM_PROTECT0: u16 = 0x50;
pub(crate) const LPC_SPI_BASE_ADDR: u16 = 0xa0;
pub(crate) const LPC_HOST_CONTROL: u16 = 0xbb;

pub(crate) const PCI_CONTROL_IMC_PRESENT: u8 = 1 << 7;
pub(crate) const HOST_CONTROL_PREFETCH_EN: u8 = 1 << 0;
pub(crate) const HOST_CONTROL_SPI_OP_IN_LPC: u8 = 1 << 5;

pub(crate) const SPI_BASE_ROM_ENABLE: u32 = 1 << 1;
pub(crate) const SPI_BASE_ADDR_MASK: u32 = 0xffff_ffe0;

pub(crate) const ROM_PROTECT_WRITE: u32 = 1 << 0;
pub(crate) const ROM_PROTECT_READ: u32 = 1 << 1;
pub(crate) const ROM_PROTECT_OFFSET_SHIFT: u32 = 2;
pub(crate) const ROM_PROTECT_OFFSET_MASK: u32 = 0x1ff;
pub(crate) const ROM_PROTECT_BASE_MASK: u32 = 0xffff_f800;

// ---------------------------------------------------------------------------
// SMBus (D14F0) PCI configuration space
// ---------------------------------------------------------------------------

pub(crate) const SMBUS_ID: u16 = 0x00;
pub(crate) const SMBUS_REVISION_ID: u16 = 0x08;
pub(crate) const SMBUS_GPIO_32_31: u16 = 0x83;
pub(crate) const SMBUS_GPIO_47: u16 = 0xa7;
pub(crate) const SMBUS_GPIO_12_11: u16 = 0xab;

/// GPIO_Enable bits (set = pin is GPIO, clear = pin is SPI).
pub(crate) const SMBUS_GPIO31_SPI_HOLD: u8 = 1 << 6;
pub(crate) const SMBUS_GPIO32_SPI_CS: u8 = 1 << 7;
pub(crate) const SMBUS_GPIO47_SPI_CLK: u8 = 1 << 6;
pub(crate) const SMBUS_GPIO11_SPI_DO: u8 = 1 << 6;
pub(crate) const SMBUS_GPIO12_SPI_DI: u8 = 1 << 7;

// ---------------------------------------------------------------------------
// SB8xx ACPI MMIO (AcpiMMioAddr, PM_Reg 24h)
// ---------------------------------------------------------------------------

/// Documented default AcpiMMioAddr (45482 p. 220).
pub const SB800_ACPI_MMIO_DEFAULT: u64 = 0xfed8_0000;
pub(crate) const ACPI_MMIO_SIZE: usize = 0x1000;

const ACPI_PMIO: usize = 0x300;
const ACPI_IOMUX: usize = 0xd00;
const ACPI_MISC: usize = 0xe00;

pub(crate) const PM_ACPI_MMIO_EN: usize = ACPI_PMIO + 0x24;
pub(crate) const PM_MISC: usize = ACPI_PMIO + 0xc8;
pub(crate) const MISC_STRAP_STATUS: usize = ACPI_MISC + 0x80;
pub(crate) const MISC_STRAP_OVERRIDE: usize = ACPI_MISC + 0x84;

/// IoMux offsets of the SB8xx SPI pins (function 0 = SPI).
pub(crate) const IOMUX_SPI_PINS: [(usize, &str); 4] = [
    (ACPI_IOMUX + 162, "SPI_CLK/GPIO162"),
    (ACPI_IOMUX + 163, "SPI_DO/GPIO163"),
    (ACPI_IOMUX + 164, "SPI_DI/GPIO164"),
    (ACPI_IOMUX + 165, "SPI_CS#/GPIO165"),
];

pub(crate) const ACPI_MMIO_DECODE_EN: u32 = 1 << 0;
pub(crate) const ACPI_MMIO_IO_SEL: u32 = 1 << 1;
pub(crate) const ACPI_MMIO_ADDR_MASK: u32 = 0xffff_f000;

pub(crate) const PM_MISC_BYPASS_ROM_SEL_SHIFT: u32 = 16;
pub(crate) const PM_MISC_BYPASS_ROM_SEL_MASK: u32 = 0x3 << PM_MISC_BYPASS_ROM_SEL_SHIFT;
pub(crate) const PM_MISC_USE_BYPASS_ROM: u32 = 1 << 18;

/// StrapStatus[1:0] = 0b11 means FWHDisableStrap=1 and UseLpcRomStrap=1 (SPI ROM).
pub(crate) const STRAP_ROM_SPI_MASK: u32 = 0x3;
pub(crate) const STRAP_IMC_ENABLE: u32 = 1 << 2;
pub(crate) const STRAP_OVERRIDE_EN: u32 = 1 << 31;

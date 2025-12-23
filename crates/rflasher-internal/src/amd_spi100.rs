//! AMD SPI100 Controller Driver
//!
//! This module implements the SPI controller driver for AMD chipsets with SPI100 controller.
//! The SPI100 controller is found in AMD Ryzen and newer platforms.
//!
//! # Supported Chipsets
//!
//! - AMD Renoir/Cezanne (FCH 790b rev 0x51)
//! - AMD Pinnacle Ridge (FCH 790b rev 0x59)
//! - AMD Raven Ridge/Matisse/Starship (FCH 790b rev 0x61)
//! - AMD Raphael/Mendocino/Phoenix/Rembrandt (FCH 790b rev 0x71)
//!
//! # Architecture
//!
//! The AMD SPI100 controller has:
//! - A 71-byte FIFO for SPI commands and data
//! - Memory-mapped flash access via ROM range registers
//! - Support for various SPI modes (normal, dual I/O, quad I/O, fast read)
//! - Configurable clock speeds
//!
//! # References
//!
//! - flashprog/amd_spi100.c - Original C implementation

use crate::controller::Controller;
use crate::error::InternalError;
use crate::physmap::PhysMap;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{AddressWidth, SpiCommand};

/// SPI100 FIFO size (in bytes)
const SPI100_FIFO_SIZE: usize = 71;

/// Maximum data transfer for read operations
/// Account for up to 4 address bytes
pub const SPI100_MAX_DATA_READ: usize = SPI100_FIFO_SIZE - 4;

/// Maximum data transfer for write operations
/// Account for up to 4 address bytes
pub const SPI100_MAX_DATA_WRITE: usize = SPI100_FIFO_SIZE - 4;

/// Register offsets for SPI100 controller
mod regs {
    /// SPI Control Register 0
    pub const SPI_CNTRL0: usize = 0x00;

    /// SPI Status Register
    pub const SPI_STATUS: usize = 0x4c;

    /// Command Register
    pub const CMD_CODE: usize = 0x45;

    /// Transmit Byte Count
    pub const CMD_TRIGGER: usize = 0x47;

    /// Transmit Byte Count
    pub const TX_BYTE_COUNT: usize = 0x48;

    /// Receive Byte Count
    pub const RX_BYTE_COUNT: usize = 0x4b;

    /// FIFO base address
    pub const FIFO_BASE: usize = 0x80;

    /// Alternate SPI CS
    pub const ALT_SPI_CS: usize = 0x1d;

    /// Speed configuration
    pub const SPEED_CFG: usize = 0x22;

    /// ROM2 address override
    pub const ROM2_ADDR_OVERRIDE: usize = 0x30;

    /// 32-bit address control 0
    pub const ADDR32_CTRL0: usize = 0x50;

    /// 32-bit address control 3
    pub const ADDR32_CTRL3: usize = 0x5c;
}

/// SPI Control 0 register bits
#[allow(dead_code)]
mod spi_cntrl0_bits {
    /// SPI Arbitration Enable (bit 19)
    pub const SPI_ARB_ENABLE: u32 = 1 << 19;

    /// Illegal Access (bit 21)
    pub const ILLEGAL_ACCESS: u32 = 1 << 21;

    /// SPI Access MAC ROM Enable (bit 22)
    pub const SPI_ACCESS_MAC_ROM_EN: u32 = 1 << 22;

    /// SPI Host Access ROM Enable (bit 23)
    pub const SPI_HOST_ACCESS_ROM_EN: u32 = 1 << 23;

    /// SPI Busy (bit 31)
    pub const SPI_BUSY: u32 = 1 << 31;
}

/// SPI Status register bits
mod spi_status_bits {
    /// Transfer complete (bit 31 cleared)
    pub const BUSY: u32 = 1 << 31;
}

/// Trigger bits for command execution
mod cmd_trigger_bits {
    /// Execute SPI command (bit 7)
    pub const EXECUTE: u8 = 1 << 7;
}

/// SPI read modes
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
enum SpiReadMode {
    NormalReadSlow = 0,
    Reserved1 = 1,
    DualIo112 = 2,
    QuadIo114 = 3,
    DualIo122 = 4,
    QuadIo114Alt = 5,
    NormalReadFast = 6,
    FastRead = 7,
}

impl SpiReadMode {
    fn name(&self) -> &'static str {
        match self {
            Self::NormalReadSlow => "Normal read (up to 33MHz)",
            Self::Reserved1 => "Reserved",
            Self::DualIo112 => "Dual IO (1-1-2)",
            Self::QuadIo114 => "Quad IO (1-1-4)",
            Self::DualIo122 => "Dual IO (1-2-2)",
            Self::QuadIo114Alt => "Quad IO (1-1-4)",
            Self::NormalReadFast => "Normal read (up to 66MHz)",
            Self::FastRead => "Fast Read",
        }
    }
}

/// SPI clock speeds
#[derive(Debug, Clone, Copy)]
struct SpiSpeed {
    khz: u32,
    name: &'static str,
}

const SPI_SPEEDS: [SpiSpeed; 8] = [
    SpiSpeed {
        khz: 66666,
        name: "66.66 MHz",
    },
    SpiSpeed {
        khz: 33333,
        name: "33.33 MHz",
    },
    SpiSpeed {
        khz: 22222,
        name: "22.22 MHz",
    },
    SpiSpeed {
        khz: 16666,
        name: "16.66 MHz",
    },
    SpiSpeed {
        khz: 100000,
        name: "100 MHz",
    },
    SpiSpeed {
        khz: 800,
        name: "800 kHz",
    },
    SpiSpeed {
        khz: 0,
        name: "Reserved",
    },
    SpiSpeed {
        khz: 0,
        name: "Reserved",
    },
];

/// AMD SPI100 Controller
#[cfg(all(feature = "std", target_os = "linux"))]
pub struct Spi100Controller {
    /// Memory-mapped SPI registers
    spibar: PhysMap,
    /// Memory-mapped flash region (optional)
    memory: Option<PhysMap>,
    /// Size of memory-mapped region
    mapped_len: usize,
    /// Whether 4-byte addressing memory map is disabled
    no_4ba_mmap: bool,
    /// Original alternate speed (for restoration on shutdown)
    altspeed: u8,
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl Spi100Controller {
    /// Create a new SPI100 controller instance
    ///
    /// # Arguments
    ///
    /// * `spibar_addr` - Physical address of SPI BAR
    /// * `memory_addr` - Optional physical address of memory-mapped flash
    /// * `mapped_len` - Size of memory-mapped flash region (0 if none)
    pub fn new(
        spibar_addr: u64,
        memory_addr: Option<u64>,
        mapped_len: usize,
    ) -> Result<Self, InternalError> {
        // Map the SPI registers (256 bytes)
        let spibar = PhysMap::new(spibar_addr, 256)?;

        // Map the memory region if provided
        let memory = if let Some(addr) = memory_addr {
            if mapped_len > 0 {
                Some(PhysMap::new(addr, mapped_len)?)
            } else {
                None
            }
        } else {
            None
        };

        let mut controller = Self {
            spibar,
            memory,
            mapped_len,
            no_4ba_mmap: false,
            altspeed: 0,
        };

        // Initialize the controller
        controller.init()?;

        Ok(controller)
    }

    /// Initialize the SPI100 controller
    fn init(&mut self) -> Result<(), InternalError> {
        // Print controller configuration
        self.print_config();

        // Set alternate speed for programming
        self.set_altspeed();

        // Check 4-byte addressing configuration
        self.check_4ba();

        Ok(())
    }

    /// Read an 8-bit value from SPI register
    #[inline(always)]
    fn read8(&self, reg: usize) -> u8 {
        self.spibar.read8(reg)
    }

    /// Read a 16-bit value from SPI register
    #[inline(always)]
    fn read16(&self, reg: usize) -> u16 {
        self.spibar.read16(reg)
    }

    /// Read a 32-bit value from SPI register
    #[inline(always)]
    fn read32(&self, reg: usize) -> u32 {
        self.spibar.read32(reg)
    }

    /// Write an 8-bit value to SPI register
    #[inline(always)]
    fn write8(&self, reg: usize, val: u8) {
        self.spibar.write8(reg, val);
    }

    /// Write a 16-bit value to SPI register
    #[inline(always)]
    fn write16(&self, reg: usize, val: u16) {
        self.spibar.write16(reg, val);
    }

    /// Write multiple bytes to SPI register (FIFO)
    fn writen(&self, reg: usize, data: &[u8]) {
        for (i, &byte) in data.iter().enumerate() {
            self.write8(reg + i, byte);
        }
    }

    /// Read multiple bytes from SPI register (FIFO)
    /// Uses aligned 32-bit reads for efficiency
    fn readn(&self, reg: usize, data: &mut [u8]) {
        let len = data.len();
        let mut offset = 0;

        // Read full 32-bit words
        while offset + 4 <= len {
            let val = self.read32(reg + offset);
            data[offset] = val as u8;
            data[offset + 1] = (val >> 8) as u8;
            data[offset + 2] = (val >> 16) as u8;
            data[offset + 3] = (val >> 24) as u8;
            offset += 4;
        }

        // Read remaining bytes
        while offset < len {
            data[offset] = self.read8(reg + offset);
            offset += 1;
        }
    }

    /// Check read/write byte counts
    fn check_readwritecnt(&self, writecnt: usize, readcnt: usize) -> Result<(), InternalError> {
        if writecnt < 1 {
            return Err(InternalError::Io(
                "SPI controller needs to send at least 1 byte",
            ));
        }

        if writecnt - 1 > SPI100_FIFO_SIZE {
            return Err(InternalError::Io(
                "SPI controller can not send that many bytes",
            ));
        }

        let maxreadcnt = SPI100_FIFO_SIZE - (writecnt - 1);
        if readcnt > maxreadcnt {
            return Err(InternalError::Io(
                "SPI controller can not receive that many bytes for this command",
            ));
        }

        Ok(())
    }

    /// Send a SPI command
    pub fn send_command(&self, writearr: &[u8], readarr: &mut [u8]) -> Result<(), InternalError> {
        let writecnt = writearr.len();
        let readcnt = readarr.len();

        self.check_readwritecnt(writecnt, readcnt)?;

        // First "command" byte is sent separately
        self.write8(regs::CMD_CODE, writearr[0]);
        self.write8(regs::TX_BYTE_COUNT, (writecnt - 1) as u8);
        self.write8(regs::RX_BYTE_COUNT, readcnt as u8);

        // Write remaining bytes to FIFO
        if writecnt > 1 {
            self.writen(regs::FIFO_BASE, &writearr[1..]);
        }

        // Check if the command/address is allowed
        let spi_cntrl0 = self.read32(regs::SPI_CNTRL0);
        if spi_cntrl0 & spi_cntrl0_bits::ILLEGAL_ACCESS != 0 {
            log::error!("Illegal access for opcode {:#04x}", writearr[0]);
            return Err(InternalError::Io("Illegal SPI command"));
        } else {
            log::trace!("Executing opcode {:#04x}", writearr[0]);
        }

        // Trigger command
        self.write8(regs::CMD_TRIGGER, cmd_trigger_bits::EXECUTE);

        // Wait for completion (10 second timeout)
        let timeout_us = 10_000_000;
        let mut elapsed_us = 0;

        loop {
            let spistatus = self.read32(regs::SPI_STATUS);
            if spistatus & spi_status_bits::BUSY == 0 {
                break;
            }

            if elapsed_us >= timeout_us {
                log::error!("SPI transfer timed out (status: {:#010x})", spistatus);
                return Err(InternalError::Io("SPI transfer timeout"));
            }

            // Delay 1 microsecond
            std::thread::sleep(std::time::Duration::from_micros(1));
            elapsed_us += 1;
        }

        log::trace!("SPI command completed");

        // Read response data from FIFO
        if readcnt > 0 {
            self.readn(regs::FIFO_BASE + writecnt - 1, readarr);
        }

        Ok(())
    }

    /// Read from memory-mapped flash
    fn mmap_read(&self, start: usize, dst: &mut [u8]) -> Result<(), InternalError> {
        let memory = self
            .memory
            .as_ref()
            .ok_or_else(|| InternalError::Io("No memory mapping available"))?;

        // Use aligned 64-bit reads for efficiency
        let len = dst.len();
        let mut offset = 0;

        while offset + 8 <= len {
            let addr = start + offset;
            // Read as two 32-bit values (PhysMap doesn't have read64)
            let lo = memory.read32(addr);
            let hi = memory.read32(addr + 4);

            dst[offset] = lo as u8;
            dst[offset + 1] = (lo >> 8) as u8;
            dst[offset + 2] = (lo >> 16) as u8;
            dst[offset + 3] = (lo >> 24) as u8;
            dst[offset + 4] = hi as u8;
            dst[offset + 5] = (hi >> 8) as u8;
            dst[offset + 6] = (hi >> 16) as u8;
            dst[offset + 7] = (hi >> 24) as u8;
            offset += 8;
        }

        // Read remaining bytes
        while offset < len {
            dst[offset] = memory.read8(start + offset);
            offset += 1;
        }

        Ok(())
    }

    /// Read data from flash
    ///
    /// This uses memory-mapped access when available and falls back to
    /// SPI commands for data outside the mapped range.
    pub fn read(&self, chip_size: u64, start: u32, buf: &mut [u8]) -> Result<(), InternalError> {
        let len = buf.len();

        // Don't consider memory mapping at all if 4BA chips are not mapped as expected
        if chip_size > 16 * 1024 * 1024 && self.no_4ba_mmap {
            return self.default_spi_read(start, buf);
        }

        // Where in the flash does the memory mapped part start?
        // Can be negative if the mapping is bigger than the chip.
        let mapped_start = chip_size as i64 - self.mapped_len as i64;

        let mut offset = 0;
        let mut current_start = start;

        // Use SPI100 engine for data outside the memory-mapped range
        if (current_start as i64) < mapped_start {
            let unmapped_len = len.min((mapped_start - current_start as i64) as usize);
            self.default_spi_read(current_start, &mut buf[offset..offset + unmapped_len])?;
            current_start += unmapped_len as u32;
            offset += unmapped_len;
        }

        // Use memory-mapped access for the rest
        if offset < len {
            let mmap_offset = (current_start as i64 - mapped_start) as usize;
            self.mmap_read(mmap_offset, &mut buf[offset..])?;
        }

        Ok(())
    }

    /// Default SPI read using command interface
    fn default_spi_read(&self, start: u32, buf: &mut [u8]) -> Result<(), InternalError> {
        // Use standard SPI READ command (0x03)
        let len = buf.len();
        let mut offset = 0;

        while offset < len {
            let chunk_len = (len - offset).min(SPI100_MAX_DATA_READ);
            let addr = start + offset as u32;

            let mut writearr = [0u8; 4];
            writearr[0] = 0x03; // READ command
            writearr[1] = (addr >> 16) as u8;
            writearr[2] = (addr >> 8) as u8;
            writearr[3] = addr as u8;

            self.send_command(&writearr, &mut buf[offset..offset + chunk_len])?;
            offset += chunk_len;
        }

        Ok(())
    }

    /// Print controller configuration
    fn print_config(&self) {
        let spi_cntrl0 = self.read32(regs::SPI_CNTRL0);

        log::debug!(
            "SPI_CNTRL0: {:#010x} SpiArbEnable={} IllegalAccess={} \
             SpiAccessMacRomEn={} SpiHostAccessRomEn={}",
            spi_cntrl0,
            (spi_cntrl0 >> 19) & 1,
            (spi_cntrl0 >> 21) & 1,
            (spi_cntrl0 >> 22) & 1,
            (spi_cntrl0 >> 23) & 1,
        );

        log::debug!(
            "  ArbWaitCount={} SpiBridgeDisable={} SpiClkGate={}",
            (spi_cntrl0 >> 24) & 7,
            (spi_cntrl0 >> 27) & 1,
            (spi_cntrl0 >> 28) & 1,
        );

        let read_mode_idx = ((spi_cntrl0 >> 28) & 6) | ((spi_cntrl0 >> 18) & 1);
        let read_mode = match read_mode_idx {
            0 => SpiReadMode::NormalReadSlow,
            1 => SpiReadMode::Reserved1,
            2 => SpiReadMode::DualIo112,
            3 => SpiReadMode::QuadIo114,
            4 => SpiReadMode::DualIo122,
            5 => SpiReadMode::QuadIo114Alt,
            6 => SpiReadMode::NormalReadFast,
            7 => SpiReadMode::FastRead,
            _ => SpiReadMode::Reserved1,
        };

        log::debug!(
            "  SpiReadMode={} SpiBusy={}",
            read_mode.name(),
            (spi_cntrl0 >> 31) & 1,
        );

        let alt_spi_cs = self.read8(regs::ALT_SPI_CS);
        log::debug!("Using SPI_CS{}", alt_spi_cs & 0x3);

        let speed_cfg = self.read16(regs::SPEED_CFG);
        log::debug!(
            "NormSpeed: {}",
            SPI_SPEEDS[(speed_cfg >> 12 & 0xf) as usize].name
        );
        log::debug!(
            "FastSpeed: {}",
            SPI_SPEEDS[(speed_cfg >> 8 & 0xf) as usize].name
        );
        log::debug!(
            "AltSpeed:  {}",
            SPI_SPEEDS[(speed_cfg >> 4 & 0xf) as usize].name
        );
        log::debug!(
            "TpmSpeed:  {}",
            SPI_SPEEDS[(speed_cfg >> 0 & 0xf) as usize].name
        );
    }

    /// Check 4-byte addressing configuration
    fn check_4ba(&mut self) {
        let rom2_addr_override = self.read16(regs::ROM2_ADDR_OVERRIDE);
        let addr32_ctrl0 = self.read32(regs::ADDR32_CTRL0);
        let addr32_ctrl3 = self.read32(regs::ADDR32_CTRL3);

        self.no_4ba_mmap = false;

        // Most bits are undocumented ("reserved"), so we play safe
        if rom2_addr_override != 0x14c0 {
            log::debug!("ROM2 address override *not* in default configuration");
            self.no_4ba_mmap = true;
        }

        // Check if the controller would use 4-byte addresses by itself
        if addr32_ctrl0 & 1 != 0 {
            log::debug!("Memory-mapped access uses 32-bit addresses");
        } else {
            log::debug!("Memory-mapped access uses 24-bit addresses");
            self.no_4ba_mmap = true;
        }

        // Another override (xor'ed) for the most-significant address bits
        if addr32_ctrl3 & 0xff != 0 {
            log::debug!("SPI ROM page bits set: {:#04x}", addr32_ctrl3 & 0xff);
            self.no_4ba_mmap = true;
        }
    }

    /// Set alternate speed for programming
    fn set_altspeed(&mut self) {
        let speed_cfg = self.read16(regs::SPEED_CFG);
        let normspeed = (speed_cfg >> 12 & 0xf) as usize;
        self.altspeed = (speed_cfg >> 4 & 0xf) as u8;

        // Set SPI speed to 33MHz but not higher than `normal read` speed
        let altspeed = if SPI_SPEEDS[normspeed].khz != 0 && SPI_SPEEDS[normspeed].khz < 33333 {
            normspeed as u8
        } else {
            1 // 33.33 MHz
        };

        if altspeed != self.altspeed {
            log::info!(
                "Setting SPI speed to {}",
                SPI_SPEEDS[altspeed as usize].name
            );
            let new_speed_cfg = (speed_cfg & !0xf0) | ((altspeed as u16) << 4);
            self.write16(regs::SPEED_CFG, new_speed_cfg);
        }
    }

    /// Restore original alternate speed
    fn restore_altspeed(&self) {
        let speed_cfg = self.read16(regs::SPEED_CFG);
        let new_speed_cfg = (speed_cfg & !0xf0) | ((self.altspeed as u16) << 4);
        self.write16(regs::SPEED_CFG, new_speed_cfg);
    }

    /// Get maximum data read size
    pub fn max_data_read(&self) -> usize {
        SPI100_MAX_DATA_READ
    }

    /// Get maximum data write size
    pub fn max_data_write(&self) -> usize {
        SPI100_MAX_DATA_WRITE
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl Drop for Spi100Controller {
    fn drop(&mut self) {
        // Restore original alternate speed
        self.restore_altspeed();
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl Controller for Spi100Controller {
    fn is_locked(&self) -> bool {
        // AMD SPI100 doesn't have a lock bit like Intel
        false
    }

    fn writes_enabled(&self) -> bool {
        // AMD SPI100 doesn't need BIOS_CNTL write enable - it's always writable
        true
    }

    fn enable_bios_write(&mut self) -> Result<(), InternalError> {
        // AMD doesn't need explicit write enable
        Ok(())
    }

    fn controller_read(&mut self, addr: u32, buf: &mut [u8], chip_size: usize) -> CoreResult<()> {
        // AMD read needs chip_size parameter, use provided chip_size or default
        let chip_size_u64 = if chip_size > 0 {
            chip_size as u64
        } else {
            16 * 1024 * 1024 // Default to 16MB if not yet probed
        };
        self.read(chip_size_u64, addr, buf).map_err(|e| match e {
            InternalError::NoChipset
            | InternalError::UnsupportedChipset { .. }
            | InternalError::MultipleChipsets => CoreError::ProgrammerNotReady,
            InternalError::PciAccess(_) | InternalError::MemoryMap { .. } => {
                CoreError::ProgrammerError
            }
            InternalError::AccessDenied { .. } => CoreError::RegionProtected,
            InternalError::Io(_) => CoreError::IoError,
            InternalError::ChipsetEnable(_) | InternalError::SpiInit(_) => {
                CoreError::ProgrammerError
            }
            InternalError::InvalidDescriptor => CoreError::ProgrammerError,
            InternalError::NotSupported(_) => CoreError::OpcodeNotSupported,
        })
    }

    fn controller_write(&mut self, _addr: u32, _data: &[u8]) -> CoreResult<()> {
        // AMD SPI100 uses SpiMaster for write operations, not OpaqueMaster
        // Caller should use SpiMaster::execute() with appropriate SPI commands
        log::warn!("OpaqueMaster::write() not supported for AMD SPI100 - use SpiMaster instead");
        Err(CoreError::OpcodeNotSupported)
    }

    fn controller_erase(&mut self, _addr: u32, _len: u32) -> CoreResult<()> {
        // AMD SPI100 uses SpiMaster for erase operations, not OpaqueMaster
        // Caller should use SpiMaster::execute() with appropriate SPI commands
        log::warn!("OpaqueMaster::erase() not supported for AMD SPI100 - use SpiMaster instead");
        Err(CoreError::OpcodeNotSupported)
    }

    fn controller_name(&self) -> &'static str {
        "AMD SPI100"
    }

    fn features(&self) -> SpiFeatures {
        <Self as SpiMaster>::features(self)
    }

    fn max_read_len(&self) -> usize {
        <Self as SpiMaster>::max_read_len(self)
    }

    fn max_write_len(&self) -> usize {
        <Self as SpiMaster>::max_write_len(self)
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        <Self as SpiMaster>::execute(self, cmd)
    }

    fn probe_opcode(&self, opcode: u8) -> bool {
        <Self as SpiMaster>::probe_opcode(self, opcode)
    }
}

// Non-Linux stub
#[cfg(not(all(feature = "std", target_os = "linux")))]
pub struct Spi100Controller {
    _private: (),
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
impl Spi100Controller {
    pub fn new(
        _spibar_addr: u64,
        _memory_addr: Option<u64>,
        _mapped_len: usize,
    ) -> Result<Self, InternalError> {
        Err(InternalError::NotSupported(
            "AMD SPI100 controller only supported on Linux",
        ))
    }

    pub fn send_command(&self, _writearr: &[u8], _readarr: &mut [u8]) -> Result<(), InternalError> {
        Err(InternalError::NotSupported(
            "AMD SPI100 controller only supported on Linux",
        ))
    }

    pub fn read(&self, _chip_size: u64, _start: u32, _buf: &mut [u8]) -> Result<(), InternalError> {
        Err(InternalError::NotSupported(
            "AMD SPI100 controller only supported on Linux",
        ))
    }

    pub fn max_data_read(&self) -> usize {
        0
    }

    pub fn max_data_write(&self) -> usize {
        0
    }
}

// =============================================================================
// SpiMaster trait implementation for AMD SPI100
// =============================================================================

#[cfg(all(feature = "std", target_os = "linux"))]
impl SpiMaster for Spi100Controller {
    fn features(&self) -> SpiFeatures {
        // AMD SPI100 supports standard SPI only (no dual/quad modes exposed to software)
        SpiFeatures::empty()
    }

    fn max_read_len(&self) -> usize {
        SPI100_MAX_DATA_READ
    }

    fn max_write_len(&self) -> usize {
        SPI100_MAX_DATA_WRITE
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Build the write array: opcode + address + write data
        let mut writearr = [0u8; SPI100_FIFO_SIZE + 1];
        let mut write_len = 1;

        // Opcode
        writearr[0] = cmd.opcode;

        // Add address if present
        match cmd.address_width {
            AddressWidth::None => {}
            AddressWidth::ThreeByte => {
                let addr = cmd.address.unwrap_or(0);
                writearr[1] = (addr >> 16) as u8;
                writearr[2] = (addr >> 8) as u8;
                writearr[3] = addr as u8;
                write_len += 3;
            }
            AddressWidth::FourByte => {
                let addr = cmd.address.unwrap_or(0);
                writearr[1] = (addr >> 24) as u8;
                writearr[2] = (addr >> 16) as u8;
                writearr[3] = (addr >> 8) as u8;
                writearr[4] = addr as u8;
                write_len += 4;
            }
        }

        // Add write data if present
        let write_data = cmd.write_data;
        if !write_data.is_empty() {
            let data_len = write_data.len().min(SPI100_FIFO_SIZE - write_len + 1);
            writearr[write_len..write_len + data_len].copy_from_slice(&write_data[..data_len]);
            write_len += data_len;
        }

        // Dummy cycles are not directly supported by SPI100 FIFO interface
        // The controller handles this internally for known commands
        if cmd.dummy_cycles > 0 {
            log::debug!(
                "Dummy cycles ({}) will be handled by controller",
                cmd.dummy_cycles
            );
        }

        // Execute the command
        self.send_command(&writearr[..write_len], cmd.read_buf)
            .map_err(map_amd_error)?;

        Ok(())
    }

    fn probe_opcode(&self, _opcode: u8) -> bool {
        // AMD SPI100 doesn't have opcode restrictions like Intel
        true
    }

    fn delay_us(&mut self, us: u32) {
        std::thread::sleep(std::time::Duration::from_micros(us as u64));
    }
}

/// Convert AMD internal error to core error
fn map_amd_error(e: InternalError) -> CoreError {
    match e {
        InternalError::NoChipset
        | InternalError::UnsupportedChipset { .. }
        | InternalError::MultipleChipsets => CoreError::ProgrammerNotReady,
        InternalError::PciAccess(_) | InternalError::MemoryMap { .. } => CoreError::ProgrammerError,
        InternalError::AccessDenied { .. } => CoreError::RegionProtected,
        InternalError::Io(_) => CoreError::IoError,
        InternalError::ChipsetEnable(_) | InternalError::SpiInit(_) => CoreError::ProgrammerError,
        InternalError::InvalidDescriptor => CoreError::ProgrammerError,
        InternalError::NotSupported(_) => CoreError::OpcodeNotSupported,
    }
}

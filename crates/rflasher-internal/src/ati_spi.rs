//! ATI/AMD Radeon GPU SPI flash programmer
//!
//! Accesses the SPI flash chip on discrete AMD/ATI GPUs via the GPU's
//! built-in ROM controller, accessed through PCI MMIO registers.
//! This allows reading/writing the VBIOS flash.
//!
//! Ported from flashrom `ati_spi.c` by Luc Verhaegen and Jiajie Chen.
//!
//! # Supported GPU families
//!
//! - **R600** (HD 2xxx–4xxx): Direct MMIO via BAR2, ROM_SW_* registers
//! - **Evergreen** (HD 5xxx): Same engine, different GPIO init
//! - **Northern Island** (HD 6xxx): Same engine, extra GPIO setup
//! - **Southern Island** (HD 7xxx): Same engine
//! - **Sea Islands+** (GCN 1.1+: Bonaire, Hawaii, Iceland, Tonga, Fiji, Polaris):
//!   BAR5, SMC indirect register access for ROM_SW_* registers

use crate::ati_pci::{find_ati_spi_device, AtiSpiType};
use crate::error::InternalError;
use crate::physmap::PhysMap;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{AddressWidth, SpiCommand};

// ============================================================================
// R600-family register definitions (direct MMIO via BAR2)
// ============================================================================

mod r600_regs {
    pub const GENERAL_PWRMGT: usize = 0x0618;
    pub const LOWER_GPIO_ENABLE: usize = 0x0710;
    pub const CTXSW_VID_LOWER_GPIO_CNTL: usize = 0x0718;
    pub const HIGH_VID_LOWER_GPIO_CNTL: usize = 0x071c;
    pub const MEDIUM_VID_LOWER_GPIO_CNTL: usize = 0x0720;
    pub const LOW_VID_LOWER_GPIO_CNTL: usize = 0x0724;

    pub const ROM_CNTL: usize = 0x1600;
    pub const PAGE_MIRROR_CNTL: usize = 0x1604;
    pub const ROM_SW_CNTL: usize = 0x1618;
    pub const ROM_SW_STATUS: usize = 0x161C;
    pub const ROM_SW_COMMAND: usize = 0x1620;
    pub const ROM_SW_DATA_BASE: usize = 0x1624;

    pub const GPIOPAD_MASK: usize = 0x1798;
    pub const GPIOPAD_A: usize = 0x179C;
    pub const GPIOPAD_EN: usize = 0x17A0;

    pub const STATUS_LOOP_COUNT: usize = 1000;
    pub const SPI_TRANSFER_SIZE: usize = 0x100;

    #[inline]
    pub const fn rom_sw_data(off: usize) -> usize {
        ROM_SW_DATA_BASE + off
    }
}

// ============================================================================
// CI-family register definitions (SMC indirect via BAR5)
// ============================================================================

mod ci_regs {
    pub const SMC1_INDEX: usize = 0x0208;
    pub const SMC1_DATA: usize = 0x020C;

    pub const GPIOPAD_MASK: usize = 0x0608;
    pub const GPIOPAD_A: usize = 0x060C;
    pub const GPIOPAD_EN: usize = 0x0610;

    pub const DRM_ID_STRAPS: usize = 0x5564;

    // SMC addresses (written to SMC1_INDEX, accessed via SMC1_DATA)
    pub const GENERAL_PWRMGT: u32 = 0xC0200000;
    pub const ROM_CNTL: u32 = 0xC0600000;
    pub const PAGE_MIRROR_CNTL: u32 = 0xC0600004;
    pub const ROM_SW_CNTL: u32 = 0xC060001C;
    pub const ROM_SW_STATUS: u32 = 0xC0600020;
    pub const ROM_SW_COMMAND: u32 = 0xC0600024;
    pub const ROM_SW_DATA_BASE: u32 = 0xC0600028;

    pub const STATUS_LOOP_COUNT: usize = 1000;
    pub const SPI_TRANSFER_SIZE: usize = 0x100;

    #[inline]
    pub const fn rom_sw_data(off: u32) -> u32 {
        ROM_SW_DATA_BASE + off
    }
}

// ============================================================================
// Saved register state for restore on shutdown
// ============================================================================

/// R600-family saved register state
struct R600SavedState {
    general_pwrmgt: u32,
    lower_gpio_enable: u32,
    ctxsw_vid_lower_gpio_cntl: u32,
    high_vid_lower_gpio_cntl: u32,
    medium_vid_lower_gpio_cntl: u32,
    low_vid_lower_gpio_cntl: u32,
    rom_cntl: u32,
    page_mirror_cntl: u32,
    gpiopad_mask: u32,
    gpiopad_a: u32,
    gpiopad_en: u32,
}

/// CI-family saved register state
struct CiSavedState {
    gpiopad_mask: u32,
    gpiopad_a: u32,
    gpiopad_en: u32,
    general_pwrmgt: u32,
    rom_cntl: u32,
    page_mirror_cntl: u32,
}

enum SavedState {
    R600(R600SavedState),
    Ci(CiSavedState),
}

// ============================================================================
// ATI SPI Controller
// ============================================================================

/// ATI/AMD Radeon GPU SPI flash controller
///
/// Accesses the SPI flash on a discrete AMD GPU through MMIO.
#[cfg(all(feature = "std", target_os = "linux"))]
pub struct AtiSpiController {
    bar: PhysMap,
    spi_type: AtiSpiType,
    saved: Option<SavedState>,
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl AtiSpiController {
    /// Create and initialize a new ATI SPI controller.
    ///
    /// Scans the PCI bus for a supported AMD GPU, maps its MMIO BAR,
    /// saves the current register state, and enables SPI access.
    pub fn new(vendor_id: u16, device_id: u16, io_base: u64) -> Result<Self, InternalError> {
        let dev =
            find_ati_spi_device(vendor_id, device_id).ok_or(InternalError::UnsupportedChipset {
                vendor_id,
                device_id,
                name: "Unknown ATI/AMD GPU",
            })?;

        log::info!(
            "ATI SPI: detected {} {} (family: {})",
            dev.vendor_name,
            dev.device_name,
            dev.spi_type.family_name()
        );

        let bar = PhysMap::new(io_base, 0x4000)?;

        let mut ctrl = Self {
            bar,
            spi_type: dev.spi_type,
            saved: None,
        };

        ctrl.save()?;
        ctrl.enable()?;

        Ok(ctrl)
    }

    // ---- MMIO helpers ----

    #[inline]
    fn mmio_read(&self, reg: usize) -> u32 {
        self.bar.read32(reg)
    }

    #[inline]
    fn mmio_read_byte(&self, reg: usize) -> u8 {
        self.bar.read8(reg)
    }

    #[inline]
    fn mmio_write(&self, reg: usize, val: u32) {
        self.bar.write32(reg, val);
    }

    fn mmio_mask(&self, reg: usize, val: u32, mask: u32) {
        let temp = self.mmio_read(reg);
        self.mmio_write(reg, (temp & !mask) | (val & mask));
    }

    // ---- CI-family SMC indirect register access ----

    fn smc_read(&self, address: u32) -> u32 {
        self.mmio_write(ci_regs::SMC1_INDEX, address);
        self.mmio_read(ci_regs::SMC1_DATA)
    }

    fn smc_write(&self, address: u32, value: u32) {
        self.mmio_write(ci_regs::SMC1_INDEX, address);
        self.mmio_write(ci_regs::SMC1_DATA, value);
    }

    fn smc_mask(&self, address: u32, val: u32, mask: u32) {
        self.mmio_write(ci_regs::SMC1_INDEX, address);
        self.mmio_mask(ci_regs::SMC1_DATA, val, mask);
    }

    // ---- Save/Restore ----

    fn save(&mut self) -> Result<(), InternalError> {
        log::debug!("ATI SPI: saving register state");

        if self.spi_type.is_ci() {
            self.saved = Some(SavedState::Ci(CiSavedState {
                general_pwrmgt: self.smc_read(ci_regs::GENERAL_PWRMGT),
                rom_cntl: self.smc_read(ci_regs::ROM_CNTL),
                page_mirror_cntl: self.smc_read(ci_regs::PAGE_MIRROR_CNTL),
                gpiopad_mask: self.mmio_read(ci_regs::GPIOPAD_MASK),
                gpiopad_a: self.mmio_read(ci_regs::GPIOPAD_A),
                gpiopad_en: self.mmio_read(ci_regs::GPIOPAD_EN),
            }));
        } else {
            self.saved = Some(SavedState::R600(R600SavedState {
                general_pwrmgt: self.mmio_read(r600_regs::GENERAL_PWRMGT),
                lower_gpio_enable: self.mmio_read(r600_regs::LOWER_GPIO_ENABLE),
                ctxsw_vid_lower_gpio_cntl: self.mmio_read(r600_regs::CTXSW_VID_LOWER_GPIO_CNTL),
                high_vid_lower_gpio_cntl: self.mmio_read(r600_regs::HIGH_VID_LOWER_GPIO_CNTL),
                medium_vid_lower_gpio_cntl: self.mmio_read(r600_regs::MEDIUM_VID_LOWER_GPIO_CNTL),
                low_vid_lower_gpio_cntl: self.mmio_read(r600_regs::LOW_VID_LOWER_GPIO_CNTL),
                rom_cntl: self.mmio_read(r600_regs::ROM_CNTL),
                page_mirror_cntl: self.mmio_read(r600_regs::PAGE_MIRROR_CNTL),
                gpiopad_mask: self.mmio_read(r600_regs::GPIOPAD_MASK),
                gpiopad_a: self.mmio_read(r600_regs::GPIOPAD_A),
                gpiopad_en: self.mmio_read(r600_regs::GPIOPAD_EN),
            }));
        }
        Ok(())
    }

    fn restore(&self) {
        log::debug!("ATI SPI: restoring register state");

        match &self.saved {
            Some(SavedState::R600(s)) => {
                self.mmio_write(r600_regs::ROM_CNTL, s.rom_cntl);
                self.mmio_write(r600_regs::GPIOPAD_A, s.gpiopad_a);
                self.mmio_write(r600_regs::GPIOPAD_EN, s.gpiopad_en);
                self.mmio_write(r600_regs::GPIOPAD_MASK, s.gpiopad_mask);
                self.mmio_write(r600_regs::GENERAL_PWRMGT, s.general_pwrmgt);
                self.mmio_write(
                    r600_regs::CTXSW_VID_LOWER_GPIO_CNTL,
                    s.ctxsw_vid_lower_gpio_cntl,
                );
                self.mmio_write(
                    r600_regs::HIGH_VID_LOWER_GPIO_CNTL,
                    s.high_vid_lower_gpio_cntl,
                );
                self.mmio_write(
                    r600_regs::MEDIUM_VID_LOWER_GPIO_CNTL,
                    s.medium_vid_lower_gpio_cntl,
                );
                self.mmio_write(
                    r600_regs::LOW_VID_LOWER_GPIO_CNTL,
                    s.low_vid_lower_gpio_cntl,
                );
                self.mmio_write(r600_regs::LOWER_GPIO_ENABLE, s.lower_gpio_enable);
                self.mmio_write(r600_regs::PAGE_MIRROR_CNTL, s.page_mirror_cntl);
            }
            Some(SavedState::Ci(s)) => {
                self.smc_write(ci_regs::ROM_CNTL, s.rom_cntl);
                self.mmio_write(ci_regs::GPIOPAD_A, s.gpiopad_a);
                self.mmio_write(ci_regs::GPIOPAD_EN, s.gpiopad_en);
                self.mmio_write(ci_regs::GPIOPAD_MASK, s.gpiopad_mask);
                self.smc_write(ci_regs::GENERAL_PWRMGT, s.general_pwrmgt);
                self.smc_write(ci_regs::PAGE_MIRROR_CNTL, s.page_mirror_cntl);
            }
            None => {}
        }
    }

    // ---- Enable SPI access ----

    fn enable(&self) -> Result<(), InternalError> {
        log::debug!(
            "ATI SPI: enabling SPI access (family: {})",
            self.spi_type.family_name()
        );

        if self.spi_type.is_ci() {
            self.ci_enable()
        } else {
            self.r600_enable()
        }
    }

    fn r600_enable(&self) -> Result<(), InternalError> {
        if self.spi_type == AtiSpiType::Rv730 {
            // Set (unused?) PCIe clock divider
            self.mmio_mask(r600_regs::ROM_CNTL, 0x19000002, 0xFF000002);
        } else {
            // Software enable clock gating, set SCK divider to 1
            self.mmio_mask(r600_regs::ROM_CNTL, 0x10000002, 0xF0000002);
        }

        if self.spi_type == AtiSpiType::NorthernIsland {
            // Additional GPIO lines (not restored by ATI's own tool)
            self.mmio_mask(0x64A0, 0x100, 0x100);
            self.mmio_mask(0x64A8, 0x100, 0x100);
            self.mmio_mask(0x64A4, 0x100, 0x100);
        }

        // Set GPIO 7,8,9 low
        self.mmio_mask(r600_regs::GPIOPAD_A, 0, 0x0700);
        // GPIO7 input, GPIO8/9 output
        self.mmio_mask(r600_regs::GPIOPAD_EN, 0x0600, 0x0700);
        // Software control on GPIO 7,8,9
        self.mmio_mask(r600_regs::GPIOPAD_MASK, 0x0700, 0x0700);

        // Disable open drain pads
        self.mmio_mask(r600_regs::GENERAL_PWRMGT, 0, 0x0800);

        if matches!(
            self.spi_type,
            AtiSpiType::R600
                | AtiSpiType::Rv730
                | AtiSpiType::Evergreen
                | AtiSpiType::NorthernIsland
        ) {
            self.mmio_mask(r600_regs::CTXSW_VID_LOWER_GPIO_CNTL, 0, 0x0400);
            self.mmio_mask(r600_regs::HIGH_VID_LOWER_GPIO_CNTL, 0, 0x0400);
            self.mmio_mask(r600_regs::MEDIUM_VID_LOWER_GPIO_CNTL, 0, 0x0400);
            self.mmio_mask(r600_regs::LOW_VID_LOWER_GPIO_CNTL, 0, 0x0400);
        }

        if matches!(self.spi_type, AtiSpiType::R600 | AtiSpiType::Rv730) {
            self.mmio_mask(r600_regs::LOWER_GPIO_ENABLE, 0x0400, 0x0400);
        }

        std::thread::sleep(std::time::Duration::from_millis(1));

        self.mmio_mask(r600_regs::GPIOPAD_MASK, 0, 0x700);
        self.mmio_mask(r600_regs::GPIOPAD_EN, 0, 0x700);
        self.mmio_mask(r600_regs::GPIOPAD_A, 0, 0x00080000);

        // Page mirror usage
        self.mmio_mask(r600_regs::PAGE_MIRROR_CNTL, 0x04000000, 0x0C000000);

        // Clear ROM_SW_STATUS
        if self.mmio_read(r600_regs::ROM_SW_STATUS) != 0 {
            for i in 0..r600_regs::STATUS_LOOP_COUNT {
                self.mmio_write(r600_regs::ROM_SW_STATUS, 0);
                std::thread::sleep(std::time::Duration::from_millis(1));
                if self.mmio_read(r600_regs::ROM_SW_STATUS) == 0 {
                    break;
                }
                if i == r600_regs::STATUS_LOOP_COUNT - 1 {
                    log::error!("ATI SPI: failed to clear ROM_SW_STATUS");
                    return Err(InternalError::SpiInit("failed to clear R600 ROM_SW_STATUS"));
                }
            }
        }

        Ok(())
    }

    fn ci_enable(&self) -> Result<(), InternalError> {
        // Set SCK divider to 1
        self.smc_mask(ci_regs::ROM_CNTL, 0x10000000, 0xF0000000);

        // Software enable clock gating
        if self.spi_type == AtiSpiType::Bonaire {
            let drm = self.mmio_read(ci_regs::DRM_ID_STRAPS);
            if drm & 0xF0000000 != 0 {
                self.smc_mask(ci_regs::ROM_CNTL, 0, 0x0000002);
            } else {
                self.smc_mask(ci_regs::ROM_CNTL, 0x0000002, 0x0000002);
            }
        } else {
            self.smc_mask(ci_regs::ROM_CNTL, 0x00000002, 0x00000002);
        }

        // Set GPIO 7,8,9 low
        self.mmio_mask(ci_regs::GPIOPAD_A, 0, 0x0700);
        // GPIO7 input, GPIO8/9 output
        self.mmio_mask(ci_regs::GPIOPAD_EN, 0x0600, 0x0700);
        // Software control on GPIO 7,8,9
        self.mmio_mask(ci_regs::GPIOPAD_MASK, 0x0700, 0x0700);

        if self.spi_type != AtiSpiType::Bonaire {
            self.mmio_mask(ci_regs::GPIOPAD_MASK, 0x40000000, 0x40000000);
            self.mmio_mask(ci_regs::GPIOPAD_EN, 0x40000000, 0x40000000);
            self.mmio_mask(ci_regs::GPIOPAD_A, 0x40000000, 0x40000000);
        }

        if !matches!(self.spi_type, AtiSpiType::Bonaire | AtiSpiType::Hawaii) {
            // Disable open drain pads
            self.smc_mask(ci_regs::GENERAL_PWRMGT, 0, 0x0800);
        }

        std::thread::sleep(std::time::Duration::from_millis(1));

        self.mmio_mask(ci_regs::GPIOPAD_MASK, 0, 0x700);
        self.mmio_mask(ci_regs::GPIOPAD_EN, 0, 0x700);
        self.mmio_mask(ci_regs::GPIOPAD_A, 0, 0x700);

        // Remnant of generations past
        self.mmio_mask(ci_regs::GPIOPAD_MASK, 0, 0x80000);
        self.mmio_mask(ci_regs::GPIOPAD_EN, 0, 0x80000);
        self.mmio_mask(ci_regs::GPIOPAD_A, 0, 0x80000);

        // Page mirror usage
        self.smc_mask(ci_regs::PAGE_MIRROR_CNTL, 0x04000000, 0x0C000000);

        // Clear ROM_SW_STATUS
        if self.smc_read(ci_regs::ROM_SW_STATUS) != 0 {
            for i in 0..ci_regs::STATUS_LOOP_COUNT {
                self.smc_write(ci_regs::ROM_SW_STATUS, 0);
                std::thread::sleep(std::time::Duration::from_millis(1));
                if self.smc_read(ci_regs::ROM_SW_STATUS) == 0 {
                    break;
                }
                if i == ci_regs::STATUS_LOOP_COUNT - 1 {
                    log::error!("ATI SPI: failed to clear CI ROM_SW_STATUS");
                    return Err(InternalError::SpiInit("failed to clear CI ROM_SW_STATUS"));
                }
            }
        }

        Ok(())
    }

    // ---- SPI command execution ----

    /// Execute a raw SPI command (R600 family — direct MMIO).
    fn r600_spi_command(&self, writearr: &[u8], readarr: &mut [u8]) -> Result<(), InternalError> {
        let writecnt = writearr.len();
        let readcnt = readarr.len();

        // Build the 4-byte command register: opcode | addr[2] | addr[1] | addr[0]
        let mut command: u32 = writearr[0] as u32;
        if writecnt > 1 {
            command |= (writearr[1] as u32) << 24;
        }
        if writecnt > 2 {
            command |= (writearr[2] as u32) << 16;
        }
        if writecnt > 3 {
            command |= (writearr[3] as u32) << 8;
        }

        let command_size = writecnt.min(4);

        self.mmio_write(r600_regs::ROM_SW_COMMAND, command);

        // Write remaining data bytes (after the 4-byte command) to FIFO.
        // ATI HW does 32-bit register writes; writing 8 bits zeroes upper bytes.
        // Also endianness is swapped between read and write paths.
        let mut i = 4;
        while i < writecnt {
            let mut value: u32 = 0;
            let remainder = (writecnt - i).min(4);

            if remainder > 0 {
                value |= (writearr[i] as u32) << 24;
            }
            if remainder > 1 {
                value |= (writearr[i + 1] as u32) << 16;
            }
            if remainder > 2 {
                value |= (writearr[i + 2] as u32) << 8;
            }
            if remainder > 3 {
                value |= writearr[i + 3] as u32;
            }

            self.mmio_write(r600_regs::rom_sw_data(i - 4), value);
            i += 4;
        }

        // Build control word and trigger
        let mut control: u32 = ((command_size - 1) as u32) << 0x10;
        if readcnt > 0 {
            control |= 0x40000 | readcnt as u32;
        } else if writecnt > 4 {
            control |= (writecnt - 4) as u32;
        }
        self.mmio_write(r600_regs::ROM_SW_CNTL, control);

        // Poll for completion
        for j in 0..r600_regs::STATUS_LOOP_COUNT {
            if self.mmio_read(r600_regs::ROM_SW_STATUS) != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
            if j == r600_regs::STATUS_LOOP_COUNT - 1 {
                log::error!("ATI SPI: R600 SPI command timed out");
                return Err(InternalError::Io("R600 SPI command timed out"));
            }
        }
        self.mmio_write(r600_regs::ROM_SW_STATUS, 0);

        // Read response data byte-by-byte
        for j in 0..readcnt {
            readarr[j] = self.mmio_read_byte(r600_regs::rom_sw_data(j));
        }

        Ok(())
    }

    /// Execute a raw SPI command (CI family — SMC indirect).
    fn ci_spi_command(&self, writearr: &[u8], readarr: &mut [u8]) -> Result<(), InternalError> {
        let writecnt = writearr.len();
        let readcnt = readarr.len();

        // Build the 4-byte command register
        let mut command: u32 = writearr[0] as u32;
        if writecnt > 1 {
            command |= (writearr[1] as u32) << 24;
        }
        if writecnt > 2 {
            command |= (writearr[2] as u32) << 16;
        }
        if writecnt > 3 {
            command |= (writearr[3] as u32) << 8;
        }

        let command_size = writecnt.min(4);

        self.smc_write(ci_regs::ROM_SW_COMMAND, command);

        // Write remaining data
        let mut i = 4u32;
        while (i as usize) < writecnt {
            let mut value: u32 = 0;
            let remainder = (writecnt - i as usize).min(4);

            if remainder > 0 {
                value |= (writearr[i as usize] as u32) << 24;
            }
            if remainder > 1 {
                value |= (writearr[i as usize + 1] as u32) << 16;
            }
            if remainder > 2 {
                value |= (writearr[i as usize + 2] as u32) << 8;
            }
            if remainder > 3 {
                value |= writearr[i as usize + 3] as u32;
            }

            // Bonaire has a gap between 0xD8 and 0xE8
            if self.spi_type == AtiSpiType::Bonaire && i >= 0xdc {
                self.smc_write(ci_regs::rom_sw_data(i + 0x0C - 4), value);
            } else {
                self.smc_write(ci_regs::rom_sw_data(i - 4), value);
            }
            i += 4;
        }

        // Build control word and trigger
        let mut control: u32 = ((command_size - 1) as u32) << 0x10;
        if readcnt > 0 {
            control |= 0x40000 | readcnt as u32;
        } else if writecnt > 4 {
            control |= (writecnt - 4) as u32;
        }
        self.smc_write(ci_regs::ROM_SW_CNTL, control);

        // Poll for completion
        for j in 0..ci_regs::STATUS_LOOP_COUNT {
            if self.smc_read(ci_regs::ROM_SW_STATUS) != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
            if j == ci_regs::STATUS_LOOP_COUNT - 1 {
                log::error!("ATI SPI: CI SPI command timed out");
                return Err(InternalError::Io("CI SPI command timed out"));
            }
        }
        self.smc_write(ci_regs::ROM_SW_STATUS, 0);

        // Read response data (32-bit words, unpacked)
        let mut i = 0u32;
        while (i as usize) < readcnt {
            let value = if self.spi_type == AtiSpiType::Bonaire && i >= 0xd8 {
                self.smc_read(ci_regs::rom_sw_data(i + 0x10))
            } else {
                self.smc_read(ci_regs::rom_sw_data(i))
            };

            let remainder = readcnt - i as usize;
            if remainder > 0 {
                readarr[i as usize] = value as u8;
            }
            if remainder > 1 {
                readarr[i as usize + 1] = (value >> 8) as u8;
            }
            if remainder > 2 {
                readarr[i as usize + 2] = (value >> 16) as u8;
            }
            if remainder > 3 {
                readarr[i as usize + 3] = (value >> 24) as u8;
            }
            i += 4;
        }

        Ok(())
    }

    /// Execute a raw SPI command, dispatching to the correct family.
    pub fn send_command(&self, writearr: &[u8], readarr: &mut [u8]) -> Result<(), InternalError> {
        if writearr.is_empty() {
            return Err(InternalError::Io("SPI command must have at least 1 byte"));
        }

        log::trace!(
            "ATI SPI: cmd 0x{:02x}, write {} bytes, read {} bytes",
            writearr[0],
            writearr.len(),
            readarr.len()
        );

        if self.spi_type.is_ci() {
            self.ci_spi_command(writearr, readarr)
        } else {
            self.r600_spi_command(writearr, readarr)
        }
    }

    /// Maximum SPI transfer size for this controller
    pub fn max_transfer_size(&self) -> usize {
        if self.spi_type.is_ci() {
            ci_regs::SPI_TRANSFER_SIZE
        } else {
            r600_regs::SPI_TRANSFER_SIZE
        }
    }

    /// Get the SPI type / GPU family
    pub fn spi_type(&self) -> AtiSpiType {
        self.spi_type
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl Drop for AtiSpiController {
    fn drop(&mut self) {
        self.restore();
    }
}

// ============================================================================
// SpiMaster trait implementation
// ============================================================================

#[cfg(all(feature = "std", target_os = "linux"))]
impl SpiMaster for AtiSpiController {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::empty()
    }

    fn max_read_len(&self) -> usize {
        self.max_transfer_size()
    }

    fn max_write_len(&self) -> usize {
        // +1 because the opcode is sent separately from the data
        self.max_transfer_size() + 1
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        // Build the write array: opcode + address + write data
        let max = self.max_transfer_size() + 5; // opcode + 4 addr + data
        let mut writearr = alloc::vec![0u8; max];
        let mut write_len = 1;

        writearr[0] = cmd.opcode;

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

        let write_data = cmd.write_data;
        if !write_data.is_empty() {
            let data_len = write_data.len().min(max - write_len);
            writearr[write_len..write_len + data_len].copy_from_slice(&write_data[..data_len]);
            write_len += data_len;
        }

        self.send_command(&writearr[..write_len], cmd.read_buf)
            .map_err(map_ati_error)
    }

    fn probe_opcode(&self, _opcode: u8) -> bool {
        true // ATI SPI doesn't restrict opcodes
    }

    fn delay_us(&mut self, us: u32) {
        std::thread::sleep(std::time::Duration::from_micros(us as u64));
    }
}

fn map_ati_error(e: InternalError) -> CoreError {
    match e {
        InternalError::Io(_) => CoreError::IoError,
        InternalError::SpiInit(_) => CoreError::ProgrammerError,
        InternalError::MemoryMap { .. } => CoreError::ProgrammerError,
        _ => CoreError::ProgrammerError,
    }
}

// ============================================================================
// Non-Linux stub
// ============================================================================

#[cfg(not(all(feature = "std", target_os = "linux")))]
pub struct AtiSpiController {
    _private: (),
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
impl AtiSpiController {
    pub fn new(_vendor_id: u16, _device_id: u16, _io_base: u64) -> Result<Self, InternalError> {
        Err(InternalError::NotSupported(
            "ATI SPI programmer only supported on Linux",
        ))
    }

    pub fn send_command(&self, _writearr: &[u8], _readarr: &mut [u8]) -> Result<(), InternalError> {
        Err(InternalError::NotSupported(
            "ATI SPI programmer only supported on Linux",
        ))
    }

    pub fn max_transfer_size(&self) -> usize {
        0
    }

    pub fn spi_type(&self) -> crate::ati_pci::AtiSpiType {
        crate::ati_pci::AtiSpiType::R600
    }
}

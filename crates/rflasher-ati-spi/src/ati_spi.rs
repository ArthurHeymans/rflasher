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

use crate::ati_pci::{AtiSpiType, find_ati_spi_device};
use crate::error::AtiSpiError;
use maybe_async::maybe_async;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{AddressWidth, SpiCommand};

use core::marker::PhantomData;
use tock_registers::fields::FieldValue;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::registers::{ReadOnly, ReadWrite};
use tock_registers::{LocalRegisterCopy, RegisterLongName, register_bitfields, register_structs};

register_bitfields![u32,
 GeneralPower [ OPEN_DRAIN OFFSET(11) NUMBITS(1) [] ],
 Gpio10 [ PIN OFFSET(10) NUMBITS(1) [] ],
 RomControl [ CLOCK_GATE OFFSET(1) NUMBITS(1) [], DIVIDER OFFSET(24) NUMBITS(8) [], PRESCALE OFFSET(28) NUMBITS(4) [] ],
 PageMirror [ MODE OFFSET(26) NUMBITS(2) [] ],
 SoftwareControl [ DATA_BYTES OFFSET(0) NUMBITS(16) [], COMMAND_BYTES OFFSET(16) NUMBITS(2) [], READ OFFSET(18) NUMBITS(1) [] ],
 SoftwareStatus [ COMPLETE OFFSET(0) NUMBITS(32) [] ],
 GpioPad [ SPI OFFSET(7) NUMBITS(3) [], LEGACY OFFSET(19) NUMBITS(1) [], EXTRA OFFSET(30) NUMBITS(1) [] ],
 NiGpio [ ENABLE OFFSET(8) NUMBITS(1) [] ],
 DrmStraps [ VALUE OFFSET(28) NUMBITS(4) [] ]
];

register_structs! {
 R600Registers {
  (0x0000 => _r0), (0x0618 => general_power: ReadWrite<u32, GeneralPower::Register>),
  (0x061c => _r1), (0x0710 => lower_gpio_enable: ReadWrite<u32, Gpio10::Register>),
  (0x0714 => _r2), (0x0718 => ctx_gpio: ReadWrite<u32, Gpio10::Register>),
  (0x071c => high_gpio: ReadWrite<u32, Gpio10::Register>), (0x0720 => medium_gpio: ReadWrite<u32, Gpio10::Register>),
  (0x0724 => low_gpio: ReadWrite<u32, Gpio10::Register>), (0x0728 => _r3),
  (0x1600 => rom_control: ReadWrite<u32, RomControl::Register>), (0x1604 => page_mirror: ReadWrite<u32, PageMirror::Register>),
  (0x1608 => _r4), (0x1618 => sw_control: ReadWrite<u32, SoftwareControl::Register>),
  (0x161c => sw_status: ReadWrite<u32, SoftwareStatus::Register>), (0x1620 => sw_command: ReadWrite<u32>),
  (0x1624 => sw_data: [ReadWrite<u32>; 64]), (0x1724 => _r5),
  (0x1798 => gpio_mask: ReadWrite<u32, GpioPad::Register>), (0x179c => gpio_value: ReadWrite<u32, GpioPad::Register>),
  (0x17a0 => gpio_enable: ReadWrite<u32, GpioPad::Register>), (0x17a4 => _r6),
  (0x64a0 => ni_gpio0: ReadWrite<u32, NiGpio::Register>), (0x64a4 => ni_gpio1: ReadWrite<u32, NiGpio::Register>),
  (0x64a8 => ni_gpio2: ReadWrite<u32, NiGpio::Register>), (0x64ac => @END),
 },
 CiRegisters {
  (0x0000 => _r0), (0x0208 => smc_index: ReadWrite<u32>), (0x020c => smc_data: ReadWrite<u32>),
  (0x0210 => _r1), (0x0608 => gpio_mask: ReadWrite<u32, GpioPad::Register>),
  (0x060c => gpio_value: ReadWrite<u32, GpioPad::Register>), (0x0610 => gpio_enable: ReadWrite<u32, GpioPad::Register>),
  (0x0614 => _r2), (0x5564 => drm_straps: ReadOnly<u32, DrmStraps::Register>), (0x5568 => @END),
 }
}

const STATUS_LOOP_COUNT: usize = 1000;
const SPI_TRANSFER_SIZE: usize = 0x100;
#[derive(Clone, Copy)]
struct SmcRegister<R: RegisterLongName> {
    address: u32,
    marker: PhantomData<R>,
}
impl<R: RegisterLongName> SmcRegister<R> {
    const fn new(address: u32) -> Self {
        Self {
            address,
            marker: PhantomData,
        }
    }
}
const SMC_GENERAL_POWER: SmcRegister<GeneralPower::Register> = SmcRegister::new(0xC0200000);
const SMC_ROM_CONTROL: SmcRegister<RomControl::Register> = SmcRegister::new(0xC0600000);
const SMC_PAGE_MIRROR: SmcRegister<PageMirror::Register> = SmcRegister::new(0xC0600004);
const SMC_SW_CONTROL: SmcRegister<SoftwareControl::Register> = SmcRegister::new(0xC060001C);
const SMC_SW_STATUS: SmcRegister<SoftwareStatus::Register> = SmcRegister::new(0xC0600020);
const SMC_SW_COMMAND: SmcRegister<()> = SmcRegister::new(0xC0600024);
const SMC_SW_DATA_BASE: u32 = 0xC0600028;

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
    bar: rflasher_pci::MappedBar,
    spi_type: AtiSpiType,
    saved: Option<SavedState>,
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl AtiSpiController {
    /// Create and initialize a new ATI SPI controller.
    ///
    /// Scans the PCI bus for a supported AMD GPU, maps its MMIO BAR,
    /// saves the current register state, and enables SPI access.
    pub fn new(
        vendor_id: u16,
        device_id: u16,
        address: rflasher_pci::PciAddress,
        bar_index: u8,
    ) -> Result<Self, AtiSpiError> {
        let dev =
            find_ati_spi_device(vendor_id, device_id).ok_or(AtiSpiError::UnsupportedDevice {
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

        let bar = rflasher_pci::SysfsPci::system().map_bar(address, bar_index, 0x7000)?;

        let mut ctrl = Self {
            bar,
            spi_type: dev.spi_type,
            saved: None,
        };

        ctrl.save()?;
        ctrl.enable()?;

        Ok(ctrl)
    }

    fn r600(&self) -> &R600Registers {
        // SAFETY: R600Registers describes the BAR layout for non-CI GPUs.
        unsafe { self.bar.registers() }
    }

    fn ci(&self) -> &CiRegisters {
        // SAFETY: CiRegisters describes the BAR layout for CI-family GPUs.
        unsafe { self.bar.registers() }
    }

    fn smc_read<R: RegisterLongName>(&self, register: SmcRegister<R>) -> LocalRegisterCopy<u32, R> {
        self.ci().smc_index.set(register.address);
        LocalRegisterCopy::new(self.ci().smc_data.get())
    }

    fn smc_write<R: RegisterLongName>(&self, register: SmcRegister<R>, value: u32) {
        self.ci().smc_index.set(register.address);
        self.ci().smc_data.set(value);
    }

    fn smc_modify<R: RegisterLongName>(
        &self,
        register: SmcRegister<R>,
        fields: FieldValue<u32, R>,
    ) {
        let address = register.address;
        let mut value = self.smc_read(register);
        value.modify(fields);
        self.smc_write(SmcRegister::<R>::new(address), value.get());
    }

    fn smc_data_register(offset: u32) -> SmcRegister<()> {
        SmcRegister::new(SMC_SW_DATA_BASE + offset)
    }

    // ---- Save/Restore ----

    fn save(&mut self) -> Result<(), AtiSpiError> {
        log::debug!("ATI SPI: saving register state");
        if self.spi_type.is_ci() {
            let r = self.ci();
            self.saved = Some(SavedState::Ci(CiSavedState {
                general_pwrmgt: self.smc_read(SMC_GENERAL_POWER).get(),
                rom_cntl: self.smc_read(SMC_ROM_CONTROL).get(),
                page_mirror_cntl: self.smc_read(SMC_PAGE_MIRROR).get(),
                gpiopad_mask: r.gpio_mask.get(),
                gpiopad_a: r.gpio_value.get(),
                gpiopad_en: r.gpio_enable.get(),
            }));
        } else {
            let r = self.r600();
            self.saved = Some(SavedState::R600(R600SavedState {
                general_pwrmgt: r.general_power.get(),
                lower_gpio_enable: r.lower_gpio_enable.get(),
                ctxsw_vid_lower_gpio_cntl: r.ctx_gpio.get(),
                high_vid_lower_gpio_cntl: r.high_gpio.get(),
                medium_vid_lower_gpio_cntl: r.medium_gpio.get(),
                low_vid_lower_gpio_cntl: r.low_gpio.get(),
                rom_cntl: r.rom_control.get(),
                page_mirror_cntl: r.page_mirror.get(),
                gpiopad_mask: r.gpio_mask.get(),
                gpiopad_a: r.gpio_value.get(),
                gpiopad_en: r.gpio_enable.get(),
            }));
        }
        Ok(())
    }

    fn restore(&self) {
        log::debug!("ATI SPI: restoring register state");
        match &self.saved {
            Some(SavedState::R600(s)) => {
                let r = self.r600();
                r.rom_control.set(s.rom_cntl);
                r.gpio_value.set(s.gpiopad_a);
                r.gpio_enable.set(s.gpiopad_en);
                r.gpio_mask.set(s.gpiopad_mask);
                r.general_power.set(s.general_pwrmgt);
                r.ctx_gpio.set(s.ctxsw_vid_lower_gpio_cntl);
                r.high_gpio.set(s.high_vid_lower_gpio_cntl);
                r.medium_gpio.set(s.medium_vid_lower_gpio_cntl);
                r.low_gpio.set(s.low_vid_lower_gpio_cntl);
                r.lower_gpio_enable.set(s.lower_gpio_enable);
                r.page_mirror.set(s.page_mirror_cntl);
            }
            Some(SavedState::Ci(s)) => {
                let r = self.ci();
                self.smc_write(SMC_ROM_CONTROL, s.rom_cntl);
                r.gpio_value.set(s.gpiopad_a);
                r.gpio_enable.set(s.gpiopad_en);
                r.gpio_mask.set(s.gpiopad_mask);
                self.smc_write(SMC_GENERAL_POWER, s.general_pwrmgt);
                self.smc_write(SMC_PAGE_MIRROR, s.page_mirror_cntl);
            }
            None => {}
        }
    }

    fn enable(&self) -> Result<(), AtiSpiError> {
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

    fn r600_enable(&self) -> Result<(), AtiSpiError> {
        let r = self.r600();
        if self.spi_type == AtiSpiType::Rv730 {
            r.rom_control
                .modify(RomControl::DIVIDER.val(0x19) + RomControl::CLOCK_GATE::SET);
        } else {
            r.rom_control
                .modify(RomControl::PRESCALE.val(1) + RomControl::CLOCK_GATE::SET);
        }
        if self.spi_type == AtiSpiType::NorthernIsland {
            r.ni_gpio0.modify(NiGpio::ENABLE::SET);
            r.ni_gpio1.modify(NiGpio::ENABLE::SET);
            r.ni_gpio2.modify(NiGpio::ENABLE::SET);
        }
        r.gpio_value.modify(GpioPad::SPI.val(0));
        r.gpio_enable.modify(GpioPad::SPI.val(6));
        r.gpio_mask.modify(GpioPad::SPI.val(7));
        r.general_power.modify(GeneralPower::OPEN_DRAIN::CLEAR);
        if matches!(
            self.spi_type,
            AtiSpiType::R600
                | AtiSpiType::Rv730
                | AtiSpiType::Evergreen
                | AtiSpiType::NorthernIsland
        ) {
            r.ctx_gpio.modify(Gpio10::PIN::CLEAR);
            r.high_gpio.modify(Gpio10::PIN::CLEAR);
            r.medium_gpio.modify(Gpio10::PIN::CLEAR);
            r.low_gpio.modify(Gpio10::PIN::CLEAR);
        }
        if matches!(self.spi_type, AtiSpiType::R600 | AtiSpiType::Rv730) {
            r.lower_gpio_enable.modify(Gpio10::PIN::SET);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        r.gpio_mask.modify(GpioPad::SPI::CLEAR);
        r.gpio_enable.modify(GpioPad::SPI::CLEAR);
        r.gpio_value.modify(GpioPad::LEGACY::CLEAR);
        r.page_mirror.modify(PageMirror::MODE.val(1));
        if r.sw_status.get() != 0 {
            for i in 0..STATUS_LOOP_COUNT {
                r.sw_status.set(0);
                std::thread::sleep(std::time::Duration::from_millis(1));
                if r.sw_status.get() == 0 {
                    break;
                }
                if i == STATUS_LOOP_COUNT - 1 {
                    return Err(AtiSpiError::SpiInit("failed to clear R600 ROM_SW_STATUS"));
                }
            }
        }
        Ok(())
    }

    fn ci_enable(&self) -> Result<(), AtiSpiError> {
        let r = self.ci();
        self.smc_modify(SMC_ROM_CONTROL, RomControl::PRESCALE.val(1));
        let gate =
            if self.spi_type == AtiSpiType::Bonaire && r.drm_straps.read(DrmStraps::VALUE) != 0 {
                RomControl::CLOCK_GATE::CLEAR
            } else {
                RomControl::CLOCK_GATE::SET
            };
        self.smc_modify(SMC_ROM_CONTROL, gate);
        r.gpio_value.modify(GpioPad::SPI.val(0));
        r.gpio_enable.modify(GpioPad::SPI.val(6));
        r.gpio_mask.modify(GpioPad::SPI.val(7));
        if self.spi_type != AtiSpiType::Bonaire {
            r.gpio_mask.modify(GpioPad::EXTRA::SET);
            r.gpio_enable.modify(GpioPad::EXTRA::SET);
            r.gpio_value.modify(GpioPad::EXTRA::SET);
        }
        if !matches!(self.spi_type, AtiSpiType::Bonaire | AtiSpiType::Hawaii) {
            self.smc_modify(SMC_GENERAL_POWER, GeneralPower::OPEN_DRAIN::CLEAR);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        r.gpio_mask
            .modify(GpioPad::SPI::CLEAR + GpioPad::LEGACY::CLEAR);
        r.gpio_enable
            .modify(GpioPad::SPI::CLEAR + GpioPad::LEGACY::CLEAR);
        r.gpio_value
            .modify(GpioPad::SPI::CLEAR + GpioPad::LEGACY::CLEAR);
        self.smc_modify(SMC_PAGE_MIRROR, PageMirror::MODE.val(1));
        if self.smc_read(SMC_SW_STATUS).get() != 0 {
            for i in 0..STATUS_LOOP_COUNT {
                self.smc_write(SMC_SW_STATUS, 0);
                std::thread::sleep(std::time::Duration::from_millis(1));
                if self.smc_read(SMC_SW_STATUS).get() == 0 {
                    break;
                }
                if i == STATUS_LOOP_COUNT - 1 {
                    return Err(AtiSpiError::SpiInit("failed to clear CI ROM_SW_STATUS"));
                }
            }
        }
        Ok(())
    }

    fn r600_spi_command(&self, writearr: &[u8], readarr: &mut [u8]) -> Result<(), AtiSpiError> {
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

        let r = self.r600();
        r.sw_command.set(command);

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

            r.sw_data[(i - 4) / 4].set(value);
            i += 4;
        }

        // Build the typed control register and trigger the transfer.
        let data_bytes = if readcnt > 0 {
            readcnt
        } else {
            writecnt.saturating_sub(4)
        };
        let mut fields = SoftwareControl::COMMAND_BYTES.val((command_size - 1) as u32)
            + SoftwareControl::DATA_BYTES.val(data_bytes as u32);
        if readcnt > 0 {
            fields += SoftwareControl::READ::SET;
        }
        r.sw_control.write(fields);

        // Poll for completion
        for j in 0..STATUS_LOOP_COUNT {
            if r.sw_status.get() != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
            if j == STATUS_LOOP_COUNT - 1 {
                log::error!("ATI SPI: R600 SPI command timed out");
                return Err(AtiSpiError::Io("R600 SPI command timed out"));
            }
        }
        r.sw_status.set(0);

        // Read response data byte-by-byte
        for (j, byte) in readarr[..readcnt].iter_mut().enumerate() {
            *byte = ((r.sw_data[j / 4].get() >> ((j % 4) * 8)) & 0xff) as u8;
        }

        Ok(())
    }

    /// Execute a raw SPI command (CI family — SMC indirect).
    fn ci_spi_command(&self, writearr: &[u8], readarr: &mut [u8]) -> Result<(), AtiSpiError> {
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

        self.smc_write(SMC_SW_COMMAND, command);

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
                self.smc_write(Self::smc_data_register(i + 0x0C), value);
            } else {
                self.smc_write(Self::smc_data_register(i - 4), value);
            }
            i += 4;
        }

        // Build the typed control register and trigger the transfer.
        let data_bytes = if readcnt > 0 {
            readcnt
        } else {
            writecnt.saturating_sub(4)
        };
        let mut control = LocalRegisterCopy::<u32, SoftwareControl::Register>::new(0);
        let mut fields = SoftwareControl::COMMAND_BYTES.val((command_size - 1) as u32)
            + SoftwareControl::DATA_BYTES.val(data_bytes as u32);
        if readcnt > 0 {
            fields += SoftwareControl::READ::SET;
        }
        control.write(fields);
        self.smc_write(SMC_SW_CONTROL, control.get());

        // Poll for completion
        for j in 0..STATUS_LOOP_COUNT {
            if self.smc_read(SMC_SW_STATUS).get() != 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
            if j == STATUS_LOOP_COUNT - 1 {
                log::error!("ATI SPI: CI SPI command timed out");
                return Err(AtiSpiError::Io("CI SPI command timed out"));
            }
        }
        self.smc_write(SMC_SW_STATUS, 0);

        // Read response data (32-bit words, unpacked)
        let mut i = 0u32;
        while (i as usize) < readcnt {
            let value = if self.spi_type == AtiSpiType::Bonaire && i >= 0xd8 {
                self.smc_read(Self::smc_data_register(i + 0x10)).get()
            } else {
                self.smc_read(Self::smc_data_register(i)).get()
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
    pub fn send_command(&self, writearr: &[u8], readarr: &mut [u8]) -> Result<(), AtiSpiError> {
        if writearr.is_empty() {
            return Err(AtiSpiError::Io("SPI command must have at least 1 byte"));
        }
        if writearr.len().saturating_sub(4) > SPI_TRANSFER_SIZE {
            return Err(AtiSpiError::Io("SPI write exceeds the hardware FIFO"));
        }
        if readarr.len() > SPI_TRANSFER_SIZE {
            return Err(AtiSpiError::Io("SPI read exceeds the hardware FIFO"));
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
            SPI_TRANSFER_SIZE
        } else {
            SPI_TRANSFER_SIZE
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
#[maybe_async(AFIT)]
impl SpiMaster for AtiSpiController {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::empty()
    }

    fn max_read_len(&self) -> usize {
        self.max_transfer_size()
    }

    fn max_write_len(&self) -> usize {
        self.max_transfer_size()
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        if cmd.write_data.len() > self.max_write_len() || cmd.read_buf.len() > self.max_read_len() {
            return Err(CoreError::SpiTransferFailed);
        }

        // Build the write array: opcode + address + write data
        let max = self.max_transfer_size() + 4; // opcode + 3-byte address + data
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
        if write_data.len() > max - write_len {
            return Err(CoreError::SpiTransferFailed);
        }
        if !write_data.is_empty() {
            writearr[write_len..write_len + write_data.len()].copy_from_slice(write_data);
            write_len += write_data.len();
        }

        self.send_command(&writearr[..write_len], cmd.read_buf)
            .map_err(map_ati_error)
    }

    fn probe_opcode(&self, _opcode: u8) -> bool {
        true // ATI SPI doesn't restrict opcodes
    }

    async fn delay_us(&mut self, us: u32) {
        std::thread::sleep(std::time::Duration::from_micros(us as u64));
    }
}

fn map_ati_error(e: AtiSpiError) -> CoreError {
    match e {
        AtiSpiError::Io(_) => CoreError::IoError,
        AtiSpiError::SpiInit(_) => CoreError::ProgrammerError,
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
    pub fn new(
        _vendor_id: u16,
        _device_id: u16,
        _address: rflasher_pci::PciAddress,
        _bar_index: u8,
    ) -> Result<Self, AtiSpiError> {
        Err(AtiSpiError::NotSupported(
            "ATI SPI programmer only supported on Linux",
        ))
    }

    pub fn send_command(&self, _writearr: &[u8], _readarr: &mut [u8]) -> Result<(), AtiSpiError> {
        Err(AtiSpiError::NotSupported(
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

#[cfg(not(all(feature = "std", target_os = "linux")))]
#[maybe_async(AFIT)]
impl SpiMaster for AtiSpiController {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::empty()
    }

    fn max_read_len(&self) -> usize {
        self.max_transfer_size()
    }

    fn max_write_len(&self) -> usize {
        self.max_transfer_size()
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        self.send_command(&[], cmd.read_buf).map_err(map_ati_error)
    }

    fn probe_opcode(&self, _opcode: u8) -> bool {
        false
    }

    async fn delay_us(&mut self, _us: u32) {}
}

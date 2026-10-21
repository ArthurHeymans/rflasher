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
//! - **Southern Island**: Lombok only; other Southern Islands remain unsupported
//! - **Sea Islands+** (GCN 1.1+: Bonaire, Hawaii, Iceland, Tonga, Fiji, Polaris):
//!   BAR5, SMC indirect register access for ROM_SW_* registers

use super::error::AtiSpiError;
use super::ids::{AtiSpiType, find_ati_spi_device};
use super::ownership::DeviceClaim;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{AddressWidth, SpiCommand, check_io_mode_supported};
use rflasher_pci::PciConfigAccess;

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
 GpioPad [ SPI OFFSET(8) NUMBITS(3) [], LEGACY OFFSET(19) NUMBITS(1) [], EXTRA OFFSET(30) NUMBITS(1) [] ],
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
// Reserve receive space for every dummy byte expressible by SpiCommand, so
// callers can safely chunk reads using max_read_len() even for SFDP/RDUID.
const MAX_DUMMY_BYTES: usize = (u8::MAX as usize).div_ceil(8);
const MAX_READ_SIZE: usize = SPI_TRANSFER_SIZE - MAX_DUMMY_BYTES;

#[derive(Debug)]
struct PreparedCommand {
    write: Vec<u8>,
    receive_len: usize,
    discard: usize,
}

fn prepare_command(cmd: &SpiCommand<'_>) -> CoreResult<PreparedCommand> {
    check_io_mode_supported(cmd.io_mode, SpiFeatures::empty())?;
    // The command register has room for an opcode and only three address bytes.
    // In particular, a fourth address byte cannot be shifted out in read mode.
    if cmd.address_width == AddressWidth::FourByte {
        return Err(CoreError::OpcodeNotSupported);
    }
    match (cmd.address_width, cmd.address) {
        (AddressWidth::None, None) => {}
        (AddressWidth::ThreeByte, Some(address)) if address <= 0xff_ffff => {}
        (AddressWidth::ThreeByte, Some(_)) => return Err(CoreError::AddressOutOfBounds),
        _ => return Err(CoreError::SpiTransferFailed),
    }
    if !cmd.dummy_cycles.is_multiple_of(8)
        || cmd.write_data.len() > SPI_TRANSFER_SIZE
        || cmd.read_buf.len() > MAX_READ_SIZE
        || (cmd.has_read() && cmd.has_write())
    {
        return Err(CoreError::SpiTransferFailed);
    }

    let dummy_bytes = cmd.dummy_cycles as usize / 8;
    if cmd.has_read() {
        // Dummy clocks are part of the receive phase, NOT transmit FIFO data.
        // Read them along with the response and discard the leading bytes.
        let mut header = [0; 4];
        header[0] = cmd.opcode;
        let address_bytes = cmd.address_width.bytes() as usize;
        if let Some(address) = cmd.address {
            cmd.address_width.encode(address, &mut header[1..]);
        }
        Ok(PreparedCommand {
            write: header[..1 + address_bytes].to_vec(),
            receive_len: dummy_bytes + cmd.read_buf.len(),
            discard: dummy_bytes,
        })
    } else {
        let len = cmd.header_len() + cmd.write_data.len();
        if len > SPI_TRANSFER_SIZE + 4 {
            return Err(CoreError::SpiTransferFailed);
        }
        let mut write = vec![0; len];
        let header_len = cmd.encode_header(&mut write);
        write[header_len..].copy_from_slice(cmd.write_data);
        Ok(PreparedCommand {
            write,
            receive_len: 0,
            discard: 0,
        })
    }
}
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
pub struct AtiSpiController {
    bar: rflasher_pci::MappedBar,
    spi_type: AtiSpiType,
    saved: Option<SavedState>,
    // Declared last: hold the claim through register restore and BAR unmapping.
    _claim: DeviceClaim,
}

impl AtiSpiController {
    /// Create and initialize a new ATI SPI controller.
    ///
    /// Maps the GPU MMIO BAR, saves the current register state, and enables
    /// SPI access. The device must be unbound with driver_override set to
    /// "none"; a per-device advisory lock excludes other controller instances.
    /// Identity and BAR selection are read from the actual PCI function, not
    /// supplied by callers. See `docs/ati-spi.md` for preparation and recovery.
    pub fn new(address: rflasher_pci::PciAddress) -> Result<Self, AtiSpiError> {
        let pci = rflasher_pci::SysfsPci::system();
        let claim = DeviceClaim::acquire(
            &std::path::Path::new("/sys/bus/pci/devices").join(address.to_string()),
        )?;
        let vendor_id = pci.read16(address, 0)?;
        let device_id = pci.read16(address, 2)?;
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

        log::warn!(
            "ATI SPI is experimental; historical flashrom test status ({:?}) does not validate this Rust port",
            dev.status
        );
        let bar_index = (dev.spi_type.io_bar() - 0x10) / 4;
        let bar = pci.map_bar(address, bar_index, 0x7000)?;

        let mut ctrl = Self {
            bar,
            spi_type: dev.spi_type,
            saved: None,
            _claim: claim,
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
    ///
    /// Read prefixes may contain at most four bytes. Raw callers must include
    /// dummy clocks in the receive length and discard those bytes themselves.
    pub fn send_command(&mut self, writearr: &[u8], readarr: &mut [u8]) -> Result<(), AtiSpiError> {
        if writearr.is_empty() {
            return Err(AtiSpiError::Io("SPI command must have at least 1 byte"));
        }
        if !readarr.is_empty() && writearr.len() > 4 {
            return Err(AtiSpiError::UnsupportedCommand(
                "read command prefix exceeds the four-byte command register",
            ));
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

    /// Maximum SPI transfer size for this controller.
    ///
    /// Both families share the same 256-byte hardware FIFO window.
    pub fn max_transfer_size(&self) -> usize {
        SPI_TRANSFER_SIZE
    }

    /// Get the SPI type / GPU family
    pub fn spi_type(&self) -> AtiSpiType {
        self.spi_type
    }
}

impl Drop for AtiSpiController {
    fn drop(&mut self) {
        self.restore();
    }
}

// ============================================================================
// SpiMaster trait implementation
// ============================================================================

impl SpiMaster for AtiSpiController {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::empty()
    }

    fn max_read_len(&self) -> usize {
        MAX_READ_SIZE
    }

    fn max_write_len(&self) -> usize {
        self.max_transfer_size()
    }

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        let prepared = prepare_command(cmd)?;
        let mut receive = [0; SPI_TRANSFER_SIZE];
        self.send_command(&prepared.write, &mut receive[..prepared.receive_len])
            .map_err(map_ati_error)?;
        cmd.read_buf
            .copy_from_slice(&receive[prepared.discard..prepared.receive_len]);
        Ok(())
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
        AtiSpiError::UnsupportedCommand(_) => CoreError::OpcodeNotSupported,
        AtiSpiError::Io(_) => CoreError::IoError,
        AtiSpiError::SpiInit(_) => CoreError::ProgrammerError,
        _ => CoreError::ProgrammerError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_framing_keeps_dummy_clocks_in_the_receive_phase() {
        let mut buf = [0; MAX_READ_SIZE];
        let sfdp = SpiCommand::read_3b(0x5a, 0x123456, &mut buf).with_dummy_cycles(8);
        let prepared = prepare_command(&sfdp).unwrap();
        assert_eq!(prepared.write, [0x5a, 0x12, 0x34, 0x56]);
        assert_eq!(prepared.discard, 1);
        assert_eq!(prepared.receive_len, MAX_READ_SIZE + 1);

        let uid = SpiCommand::read_reg(0x4b, &mut buf[..8]).with_dummy_cycles(32);
        let prepared = prepare_command(&uid).unwrap();
        assert_eq!(prepared.write, [0x4b]);
        assert_eq!((prepared.discard, prepared.receive_len), (4, 12));
        let long_dummy = SpiCommand::read_3b(0x0b, 0, &mut buf).with_dummy_cycles(248);
        assert!(prepare_command(&long_dummy).unwrap().receive_len <= SPI_TRANSFER_SIZE);
    }

    #[test]
    fn rejects_unrepresentable_frames_and_keeps_a_full_page_write() {
        let mut buf = [0; SPI_TRANSFER_SIZE];
        assert!(matches!(
            prepare_command(&SpiCommand::read_4b(0x13, 0, &mut buf[..8])),
            Err(CoreError::OpcodeNotSupported)
        ));
        assert!(matches!(
            prepare_command(&SpiCommand::erase_4b(0xdc, 0)),
            Err(CoreError::OpcodeNotSupported)
        ));
        assert!(matches!(
            prepare_command(&SpiCommand::write_4b(0x12, 0, &[1])),
            Err(CoreError::OpcodeNotSupported)
        ));
        assert!(prepare_command(&SpiCommand::read_3b(3, 0x1000000, &mut buf[..8])).is_err());
        assert!(
            prepare_command(&SpiCommand::read_reg(0x4b, &mut buf[..8]).with_dummy_cycles(7))
                .is_err()
        );
        assert!(prepare_command(&SpiCommand::read_reg(0x9f, &mut buf)).is_err());
        assert!(
            prepare_command(
                &SpiCommand::read_3b(0x3b, 0, &mut buf[..8])
                    .with_io_mode(rflasher_core::spi::IoMode::DualOut)
            )
            .is_err()
        );
        let mut duplex = SpiCommand::read_reg(0x9f, &mut buf[..8]);
        duplex.write_data = &[1];
        assert!(prepare_command(&duplex).is_err());
        let mut missing_address = SpiCommand::simple(3);
        missing_address.address_width = AddressWidth::ThreeByte;
        assert!(prepare_command(&missing_address).is_err());

        let prepared = prepare_command(&SpiCommand::write_3b(2, 0x123456, &[0xa5; 256])).unwrap();
        assert_eq!(&prepared.write[..4], &[2, 0x12, 0x34, 0x56]);
        assert_eq!(prepared.write.len(), 260);
        assert_eq!(prepared.receive_len, 0);
    }

    fn mock_controller(spi_type: AtiSpiType) -> (tempfile::TempDir, AtiSpiController) {
        let dir = tempfile::tempdir().unwrap();
        let address = rflasher_pci::PciAddress::new(0, 3, 0, 0);
        let path = dir.path().join(address.to_string());
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("config"), []).unwrap();
        std::fs::write(path.join("driver_override"), "none\n").unwrap();
        let file = std::fs::File::create(path.join("resource2")).unwrap();
        file.set_len(0x7000).unwrap();
        let bar = rflasher_pci::SysfsPci::new(dir.path())
            .map_bar(address, 2, 0x7000)
            .unwrap();
        let claim = DeviceClaim::acquire(&path).unwrap();
        (
            dir,
            AtiSpiController {
                bar,
                spi_type,
                saved: None,
                _claim: claim,
            },
        )
    }

    #[test]
    fn r600_read_discards_dummy_data_and_program_uses_entire_fifo() {
        let (_dir, mut ctrl) = mock_controller(AtiSpiType::R600);
        ctrl.r600().sw_status.set(1);
        ctrl.r600().sw_data[0].set(u32::from_le_bytes([0xaa, b'S', b'F', b'D']));
        ctrl.r600().sw_data[1].set(u32::from_le_bytes([b'P', 0, 0, 0]));
        let mut buf = [0; 4];
        futures_lite::future::block_on(
            ctrl.execute(&mut SpiCommand::read_3b(0x5a, 0, &mut buf).with_dummy_cycles(8)),
        )
        .unwrap();
        assert_eq!(&buf, b"SFDP");
        assert_eq!(ctrl.r600().sw_command.get(), 0x5a);
        assert_eq!(
            ctrl.r600().sw_control.read(SoftwareControl::COMMAND_BYTES),
            3
        );
        assert_eq!(ctrl.r600().sw_control.read(SoftwareControl::DATA_BYTES), 5);
        assert!(ctrl.r600().sw_control.is_set(SoftwareControl::READ));

        ctrl.r600().sw_status.set(1);
        let page: Vec<u8> = (0..=255).collect();
        futures_lite::future::block_on(ctrl.execute(&mut SpiCommand::write_3b(2, 0x123456, &page)))
            .unwrap();
        assert_eq!(ctrl.r600().sw_command.get(), 0x12345602);
        assert_eq!(ctrl.r600().sw_data[0].get(), 0x00010203);
        assert_eq!(ctrl.r600().sw_data[63].get(), 0xfcfdfeff);
        assert_eq!(
            ctrl.r600().sw_control.read(SoftwareControl::DATA_BYTES),
            256
        );
        assert!(!ctrl.r600().sw_control.is_set(SoftwareControl::READ));
    }

    #[test]
    fn invalid_raw_reads_fail_before_touching_either_register_interface() {
        for spi_type in [AtiSpiType::R600, AtiSpiType::Bonaire] {
            let (_dir, mut ctrl) = mock_controller(spi_type);
            assert!(
                ctrl.send_command(&[0x5a, 0, 0, 0, 0xff], &mut [0; 8])
                    .is_err()
            );
            assert!(ctrl.send_command(&[3], &mut [0; 257]).is_err());
            assert!(ctrl.send_command(&[], &mut []).is_err());
            assert!(ctrl.send_command(&[0; 261], &mut []).is_err());
            assert_eq!(ctrl.r600().sw_command.get(), 0);
            assert_eq!(ctrl.ci().smc_index.get(), 0);
        }
    }

    /// GPIO pad mux bits must match the masks upstream ati_spi.c drives
    /// (0x700 for GPIO7-9, 0x80000 legacy bit); a shifted field would wedge
    /// the SPI mux on real GPUs. tock stores the unshifted mask + shift.
    #[test]
    fn gpio_pad_field_positions_match_upstream_masks() {
        assert_eq!((GpioPad::SPI.mask, GpioPad::SPI.shift), (0x7, 8)); // 0x700
        assert_eq!((GpioPad::LEGACY.mask, GpioPad::LEGACY.shift), (0x1, 19)); // 0x80000
        assert_eq!((GpioPad::EXTRA.mask, GpioPad::EXTRA.shift), (0x1, 30)); // 0x40000000
    }
}

//! Opening the internal programmer
//!
//! [`InternalProgrammer::open`] detects the chipset and returns either raw
//! SPI access ([`InternalSpi`], Intel software sequencing, AMD SPI100 and
//! the AMD SB600-family controller) or
//! address-based access ([`HwseqFlash`], Intel hardware sequencing).
//! Embedded users wrap a controller themselves instead: in `SpiFlashDevice`
//! for raw SPI or in [`HwseqFlash`]; see the crate README.

use crate::amd_sb600spi::Sb600SpiSpeed;
use crate::error::InternalError;
use crate::ichspi::SpiMode;

/// Options for the internal programmer
#[derive(Debug, Clone, Default)]
pub struct InternalOptions {
    /// SPI sequencing mode (auto, hwseq, swseq)
    pub mode: SpiMode,
    /// AMD SB600-family command engine clock; `None` is flashprog's
    /// default, 16.5 MHz.
    pub spispeed: Option<Sb600SpiSpeed>,
    /// AMD SB7xx/SP5100: the operator asserts that the IMC is strapped off,
    /// which cannot be read through PCI configuration space.
    pub sb7xx_imc_off: bool,
}

impl InternalOptions {
    /// Create options with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the SPI mode
    pub fn with_mode(mut self, mode: SpiMode) -> Self {
        self.mode = mode;
        self
    }

    /// Parse options from key-value pairs (from CLI)
    ///
    /// Supported options:
    /// - ich_spi_mode=auto|hwseq|swseq
    /// - spispeed=33mhz|22mhz|16.5mhz (AMD SB600 family)
    /// - sb7xx_imc=off (AMD SB7xx/SP5100: assert the IMC is strapped off)
    pub fn from_options(options: &[(&str, &str)]) -> Result<Self, InternalError> {
        let mut opts = Self::default();

        for (key, value) in options {
            match *key {
                "ich_spi_mode" | "mode" => {
                    opts.mode = SpiMode::parse(value).ok_or(InternalError::NotSupported(
                        "Invalid ich_spi_mode value (use: auto, hwseq, or swseq)",
                    ))?;
                }
                "spispeed" => {
                    opts.spispeed = Some(Sb600SpiSpeed::parse(value).ok_or(
                        InternalError::NotSupported(
                            "Invalid spispeed value (use: 33mhz, 22mhz or 16.5mhz)",
                        ),
                    )?);
                }
                "sb7xx_imc" => {
                    if !value.eq_ignore_ascii_case("off") {
                        return Err(InternalError::NotSupported(
                            "Invalid sb7xx_imc value (use: off)",
                        ));
                    }
                    opts.sb7xx_imc_off = true;
                }
                _ => {
                    return Err(InternalError::NotSupported(
                        "Unknown internal programmer option (supported: ich_spi_mode=auto|hwseq|swseq, spispeed=<speed>, sb7xx_imc=off)",
                    ));
                }
            }
        }

        Ok(opts)
    }

    /// Refuse SB600-family options for other controllers.
    #[cfg_attr(not(all(feature = "std", target_os = "linux")), allow(dead_code))]
    fn reject_sb600_options(&self) -> Result<(), InternalError> {
        if self.spispeed.is_some() || self.sb7xx_imc_off {
            return Err(InternalError::NotSupported(
                "spispeed and sb7xx_imc apply to the AMD SB600-family controller only",
            ));
        }
        Ok(())
    }
}

pub use crate::hwseq::HwseqFlash;
#[cfg(all(feature = "std", target_os = "linux"))]
pub use linux::{InternalProgrammer, InternalSpi};

#[cfg(all(feature = "std", target_os = "linux"))]
mod linux {
    use super::{HwseqFlash, InternalOptions};
    use crate::amd_enable::{enable_amd_spi100_with_host, open_amd_sb600};
    use crate::controller::Controller;
    use crate::error::InternalError;
    use crate::host::{DefaultPciAccess, LinuxHost, PciAddress};
    use crate::ichspi::{IchSpiController, SpiMode};
    use crate::{AnyDetectedChipset, DetectedAmdChipset, DetectedChipset};

    use rflasher_core::error::Result as CoreResult;
    use rflasher_core::flash::{self, FlashContext};
    use rflasher_core::programmer::{FlashAccess, SpiFeatures, SpiMaster};
    use rflasher_core::spi::SpiCommand;

    /// An opened internal programmer, in the access mode its controller allows.
    // There is one per process; boxing the controller would buy nothing.
    #[allow(clippy::large_enum_variant)]
    pub enum InternalProgrammer {
        /// Raw SPI commands: Intel software sequencing, AMD SPI100 or the
        /// AMD SB600-family controller.
        Spi(InternalSpi),
        /// Intel hardware sequencing: the controller runs the flash protocol.
        HardwareSequencing(HwseqFlash<IchSpiController<LinuxHost>>),
    }

    impl InternalProgrammer {
        /// Detect the chipset and enable its SPI controller.
        pub fn open(options: InternalOptions) -> Result<Self, InternalError> {
            match crate::detect_chipset()?.ok_or(InternalError::NoChipset)? {
                AnyDetectedChipset::Intel(chipset) => Self::from_intel_chipset(&chipset, options),
                AnyDetectedChipset::Amd(chipset) => {
                    Self::from_amd_chipset_with_options(&chipset, options)
                }
            }
        }

        /// Open a specific detected Intel chipset.
        pub fn from_intel_chipset(
            chipset: &DetectedChipset,
            options: InternalOptions,
        ) -> Result<Self, InternalError> {
            options.reject_sb600_options()?;
            let controller = IchSpiController::new(chipset, options.mode)?;
            if controller.spi_mode() == SpiMode::HardwareSequencing {
                HwseqFlash::new(controller).map(Self::HardwareSequencing)
            } else {
                Ok(Self::Spi(InternalSpi {
                    controller: Box::new(controller),
                }))
            }
        }

        /// Open a specific detected AMD chipset.
        pub fn from_amd_chipset(chipset: &DetectedAmdChipset) -> Result<Self, InternalError> {
            Self::from_amd_chipset_with_options(chipset, InternalOptions::default())
        }

        /// Open a specific detected AMD chipset with programmer options.
        pub fn from_amd_chipset_with_options(
            chipset: &DetectedAmdChipset,
            options: InternalOptions,
        ) -> Result<Self, InternalError> {
            let device = PciAddress::new(
                chipset.domain,
                chipset.bus,
                chipset.device,
                chipset.function,
            );
            if chipset.chipset_type().uses_sb600_spi() {
                let controller = open_amd_sb600(
                    chipset.enable,
                    device,
                    options.spispeed,
                    options.sb7xx_imc_off,
                )?;
                return Ok(Self::Spi(InternalSpi {
                    controller: Box::new(controller),
                }));
            }
            options.reject_sb600_options()?;
            let info = enable_amd_spi100_with_host(
                &DefaultPciAccess,
                chipset.enable,
                device,
                chipset.revision_id,
            )?;
            // SAFETY: `info` was just discovered from the current Linux host via
            // `DefaultPciAccess` and has not been modified.
            let controller = unsafe { info.create_controller()? };
            Ok(Self::Spi(InternalSpi {
                controller: Box::new(controller),
            }))
        }
    }

    /// Raw SPI access through an internal controller.
    ///
    /// # Intel software-sequencing limitations
    ///
    /// - Only classified opcodes in the OPMENU table can be executed;
    ///   unknown opcodes are not advertised or executed. 0x35 (RDSR2 or
    ///   QPI enter) only is for an identified chip documented to read
    ///   status register 2 with it, and an unlocked menu then gains it
    /// - Mutating commands enable BIOS writes first; opcode meanings must
    ///   match the connected flash
    /// - Programs and erases overlapping protected ranges, write-restricted
    ///   descriptor regions or flash below BBAR are refused. Addresses are
    ///   assumed to reach the chip unchanged (3-byte mode, extended address
    ///   register zero), and erased block sizes come from the chip metadata
    /// - Chip erase is refused while any flash range is write-restricted
    /// - Maximum 64 bytes per transfer
    /// - Only single I/O mode (no dual/quad)
    /// - No 4-byte addressing support (24-bit address max)
    /// - Dummy cycles are not supported by the Intel controller
    pub struct InternalSpi {
        pub(crate) controller: Box<dyn Controller + Send>,
    }

    impl SpiMaster for InternalSpi {
        fn features(&self) -> SpiFeatures {
            self.controller.features()
        }

        fn max_read_len(&self) -> usize {
            self.controller.max_read_len()
        }

        fn max_write_len(&self) -> usize {
            self.controller.max_write_len()
        }

        async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
            self.controller.execute(cmd)
        }

        fn probe_opcode(&self, opcode: u8) -> bool {
            self.controller.probe_opcode(opcode)
        }

        fn chip_identified(&mut self, chip: &rflasher_core::chip::FlashChip) {
            self.controller.chip_identified(chip)
        }

        fn check_access(&self, access: FlashAccess, addr: u32, len: u32) -> CoreResult<()> {
            self.controller.check_access(access, addr, len)
        }

        async fn delay_us(&mut self, us: u32) {
            std::thread::sleep(std::time::Duration::from_micros(us as u64));
        }

        async fn read_flash(
            &mut self,
            ctx: &FlashContext,
            addr: u32,
            buf: &mut [u8],
        ) -> CoreResult<()> {
            let len = u32::try_from(buf.len())
                .map_err(|_| rflasher_core::error::Error::AddressOutOfBounds)?;
            self.controller.check_access(FlashAccess::Read, addr, len)?;
            match self.controller.fast_read(ctx, addr, buf) {
                Some(result) => result,
                None => flash::spi_read(self, ctx, addr, buf).await,
            }
        }
    }
}

/// Programmer information
pub fn programmer_info() -> rflasher_core::programmer::ProgrammerInfo {
    rflasher_core::programmer::ProgrammerInfo {
        name: "internal",
        aliases: &[],
        description: "Intel ICH/PCH, AMD SB600-family and SPI100 internal flash programmer",
        requires_root: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "std", target_os = "linux"))]
    #[test]
    fn amd_reads_use_the_mapping_and_large_chip_writes_use_native_spi() {
        use crate::amd_spi100::tests::{context, mapped_controller};
        use futures_lite::future::block_on;
        use rflasher_core::flash::{FlashDevice, SpiFlashDevice};

        let programmer = InternalSpi {
            controller: Box::new(mapped_controller()),
        };
        let ctx = context();
        let addr = ctx.chip.total_size - 256;
        let mut device = SpiFlashDevice::new(programmer, ctx);
        let mut data = [0; 4];
        block_on(device.read(addr, &mut data)).unwrap();
        assert_eq!(data, [0, 17, 34, 51]);
        block_on(device.write(0x0100_1234, &[0xab])).unwrap();
    }

    #[test]
    fn sb600_options_parse_only_documented_values() {
        let opts =
            InternalOptions::from_options(&[("spispeed", "33mhz"), ("sb7xx_imc", "off")]).unwrap();
        assert_eq!(opts.spispeed, Some(Sb600SpiSpeed::Mhz33));
        assert!(opts.sb7xx_imc_off);
        for (key, value) in [
            ("spispeed", "66mhz"),
            ("spispeed", "100mhz"),
            ("sb7xx_imc", "on"),
        ] {
            assert!(InternalOptions::from_options(&[(key, value)]).is_err());
        }
        assert!(opts.reject_sb600_options().is_err());
        assert!(InternalOptions::default().reject_sb600_options().is_ok());
    }
}

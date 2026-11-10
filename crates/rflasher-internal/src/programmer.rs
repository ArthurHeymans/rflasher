//! Opening the internal programmer
//!
//! [`InternalProgrammer::open`] detects the chipset and returns either raw
//! SPI access ([`InternalSpi`], Intel software sequencing and AMD SPI100) or
//! address-based access ([`HwseqFlash`], Intel hardware sequencing).
//! Embedded users wrap a controller themselves instead: in `SpiFlashDevice`
//! for raw SPI or in [`HwseqFlash`]; see the crate README.

use crate::error::InternalError;
use crate::ichspi::SpiMode;

/// Options for the internal programmer
#[derive(Debug, Clone, Default)]
pub struct InternalOptions {
    /// SPI sequencing mode (auto, hwseq, swseq)
    pub mode: SpiMode,
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
    pub fn from_options(options: &[(&str, &str)]) -> Result<Self, InternalError> {
        let mut opts = Self::default();

        for (key, value) in options {
            match *key {
                "ich_spi_mode" | "mode" => {
                    opts.mode = SpiMode::parse(value).ok_or(InternalError::NotSupported(
                        "Invalid ich_spi_mode value (use: auto, hwseq, or swseq)",
                    ))?;
                }
                _ => {
                    return Err(InternalError::NotSupported(
                        "Unknown internal programmer option (supported: ich_spi_mode=auto|hwseq|swseq)",
                    ));
                }
            }
        }

        Ok(opts)
    }
}

pub use crate::hwseq::HwseqFlash;
#[cfg(all(feature = "std", target_os = "linux"))]
pub use linux::{InternalProgrammer, InternalSpi};

#[cfg(all(feature = "std", target_os = "linux"))]
mod linux {
    use super::{HwseqFlash, InternalOptions};
    use crate::amd_enable::enable_amd_spi100_with_host;
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
        /// Raw SPI commands: Intel software sequencing or AMD SPI100.
        Spi(InternalSpi),
        /// Intel hardware sequencing: the controller runs the flash protocol.
        HardwareSequencing(HwseqFlash<IchSpiController<LinuxHost>>),
    }

    impl InternalProgrammer {
        /// Detect the chipset and enable its SPI controller.
        pub fn open(options: InternalOptions) -> Result<Self, InternalError> {
            match crate::detect_chipset()?.ok_or(InternalError::NoChipset)? {
                AnyDetectedChipset::Intel(chipset) => Self::from_intel_chipset(&chipset, options),
                AnyDetectedChipset::Amd(chipset) => Self::from_amd_chipset(&chipset),
            }
        }

        /// Open a specific detected Intel chipset.
        pub fn from_intel_chipset(
            chipset: &DetectedChipset,
            options: InternalOptions,
        ) -> Result<Self, InternalError> {
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
            let info = enable_amd_spi100_with_host(
                &DefaultPciAccess,
                chipset.enable,
                PciAddress::new(
                    chipset.domain,
                    chipset.bus,
                    chipset.device,
                    chipset.function,
                ),
                chipset.revision_id,
            )?;
            Ok(Self::Spi(InternalSpi {
                controller: Box::new(info.create_controller()?),
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
        description: "Intel ICH/PCH and AMD SPI100 internal flash programmer",
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
    fn test_programmer_info() {
        let info = programmer_info();
        assert_eq!(info.name, "internal");
        assert!(info.requires_root);
    }
}

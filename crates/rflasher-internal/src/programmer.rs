//! Opening the internal programmer
//!
//! [`InternalProgrammer::open`] detects the chipset and returns either raw
//! SPI access ([`InternalSpi`], Intel software sequencing and AMD SPI100) or
//! address-based access ([`HwseqFlash`], Intel hardware sequencing).
//! Embedded users can drive the controllers directly instead; see the
//! crate README.

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

#[cfg(all(feature = "std", target_os = "linux"))]
pub use linux::{HwseqFlash, InternalProgrammer, InternalSpi};

#[cfg(all(feature = "std", target_os = "linux"))]
mod linux {
    use super::InternalOptions;
    use crate::amd_enable::enable_amd_spi100_with_host;
    use crate::controller::Controller;
    use crate::error::InternalError;
    use crate::host::{DefaultPciAccess, PciAddress};
    use crate::ichspi::{IchSpiController, SpiMode};
    use crate::{AnyDetectedChipset, DetectedAmdChipset, DetectedChipset};

    use rflasher_core::chip::{EraseBlock, WriteGranularity};
    use rflasher_core::error::{Error as CoreError, Result as CoreResult};
    use rflasher_core::flash::{self, FlashContext, FlashDevice};
    use rflasher_core::programmer::{SpiFeatures, SpiMaster};
    use rflasher_core::spi::{SpiCommand, opcodes};

    /// An opened internal programmer, in the access mode its controller allows.
    pub enum InternalProgrammer {
        /// Raw SPI commands: Intel software sequencing or AMD SPI100.
        Spi(InternalSpi),
        /// Intel hardware sequencing: the controller runs the flash protocol.
        HardwareSequencing(HwseqFlash),
    }

    impl InternalProgrammer {
        /// Detect the chipset, enable its SPI controller and BIOS writes.
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
            let mut controller = IchSpiController::new(chipset, options.mode)?;
            if let Err(e) = controller.enable_bios_write() {
                log::warn!("Could not enable BIOS writes: {}", e);
            }
            if controller.spi_mode() == SpiMode::HardwareSequencing {
                HwseqFlash::new(Box::new(controller)).map(Self::HardwareSequencing)
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
    /// - Only opcodes in the OPMENU table can be executed
    /// - Maximum 64 bytes per transfer
    /// - Only single I/O mode (no dual/quad)
    /// - No 4-byte addressing support (24-bit address max)
    /// - Dummy cycles are not supported by the Intel controller
    pub struct InternalSpi {
        pub(super) controller: Box<dyn Controller + Send>,
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

    /// Flash behind Intel hardware sequencing.
    ///
    /// The size comes from the flash descriptor and the erase block size from
    /// the controller, which always erases whole hardware blocks.
    pub struct HwseqFlash {
        controller: Box<dyn Controller + Send>,
        size: u32,
        erase_blocks: [EraseBlock; 1],
    }

    impl HwseqFlash {
        fn new(mut controller: Box<dyn Controller + Send>) -> Result<Self, InternalError> {
            let mut header = [0; 4096];
            controller
                .controller_read(0, &mut header, 0)
                .map_err(|_| InternalError::Io("Cannot read the flash descriptor"))?;
            let size = rflasher_core::layout::parse_ifd(&header)
                .ok()
                .and_then(|layout| layout.regions.iter().map(|r| r.end + 1).max())
                .filter(|&size| size > 0)
                .ok_or(InternalError::InvalidDescriptor)?;
            log::info!("Flash size: {} bytes (from IFD)", size);
            Self::with_size(controller, size)
        }

        pub(crate) fn with_size(
            controller: Box<dyn Controller + Send>,
            size: u32,
        ) -> Result<Self, InternalError> {
            let block = controller.erase_block_size(size)?.unwrap_or(4096);
            log::info!("Hardware sequencing erase block size: {} bytes", block);
            Ok(Self {
                controller,
                size,
                // The opcode is never sent; the controller picks its own.
                erase_blocks: [EraseBlock::with_count(opcodes::SE_20, block, size / block)],
            })
        }

        fn check_writable(&self, addr: u32, len: usize) -> CoreResult<()> {
            if !self.is_valid_range(addr, len) {
                Err(CoreError::AddressOutOfBounds)
            } else if !self.controller.writes_enabled() {
                Err(CoreError::WriteProtected)
            } else {
                Ok(())
            }
        }
    }

    impl FlashDevice for HwseqFlash {
        fn size(&self) -> u32 {
            self.size
        }

        fn erase_granularity(&self) -> u32 {
            self.erase_blocks[0].min_block_size()
        }

        fn write_granularity(&self) -> WriteGranularity {
            WriteGranularity::Bit
        }

        fn erase_blocks(&self) -> &[EraseBlock] {
            &self.erase_blocks
        }

        async fn read(&mut self, addr: u32, buf: &mut [u8]) -> CoreResult<()> {
            if !self.is_valid_range(addr, buf.len()) {
                return Err(CoreError::AddressOutOfBounds);
            }
            self.controller
                .controller_read(addr, buf, self.size as usize)
        }

        async fn write(&mut self, addr: u32, data: &[u8]) -> CoreResult<()> {
            self.check_writable(addr, data.len())?;
            self.controller.controller_write(addr, data)
        }

        async fn erase(&mut self, addr: u32, len: u32) -> CoreResult<()> {
            self.check_writable(addr, len as usize)?;
            self.controller.controller_erase(addr, len)
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

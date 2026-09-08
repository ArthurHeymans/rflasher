//! Stateful regressions shared by the SPI and hybrid session adapters.
use super::*;
use crate::chip::{EraseBlock, Features, FlashChip, QeMethod, WriteGranularity};
use crate::error::{Error, Result};
use crate::programmer::{OpaqueMaster, SpiFeatures, SpiMaster};
use crate::protocol::{CommandAddressing, SpiReadOp};
use crate::spi::{AddressWidth, IoMode, SpiCommand, opcodes};
use crate::wp::{WpBits, WpConfig, WpMode, WpRange, WriteOptions};
use alloc::{vec, vec::Vec};

#[derive(Default)]
struct SessionMaster {
    sr: [u8; 3],
    nv: [u8; 3],
    volatile_write: bool,
    legacy_ewsr: bool,
    persistent_writes: usize,
    status_writes: usize,
    fail_after_write: Option<u8>,
    pending_failure: Option<u8>,
    reject_restore: bool,
    fail_persistent_write: bool,
    fail_enter: bool,
    four_byte: bool,
    high_address: bool,
    op: Option<SpiReadOp>,
    commands: Vec<(u8, Option<u32>, IoMode)>,
}

impl SessionMaster {
    fn power_cycle(&mut self) {
        self.sr = self.nv;
        self.four_byte = false;
    }
    fn check_read(&self, mode: IoMode) {
        assert!(
            !mode.requires_quad() || self.sr[1] & 2 != 0,
            "quad with QE clear"
        );
    }
}

impl SpiMaster for SessionMaster {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::FOUR_BYTE_ADDR | SpiFeatures::QUAD_IO | SpiFeatures::QUAD_IN
    }
    fn max_read_len(&self) -> usize {
        4096
    }
    fn max_write_len(&self) -> usize {
        256
    }
    async fn delay_us(&mut self, _: u32) {}
    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
        self.commands.push((cmd.opcode, cmd.address, cmd.io_mode));
        if self.pending_failure == Some(cmd.opcode) {
            self.pending_failure = None;
            return Err(Error::ProgrammerError);
        }
        if cmd.address.is_some() && self.high_address {
            assert!(self.four_byte, "addressed command after accidental EX4B");
            assert_eq!(cmd.address_width, AddressWidth::FourByte);
        }
        match cmd.opcode {
            opcodes::RDSR => cmd.read_buf[0] = self.sr[0],
            opcodes::RDSR2 => cmd.read_buf[0] = self.sr[1],
            opcodes::RDSR3 => cmd.read_buf[0] = self.sr[2],
            opcodes::EWSR => self.volatile_write = !self.legacy_ewsr,
            opcodes::WREN => self.volatile_write = false,
            opcodes::WRSR | opcodes::WRSR2 => {
                let first = usize::from(cmd.opcode == opcodes::WRSR2);
                if !self.volatile_write {
                    self.persistent_writes += 1;
                    if self.fail_persistent_write {
                        return Err(Error::ProgrammerError);
                    }
                }
                self.status_writes += 1;
                if !(self.reject_restore
                    && self.volatile_write
                    && cmd.write_data.last().is_some_and(|b| b & 2 == 0))
                {
                    for (i, &byte) in cmd.write_data.iter().enumerate() {
                        self.sr[first + i] = byte;
                        if !self.volatile_write {
                            self.nv[first + i] = byte;
                        }
                    }
                }
                self.pending_failure = self.fail_after_write.take();
            }
            opcodes::EN4B => {
                if self.fail_enter {
                    return Err(Error::ProgrammerError);
                }
                self.four_byte = true;
            }
            opcodes::EX4B => self.four_byte = false,
            _ => {
                if !cmd.read_buf.is_empty() {
                    self.check_read(cmd.io_mode);
                    cmd.read_buf.fill(0xff);
                }
            }
        }
        Ok(())
    }
}

impl OpaqueMaster for SessionMaster {
    fn size(&self) -> usize {
        32 * 1024 * 1024
    }
    fn set_read_op(&mut self, op: SpiReadOp, _: Features, _: CommandAddressing) {
        self.op = Some(op);
    }
    async fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        let op = self.op.expect("read op pushed before opaque I/O");
        self.check_read(op.io_mode);
        if self.high_address {
            assert!(self.four_byte);
        }
        self.commands.push((op.opcode, Some(addr), op.io_mode));
        buf.fill(0xff);
        Ok(())
    }
    async fn write(&mut self, addr: u32, _: &[u8]) -> Result<()> {
        if self.high_address {
            assert!(self.four_byte);
        }
        self.commands
            .push((opcodes::PP, Some(addr), IoMode::Single));
        Ok(())
    }
    async fn erase(&mut self, _: u32, _: u32) -> Result<()> {
        Err(Error::ProgrammerError)
    }
}

fn chip() -> FlashChip {
    FlashChip {
        vendor: "Test".into(),
        name: "Session".into(),
        jedec_manufacturer: 0xef,
        jedec_device: 0x4018,
        total_size: 16 * 1024 * 1024,
        page_size: 256,
        features: Features::FAST_READ
            | Features::FAST_READ_QIO
            | Features::WRSR_WREN
            | Features::WRSR_EWSR,
        voltage_min_mv: 2700,
        voltage_max_mv: 3600,
        write_granularity: WriteGranularity::Page,
        erase_blocks: vec![EraseBlock::with_count(0x20, 4096, 8192)],
        tested: Default::default(),
        qe_method: QeMethod::Sr2Bit1WriteSr2,
        dummy_cycles_112: 0,
        dummy_cycles_122: 0,
        dummy_cycles_114: 0,
        dummy_cycles_144: 0,
        dummy_cycles_qpi: 0,
    }
}

macro_rules! adapter_tests {
    ($module:ident, $adapter:ident) => {
        mod $module {
            use super::*;
            type Device = $adapter<SessionMaster>;
            fn device() -> Device {
                Device::new(SessionMaster::default(), FlashContext::new(chip()))
            }

            #[test]
            fn repeated_prepare_retains_qe_ownership() {
                futures_lite::future::block_on(async {
                    let mut dev = device();
                    dev.prepare().await.unwrap();
                    dev.prepare().await.unwrap();
                    assert_eq!(dev.master().status_writes, 1);
                    dev.finish().await.unwrap();
                    assert_eq!(dev.master().sr[1] & 2, 0);
                });
            }

            #[test]
            fn wp_is_persistent_without_capturing_temporary_qe() {
                futures_lite::future::block_on(async {
                    for preexisting_qe in [false, true] {
                        let mut dev = device();
                        dev.master().sr = [0x1c, u8::from(preexisting_qe) * 2, 0];
                        dev.master().nv = dev.master().sr;
                        dev.prepare().await.unwrap();
                        dev.disable_wp(WriteOptions::default()).await.unwrap();
                        assert!(!dev.prepared().read_op.io_mode.requires_quad());
                        FlashDevice::read(&mut dev, 0, &mut [0; 8]).await.unwrap();
                        dev.finish().await.unwrap();
                        dev.master().power_cycle();
                        assert_eq!(dev.master().sr[0] & 0x1c, 0);
                        assert_eq!(dev.master().sr[1] & 2, u8::from(preexisting_qe) * 2);
                        assert!(dev.master().persistent_writes > 0);
                    }
                });
            }

            #[test]
            fn all_wp_setters_abort_if_qe_restoration_is_unconfirmed() {
                futures_lite::future::block_on(async {
                    for setter in 0..5 {
                        let mut dev = device();
                        dev.prepare().await.unwrap();
                        dev.master().reject_restore = true;
                        let options = WriteOptions::default();
                        let result = match setter {
                            0 => dev.disable_wp(options).await,
                            1 => dev.set_wp_mode(WpMode::Disabled, options).await,
                            2 => dev.set_wp_range(&WpRange::none(), options).await,
                            3 => {
                                dev.write_wp_config(
                                    &WpConfig::new(WpMode::Disabled, WpRange::none()),
                                    options,
                                )
                                .await
                            }
                            _ => dev.write_wp_bits(&WpBits::default(), options).await,
                        };
                        assert!(result.is_err());
                        assert_eq!(dev.master().persistent_writes, 0);
                        assert!(!dev.prepared().read_op.io_mode.requires_quad());
                        if let Some(op) = dev.master().op {
                            assert!(!op.io_mode.requires_quad());
                        }
                        assert!(dev.prepared().volatile_qe_enabled);
                        dev.master().reject_restore = false;
                        FlashDevice::read(&mut dev, 0, &mut [0; 8]).await.unwrap();
                        dev.finish().await.unwrap();
                        assert_eq!(dev.master().sr[1] & 2, 0);
                    }
                });
            }

            #[test]
            fn failed_wp_write_reprepares_before_flash_io() {
                futures_lite::future::block_on(async {
                    let mut dev = device();
                    dev.prepare().await.unwrap();
                    dev.master().fail_persistent_write = true;
                    assert!(dev.disable_wp(WriteOptions::default()).await.is_err());
                    assert_eq!(dev.master().sr[1] & 2, 0);
                    FlashDevice::read(&mut dev, 0, &mut [0; 8]).await.unwrap();
                    assert!(dev.prepared().read_op.io_mode.requires_quad());
                    dev.finish().await.unwrap();
                });
            }

            #[test]
            fn accepted_qe_write_with_failed_poll_or_confirmation_is_restored() {
                futures_lite::future::block_on(async {
                    for failure in [opcodes::RDSR, opcodes::RDSR2] {
                        let mut dev = device();
                        dev.master().fail_after_write = Some(failure);
                        dev.prepare().await.unwrap();
                        assert!(dev.prepared().volatile_qe_enabled);
                        assert!(!dev.prepared().read_op.io_mode.requires_quad());
                        assert_eq!(dev.master().sr[1] & 2, 2);
                        dev.finish().await.unwrap();
                        assert_eq!(dev.master().sr[1] & 2, 0);
                    }
                });
            }

            #[test]
            fn restoration_poll_failure_blocks_wp_and_keeps_retry_obligation() {
                futures_lite::future::block_on(async {
                    let mut dev = device();
                    dev.prepare().await.unwrap();
                    dev.master().fail_after_write = Some(opcodes::RDSR);
                    assert!(dev.disable_wp(WriteOptions::default()).await.is_err());
                    assert_eq!(dev.master().persistent_writes, 0);
                    assert!(dev.prepared().volatile_qe_enabled);
                    assert_eq!(dev.master().sr[1] & 2, 0);
                    dev.finish().await.unwrap();
                    assert!(!dev.prepared().volatile_qe_enabled);
                });
            }

            #[test]
            fn failed_prepare_rolls_back_qe_and_can_retry() {
                futures_lite::future::block_on(async {
                    let mut c = chip();
                    c.total_size = 32 * 1024 * 1024;
                    c.features |= Features::FOUR_BYTE_ENTER;
                    let mut dev = Device::new(
                        SessionMaster {
                            fail_enter: true,
                            ..Default::default()
                        },
                        FlashContext::new(c),
                    );
                    assert!(dev.prepare().await.is_err());
                    assert_eq!(dev.master().sr[1] & 2, 0);
                    dev.master().fail_enter = false;
                    dev.prepare().await.unwrap();
                    dev.finish().await.unwrap();
                });
            }

            #[test]
            fn legacy_ewsr_only_wp_survives_power_cycle() {
                futures_lite::future::block_on(async {
                    let mut c = chip();
                    c.features = Features::WRSR_EWSR;
                    let mut dev = Device::new(
                        SessionMaster {
                            legacy_ewsr: true,
                            sr: [0x1c, 0, 0],
                            nv: [0x1c, 0, 0],
                            ..Default::default()
                        },
                        FlashContext::new(c),
                    );
                    dev.disable_wp(WriteOptions::default()).await.unwrap();
                    assert!(dev.master().commands.iter().any(|c| c.0 == opcodes::EWSR));
                    assert!(!dev.master().commands.iter().any(|c| c.0 == opcodes::WREN));
                    dev.master().power_cycle();
                    assert_eq!(dev.master().sr[0] & 0x1c, 0);
                });
            }

            #[test]
            fn retained_en4b_survives_successive_high_erases_and_io() {
                futures_lite::future::block_on(async {
                    let mut c = chip();
                    c.total_size = 32 * 1024 * 1024;
                    c.features = Features::FOUR_BYTE_ENTER | Features::WRSR_WREN;
                    let mut dev = Device::new(
                        SessionMaster {
                            high_address: true,
                            ..Default::default()
                        },
                        FlashContext::new(c),
                    );
                    dev.prepare().await.unwrap();
                    FlashDevice::erase(&mut dev, 0x100_0000, 4096)
                        .await
                        .unwrap();
                    FlashDevice::erase(&mut dev, 0x100_1000, 4096)
                        .await
                        .unwrap();
                    FlashDevice::read(&mut dev, 0x100_2000, &mut [0; 8])
                        .await
                        .unwrap();
                    FlashDevice::write(&mut dev, 0x100_3000, &[0x42])
                        .await
                        .unwrap();
                    dev.disable_wp(WriteOptions::default()).await.unwrap();
                    FlashDevice::read(&mut dev, 0x100_4000, &mut [0; 8])
                        .await
                        .unwrap();
                    dev.finish().await.unwrap();
                    let master = dev.master();
                    assert!(master.four_byte);
                    assert_eq!(
                        master
                            .commands
                            .iter()
                            .filter(|c| c.0 == opcodes::EN4B)
                            .count(),
                        1
                    );
                    assert!(!master.commands.iter().any(|c| c.0 == opcodes::EX4B));
                    for addr in [0x100_0000, 0x100_1000, 0x100_2000, 0x100_3000, 0x100_4000] {
                        assert!(master.commands.iter().any(|c| c.1 == Some(addr)));
                    }
                });
            }
        }
    };
}
adapter_tests!(spi_device, SpiFlashDevice);
adapter_tests!(hybrid_device, HybridFlashDevice);

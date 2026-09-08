//! Stateful behavioral coverage of the shared operation owner, through both adapters.
use super::io::{self, AddressPlan, ErasePlan, ReadPlan, WritePlan};
use super::*;
use crate::chip::{EraseBlock, Features, FlashChip, QeMethod, WriteGranularity};
use crate::error::{Error, Result};
use crate::programmer::{OpaqueMaster, SpiFeatures, SpiMaster};
use crate::spi::{AddressWidth, IoMode, SpiCommand, opcodes};
use crate::wp::{WpBits, WpConfig, WpMode, WpRange, WriteOptions};
use alloc::{vec, vec::Vec};

#[derive(Clone, Copy)]
enum Failure {
    Error,
    Pending,
}

#[derive(Default)]
struct Master {
    sr: [u8; 3],
    nv: [u8; 3],
    volatile: bool,
    legacy: bool,
    four: bool,
    ear: u8,
    reject_restore: bool,
    fault: Option<(u8, usize, Failure)>,
    calls: Vec<u8>,
    addresses: Vec<(u8, u32)>,
    bulk_reads: usize,
    short_read: bool,
    no_bulk_read: bool,
    firmware_erase: bool,
    fail_firmware_erase: bool,
    firmware_erases: usize,
    erased: Vec<(u32, u32)>,
    busy: bool,
}
impl Master {
    async fn event(&mut self, opcode: u8) -> Result<()> {
        self.calls.push(opcode);
        if let Some((op, nth, kind)) = self.fault
            && op == opcode
            && self.calls.iter().filter(|o| **o == op).count() == nth
        {
            self.fault = None;
            match kind {
                Failure::Error => return Err(Error::SpiTransferFailed),
                Failure::Pending => core::future::pending::<()>().await,
            }
        }
        Ok(())
    }
    fn address(&mut self, opcode: u8, addr: u32, width: AddressWidth, native: bool) -> u32 {
        assert_eq!(
            width == AddressWidth::FourByte,
            native || self.four,
            "wrong command address width"
        );
        let actual = if width == AddressWidth::ThreeByte {
            (addr & 0xffffff) | u32::from(self.ear) << 24
        } else {
            addr
        };
        self.addresses.push((opcode, actual));
        actual
    }
    fn fill(&self, addr: u32, mode: IoMode, buf: &mut [u8]) {
        assert!(
            !mode.requires_quad() || self.sr[1] & 2 != 0,
            "quad before QE"
        );
        for (offset, b) in buf.iter_mut().enumerate() {
            let at = addr + offset as u32;
            *b = if self.erased.iter().any(|(a, n)| at >= *a && at < a + n) {
                0xff
            } else {
                (at ^ (at >> 8) ^ (at >> 24)) as u8
            };
        }
    }
}
impl SpiMaster for Master {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::FOUR_BYTE_ADDR | SpiFeatures::QUAD_IO | SpiFeatures::DUAL_IO
    }
    fn max_read_len(&self) -> usize {
        512
    }
    fn max_write_len(&self) -> usize {
        256
    }
    async fn delay_us(&mut self, _: u32) {}
    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
        match cmd.opcode {
            opcodes::RDSR => cmd.read_buf[0] = self.sr[0] | u8::from(self.busy),
            opcodes::RDSR2 => cmd.read_buf[0] = self.sr[1],
            opcodes::RDSR3 => cmd.read_buf[0] = self.sr[2],
            opcodes::EWSR => self.volatile = !self.legacy,
            opcodes::WREN => self.volatile = false,
            opcodes::WRSR | opcodes::WRSR2 => {
                let first = usize::from(cmd.opcode == opcodes::WRSR2);
                for (i, &b) in cmd.write_data.iter().enumerate() {
                    if !(self.reject_restore && first + i == 1 && b & 2 == 0) {
                        self.sr[first + i] = b;
                        if !self.volatile {
                            self.nv[first + i] = b;
                        }
                    }
                }
            }
            opcodes::EN4B => self.four = true,
            opcodes::EX4B => self.four = false,
            opcodes::RDEAR | opcodes::RDEAR_ALT => cmd.read_buf[0] = self.ear,
            opcodes::WREAR | opcodes::WREAR_ALT => self.ear = cmd.write_data[0],
            _ => {
                if let Some(addr) = cmd.address {
                    let native = matches!(cmd.opcode, 0x13 | 0x0c | 0xec | 0x12 | 0x21);
                    let actual = self.address(cmd.opcode, addr, cmd.address_width, native);
                    if matches!(cmd.opcode, 0x20 | 0x21) {
                        self.erased.push((actual, 4096));
                    } else if !cmd.read_buf.is_empty() {
                        self.fill(actual, cmd.io_mode, cmd.read_buf);
                    }
                }
            }
        }
        // Fail after accepting the mutation, as real transports can do.
        self.event(cmd.opcode).await
    }
}
impl OpaqueMaster for Master {
    fn size(&self) -> usize {
        32 << 20
    }
    fn supports_read_plan(&self, _: &ReadPlan) -> bool {
        !self.no_bulk_read
    }
    fn supports_write_plan(&self, _: &WritePlan) -> bool {
        true
    }
    fn supports_erase_plan(&self, _: &ErasePlan) -> bool {
        self.firmware_erase
    }
    async fn read(&mut self, _: u32, _: &mut [u8]) -> Result<()> {
        panic!("raw opaque read discarded plan")
    }
    async fn write(&mut self, _: u32, _: &[u8]) -> Result<()> {
        panic!("raw opaque write discarded plan")
    }
    async fn erase(&mut self, _: u32, _: u32) -> Result<()> {
        panic!("raw opaque erase used as capability check")
    }
    async fn read_planned(&mut self, plan: &ReadPlan, addr: u32, buf: &mut [u8]) -> Result<()> {
        for (i, chunk) in buf.chunks_mut(512).enumerate() {
            let address = addr + (i * 512) as u32;
            if let AddressPlan::Ear(f) = plan.address {
                crate::protocol::set_extended_address(self, f, (address >> 24) as u8).await?;
            }
            let actual = self.address(
                plan.op.opcode,
                address,
                plan.op.address_width,
                plan.op.native_4ba,
            );
            self.bulk_reads += 1;
            self.fill(actual, plan.op.io_mode, chunk);
            self.event(plan.op.opcode).await?;
            if self.short_read {
                return Err(Error::SpiTransferFailed);
            }
        }
        Ok(())
    }
    async fn write_planned(&mut self, plan: &WritePlan, addr: u32, data: &[u8]) -> Result<()> {
        io::write_spi(self, plan, addr, data).await
    }
    async fn erase_planned(&mut self, plan: &ErasePlan, addr: u32, len: u32) -> Result<()> {
        self.firmware_erases += 1;
        self.address(
            plan.opcode,
            addr,
            plan.address.width(),
            plan.address == AddressPlan::NativeFourByte,
        );
        self.erased.push((addr, len));
        if self.fail_firmware_erase {
            Err(Error::SpiTransferFailed)
        } else {
            Ok(())
        }
    }
}
fn chip() -> FlashChip {
    FlashChip {
        vendor: "Test".into(),
        name: "Scoped".into(),
        jedec_manufacturer: 0xef,
        jedec_device: 0x4018,
        total_size: 32 << 20,
        page_size: 256,
        features: Features::FAST_READ
            | Features::FAST_READ_QIO
            | Features::WRSR_WREN
            | Features::WRSR_EWSR
            | Features::FOUR_BYTE_ENTER,
        voltage_min_mv: 2700,
        voltage_max_mv: 3600,
        write_granularity: WriteGranularity::Page,
        erase_blocks: vec![EraseBlock::with_count(0x20, 4096, 8192)],
        tested: Default::default(),
        qe_method: QeMethod::Sr2Bit1WriteSr2,
        dummy_cycles_112: None,
        dummy_cycles_122: None,
        dummy_cycles_114: None,
        dummy_cycles_144: None,
        dummy_cycles_qpi: None,
    }
}

macro_rules! adapter_tests {
    ($module:ident, $adapter:ident) => {
        mod $module {
            use super::*;
            type Device = $adapter<Master>;
            fn device() -> Device {
                Device::new(Master::default(), FlashContext::new(chip()))
            }
            async fn assert_blocked(dev: &mut Device) {
                let before = dev.master().calls.len();
                assert_eq!(dev.read(0, &mut [0; 1]).await, Err(Error::RecoveryRequired));
                assert_eq!(dev.write(0, &[0]).await, Err(Error::RecoveryRequired));
                assert_eq!(dev.erase(0, 4096).await, Err(Error::RecoveryRequired));
                assert!(dev.read_wp_bits().await.is_err());
                assert!(dev.read_wp_config().await.is_err());
                assert!(dev.disable_wp(WriteOptions::default()).await.is_err());
                assert!(
                    dev.set_wp_mode(WpMode::Disabled, WriteOptions::default())
                        .await
                        .is_err()
                );
                assert!(
                    dev.set_wp_range(&WpRange::none(), WriteOptions::default())
                        .await
                        .is_err()
                );
                assert!(
                    dev.write_wp_bits(&WpBits::default(), WriteOptions::default())
                        .await
                        .is_err()
                );
                assert!(
                    dev.write_wp_config(&WpConfig::default(), WriteOptions::default())
                        .await
                        .is_err()
                );
                assert_eq!(
                    dev.master().calls.len(),
                    before,
                    "latched entry touched hardware"
                );
            }
            #[test]
            fn exact_read_owns_all_chunks_then_persistent_wp_preserves_initial_qe() {
                futures_lite::future::block_on(async {
                    for initial_qe in [0, 2] {
                        let mut dev = device();
                        dev.master().sr = [0x1c, initial_qe | 0x08, 0];
                        dev.master().nv = dev.master().sr;
                        let addr = 0x0100_0003;
                        let mut buf = [0; 1703];
                        dev.read(addr, &mut buf).await.unwrap();
                        for (i, b) in buf.iter().enumerate() {
                            let at = addr + i as u32;
                            assert_eq!(*b, (at ^ (at >> 8) ^ (at >> 24)) as u8);
                        }
                        assert!(!dev.master().four);
                        assert_eq!(dev.master().sr[1], initial_qe | 0x08);
                        let calls = &dev.master().calls;
                        assert_eq!(calls.iter().filter(|o| **o == opcodes::EN4B).count(), 1);
                        assert_eq!(calls.iter().filter(|o| **o == opcodes::EX4B).count(), 1);
                        assert_eq!(
                            calls.iter().filter(|o| **o == opcodes::WRSR2).count(),
                            if initial_qe == 0 { 2 } else { 0 }
                        );
                        dev.disable_wp(WriteOptions::default()).await.unwrap();
                        dev.master().sr = dev.master().nv; // power cycle
                        assert_eq!(dev.master().sr[0] & 0x1c, 0);
                        assert_eq!(dev.master().sr[1], initial_qe | 0x08);
                    }
                });
            }
            #[test]
            fn accepted_setup_writes_reach_cleanup_after_transport_poll_or_readback_failure() {
                futures_lite::future::block_on(async {
                    for (opcode, nth) in [
                        (opcodes::EN4B, 1),
                        (opcodes::RDSR2, 1),
                        (opcodes::WRSR2, 1),
                        (opcodes::RDSR, 1),
                        (opcodes::RDSR2, 3),
                        (opcodes::QIOR, 2),
                    ] {
                        let mut dev = device();
                        dev.master().fault = Some((opcode, nth, Failure::Error));
                        assert!(dev.read(0x0100_0000, &mut [0; 1200]).await.is_err());
                        if dev.recovery_required() {
                            // Hybrid transfer failures leave firmware state uncertain.
                            assert_eq!(opcode, opcodes::QIOR);
                            assert!(dev.master().four);
                            continue;
                        }
                        assert!(!dev.master().four, "failed EN4B still owned EX4B");
                        assert_eq!(
                            dev.master().sr[1] & 2,
                            0,
                            "QE ownership recorded before write"
                        );
                    }
                });
            }
            #[test]
            fn cleanup_failure_blocks_all_access_and_attempts_independent_exit() {
                futures_lite::future::block_on(async {
                    let mut dev = device();
                    dev.master().reject_restore = true;
                    assert_eq!(dev.read(0, &mut [0; 8]).await, Err(Error::RecoveryRequired));
                    assert!(!dev.master().four, "QE error must not skip EX4B");
                    assert_blocked(&mut dev).await;
                    let mut dev = device();
                    dev.master().fault = Some((opcodes::EX4B, 1, Failure::Error));
                    assert_eq!(dev.read(0, &mut [0; 8]).await, Err(Error::RecoveryRequired));
                    assert_blocked(&mut dev).await;
                });
            }
            #[test]
            fn dropped_operation_rejects_io_and_every_wp_entry() {
                futures_lite::future::block_on(async {
                    for (opcode, nth) in [
                        (opcodes::EN4B, 1),
                        (opcodes::WRSR2, 1),
                        (opcodes::QIOR, 1),
                        (opcodes::WRSR2, 2),
                        (opcodes::EX4B, 1),
                    ] {
                        let mut dev = device();
                        dev.master().fault = Some((opcode, nth, Failure::Pending));
                        {
                            let mut buf = [0; 8];
                            let mut future = core::pin::pin!(dev.read(0, &mut buf));
                            assert!(futures_lite::future::poll_once(&mut future).await.is_none());
                        }
                        assert!(dev.recovery_required());
                        assert_blocked(&mut dev).await;
                    }
                });
            }
            #[test]
            fn erase_verification_has_fresh_scope_and_program_addressing_is_independent() {
                futures_lite::future::block_on(async {
                    let mut dev = device();
                    dev.context_mut().chip.features |= Features::FOUR_BYTE_QUAD_IO_READ;
                    dev.read(0x0100_0000, &mut [0; 8]).await.unwrap(); // native read, compatibility PP/erase
                    for addr in [0x0100_0000, 0x0100_1000] {
                        dev.erase(addr, 4096).await.unwrap();
                        assert!(!dev.master().four);
                    }
                    dev.write(0x0100_2003, &[0xaa; 300]).await.unwrap();
                    dev.read(0x0100_2003, &mut [0; 8]).await.unwrap();
                    let master = dev.master();
                    assert!(master.addresses.contains(&(0x20, 0x0100_0000)));
                    assert!(master.addresses.contains(&(0x20, 0x0100_1000)));
                    assert!(master.addresses.contains(&(0x02, 0x0100_2003)));
                    assert_eq!(
                        master.calls.iter().filter(|o| **o == opcodes::EN4B).count(),
                        5
                    );
                    assert_eq!(
                        master.calls.iter().filter(|o| **o == opcodes::EX4B).count(),
                        5
                    );
                });
            }
            #[test]
            fn ear_is_saved_and_restored_across_bank_boundary() {
                futures_lite::future::block_on(async {
                    let mut dev = device();
                    dev.context_mut().chip.features -= Features::FOUR_BYTE_ENTER;
                    dev.context_mut().chip.features |= Features::EXT_ADDR_REG_C5C8;
                    dev.master().ear = 3;
                    let mut buf = [0; 1024];
                    dev.read(0x00ff_ff00, &mut buf).await.unwrap();
                    assert_eq!(dev.master().ear, 3);
                    dev.write(0x00ff_ff00, &[0x55; 1024]).await.unwrap();
                    assert_eq!(dev.master().ear, 3);
                    assert!(
                        dev.master()
                            .addresses
                            .iter()
                            .any(|(_, addr)| *addr >= 0x0100_0000)
                    );
                });
            }
            #[test]
            fn explicit_alternate_ear_overrides_generic_capability_for_save_and_restore() {
                futures_lite::future::block_on(async {
                    let mut dev = device();
                    dev.context_mut().chip.features -= Features::FOUR_BYTE_ENTER;
                    // This combination is emitted by chip codegen.
                    dev.context_mut().chip.features |=
                        Features::EXT_ADDR_REG | Features::EXT_ADDR_REG_1716;
                    dev.master().ear = 3;
                    dev.read(0x0100_0000, &mut [0; 8]).await.unwrap();
                    assert_eq!(dev.master().ear, 3);
                    let calls = &dev.master().calls;
                    assert_eq!(
                        calls.iter().filter(|&&op| op == opcodes::RDEAR_ALT).count(),
                        2
                    );
                    assert_eq!(
                        calls.iter().filter(|&&op| op == opcodes::WREAR_ALT).count(),
                        2
                    );
                    assert!(!calls.contains(&opcodes::RDEAR));
                    assert!(!calls.contains(&opcodes::WREAR));
                });
            }
            #[test]
            fn mandatory_legacy_ewsr_still_persists_wp() {
                futures_lite::future::block_on(async {
                    let mut dev = device();
                    dev.context_mut().chip.features -= Features::WRSR_WREN | Features::ANY_QUAD;
                    dev.master().legacy = true;
                    dev.master().sr[0] = 0x1c;
                    dev.master().nv = dev.master().sr;
                    dev.disable_wp(WriteOptions::default()).await.unwrap();
                    assert!(dev.master().calls.contains(&opcodes::EWSR));
                    assert_eq!(dev.master().nv[0] & 0x1c, 0);
                });
            }
        }
    };
}
adapter_tests!(spi, SpiFlashDevice);
adapter_tests!(hybrid, HybridFlashDevice);

#[test]
fn planned_erase_capability_selects_firmware_or_spi_before_io_never_retries_failure() {
    futures_lite::future::block_on(async {
        for (supported, failed) in [(false, false), (true, false), (true, true)] {
            let master = Master {
                firmware_erase: supported,
                fail_firmware_erase: failed,
                ..Default::default()
            };
            let mut dev = HybridFlashDevice::new(master, FlashContext::new(chip()));
            assert_eq!(dev.erase(0x0100_0000, 4096).await.is_ok(), !failed);
            assert_eq!(dev.master().firmware_erases, usize::from(supported));
            assert_eq!(dev.master().calls.contains(&0x20), !supported);
            assert_eq!(dev.recovery_required(), failed);
        }
    });
}
#[test]
fn short_planned_read_requires_recovery_without_touching_temporary_state() {
    futures_lite::future::block_on(async {
        let mut dev = HybridFlashDevice::new(
            Master {
                short_read: true,
                ..Default::default()
            },
            FlashContext::new(chip()),
        );
        assert_eq!(
            dev.read(3, &mut [0; 1600]).await,
            Err(Error::RecoveryRequired)
        );
        assert_eq!(dev.master().sr[1] & 2, 2);
        assert!(dev.master().four);
        assert_eq!(dev.master().bulk_reads, 1);
    });
}
#[test]
fn still_busy_cleanup_does_not_exit_address_mode_or_allow_reuse() {
    futures_lite::future::block_on(async {
        let mut dev = SpiFlashDevice::new(
            Master {
                busy: true,
                ..Default::default()
            },
            FlashContext::new(chip()),
        );
        assert_eq!(
            dev.write(0x0100_0000, &[0; 8]).await,
            Err(Error::RecoveryRequired)
        );
        assert!(dev.recovery_required());
        assert!(dev.master().four);
        assert!(!dev.master().calls.contains(&opcodes::EX4B));
    });
}

#[test]
fn unsupported_bulk_read_selects_single_spi_before_qe_setup() {
    futures_lite::future::block_on(async {
        let mut dev = HybridFlashDevice::new(
            Master {
                no_bulk_read: true,
                ..Default::default()
            },
            FlashContext::new(chip()),
        );
        dev.read(0x0100_0003, &mut [0; 1024]).await.unwrap();
        assert_eq!(dev.master().bulk_reads, 0);
        assert!(!dev.master().calls.contains(&opcodes::WRSR2));
        assert!(
            dev.master()
                .addresses
                .contains(&(opcodes::FAST_READ, 0x0100_0003))
        );
        assert!(!dev.master().four);
    });
}

#[test]
fn planned_defaults_reject_without_discarding_plans_but_raw_opaque_still_works() {
    #[derive(Default)]
    struct RawOnly(usize);
    impl OpaqueMaster for RawOnly {
        fn size(&self) -> usize {
            4096
        }
        async fn read(&mut self, _: u32, buf: &mut [u8]) -> Result<()> {
            self.0 += 1;
            buf.fill(0xff);
            Ok(())
        }
        async fn write(&mut self, _: u32, _: &[u8]) -> Result<()> {
            self.0 += 1;
            Ok(())
        }
        async fn erase(&mut self, _: u32, _: u32) -> Result<()> {
            self.0 += 1;
            Ok(())
        }
    }
    futures_lite::future::block_on(async {
        let ctx = FlashContext::new(chip());
        let master = Master::default();
        let read = io::select_read_plan(&master, &ctx, false, |_| true).unwrap();
        let write = io::select_write_plan(&master, &ctx, |_| true).unwrap();
        let erase = io::select_erase_plan(&master, &ctx, 0, 4096).unwrap();
        let mut raw = RawOnly::default();
        assert!(!raw.supports_read_plan(&read));
        assert!(!raw.supports_write_plan(&write));
        assert!(!raw.supports_erase_plan(&erase));
        assert_eq!(
            raw.read_planned(&read, 0, &mut [0; 1]).await,
            Err(Error::ChipNotSupported)
        );
        assert_eq!(
            raw.write_planned(&write, 0, &[0]).await,
            Err(Error::ChipNotSupported)
        );
        assert_eq!(
            raw.erase_planned(&erase, 0, 4096).await,
            Err(Error::ChipNotSupported)
        );
        assert_eq!(raw.0, 0);
        let mut device = OpaqueFlashDevice::new(raw, 4096);
        device.read(0, &mut [0; 1]).await.unwrap();
        device.write(0, &[0]).await.unwrap();
        device.erase(0, 4096).await.unwrap();
    });
}

#[test]
fn uncertain_firmware_failure_skips_even_status_cleanup_and_blocks_wp() {
    futures_lite::future::block_on(async {
        let mut dev = HybridFlashDevice::new(
            Master {
                firmware_erase: true,
                fail_firmware_erase: true,
                ..Default::default()
            },
            FlashContext::new(chip()),
        );
        assert_eq!(
            dev.erase(0x0100_0000, 4096).await,
            Err(Error::RecoveryRequired)
        );
        assert!(
            !dev.master().busy,
            "flash idle does not prove firmware quiescence"
        );
        assert!(dev.master().four);
        // Setup's EN4B is the only ordinary SPI command. No RDSR, WRDI or EX4B.
        assert_eq!(dev.master().calls, [opcodes::EN4B]);
        assert!(dev.disable_wp(WriteOptions::default()).await.is_err());
        assert!(dev.read_wp_config().await.is_err());
        assert_eq!(dev.master().calls, [opcodes::EN4B]);
    });
}

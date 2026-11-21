use alloc::{vec, vec::Vec};

use super::diff::{get_next_write_range, need_erase};
use super::probe::{common_operation_profile, select_database_chip};
use super::*;
use crate::chip::ChipProvider;
use crate::chip::{EraseBlock, Features, WriteGranularity};
use crate::error::{Error, Result};
use crate::flash::{AddressMode, FlashContext};
use crate::programmer::{SpiFeatures, SpiMaster};
use crate::protocol;
use crate::spi::opcodes;

#[test]
fn bundled_common_jedec_collisions_resolve_to_safe_profiles() {
    let db = rflasher_chips::ChipDatabase::new();
    for (manufacturer, device) in [
        (0xc8, 0x4018),
        (0x0b, 0x4018),
        (0xc2, 0x2017),
        (0xc2, 0x2018),
        (0xef, 0x4017),
    ] {
        let profile = select_database_chip(&db, manufacturer, device, None, None)
            .unwrap()
            .unwrap();
        let candidates: Vec<_> = (0..)
            .map_while(|n| db.find_nth_by_jedec_id(manufacturer, device, n))
            .collect();
        assert!(candidates.len() > 1);
        assert!(
            profile
                .erase_blocks
                .iter()
                .all(|b| candidates.iter().all(|c| c.erase_blocks().contains(b)))
        );
        assert!(profile.erase_blocks.iter().any(|b| !b.is_chip_erase()));
        // Popular aliases must keep persistent write protection.
        assert!(
            crate::wp::WriteOptions::default()
                .for_features(profile.features)
                .is_ok(),
            "{manufacturer:02x}:{device:04x}"
        );
        assert!(!profile.features.contains(Features::WP_UNRESOLVED));
    }
}

#[test]
fn common_profiles_do_not_drop_cmp_checks_on_three_byte_chips() {
    let db = rflasher_chips::ChipDatabase::new();
    let chip = db.find_by_jedec_id(0xef, 0x4017).unwrap();
    assert!(!chip.requires_4byte_addr());
    let checks = Features::WP_CMP | Features::WP_CMP_SR2;
    assert!(chip.features.contains(checks));
    let profile = common_operation_profile(&[chip, chip]).unwrap();
    assert!(profile.features.contains(checks));
    for remove in [Features::WP_CMP, Features::WP_CMP_SR2, checks] {
        let mut other = chip.clone();
        other.features.remove(remove);
        assert!(common_operation_profile(&[chip, &other]).is_none());
        assert!(common_operation_profile(&[&other, chip]).is_none());
    }
}

#[test]
fn aliases_with_different_wrsr_lengths_cannot_write_status() {
    let db = rflasher_chips::ChipDatabase::new();
    let chip = db.find_by_jedec_id(0xef, 0x4017).unwrap();
    let mut other = chip.clone();
    other.features.toggle(Features::WRSR_EXT);
    for pair in [[chip, &other], [&other, chip]] {
        let profile = common_operation_profile(&pair).unwrap();
        assert!(profile.features.contains(Features::WP_UNRESOLVED));
        assert!(
            crate::wp::WriteOptions::default()
                .for_features(profile.features)
                .is_err()
        );
    }
}

#[test]
fn aliases_retain_possible_second_status_register() {
    let db = rflasher_chips::ChipDatabase::new();
    let chip = db.find_by_jedec_id(0xef, 0x4017).unwrap();
    let mut other = chip.clone();
    other
        .features
        .remove(Features::STATUS_REG_2 | Features::QE_SR2);
    for pair in [[chip, &other], [&other, chip]] {
        assert!(
            common_operation_profile(&pair)
                .unwrap()
                .features
                .contains(Features::STATUS_REG_2)
        );
    }
}

#[test]
fn aliases_with_different_wps_layouts_cannot_report_wp() {
    let db = rflasher_chips::ChipDatabase::new();
    let chip = db.find_by_jedec_id(0xef, 0x4017).unwrap();
    let mut other = chip.clone();
    other.features.toggle(Features::WP_WPS);
    for pair in [[chip, &other], [&other, chip]] {
        assert!(
            common_operation_profile(&pair)
                .unwrap()
                .features
                .contains(Features::WP_UNRESOLVED)
        );
    }
}

struct IdMaster {
    response: [u8; 4],
    reread: Option<Result<[u8; 4]>>,
    commands: Vec<(u8, usize)>,
    max_read: usize,
}

impl SpiMaster for IdMaster {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::empty()
    }
    fn max_read_len(&self) -> usize {
        self.max_read
    }
    fn max_write_len(&self) -> usize {
        256
    }
    async fn execute(&mut self, cmd: &mut crate::spi::SpiCommand<'_>) -> Result<()> {
        self.commands.push((cmd.opcode, cmd.read_buf.len()));
        if cmd.opcode == opcodes::RDID {
            assert!(cmd.read_buf.len() <= self.max_read);
            let response = if cmd.read_buf.len() == 4 {
                self.reread.unwrap_or(Ok(self.response))?
            } else {
                self.response
            };
            cmd.read_buf
                .copy_from_slice(&response[..cmd.read_buf.len()]);
        } else {
            assert!(matches!(cmd.opcode, opcodes::RDSFDP | opcodes::RES));
            cmd.read_buf.fill(0xff);
        }
        Ok(())
    }
    async fn delay_us(&mut self, _us: u32) {}
}

#[test]
fn continuation_probe_matches_banked_ids() {
    use futures_lite::future::block_on;
    let db = rflasher_chips::ChipDatabase::new();
    for (response, expected, reads) in [
        ([0x7f, 0x37, 0x20, 0x20], "A25L05PT", vec![3, 4]),
        ([0x7f, 0x9d, 0x21, 0x21], "Pm25LD010(C)", vec![3]),
        ([0x9d, 0x60, 0x15, 0], "IS25LP016D", vec![3]),
        ([0x37, 0x40, 0x15, 0], "A25LQ16", vec![3]),
    ] {
        let mut master = IdMaster {
            response,
            reread: None,
            commands: Vec::new(),
            max_read: 256,
        };
        let result = block_on(probe_detailed(&mut master, &db)).unwrap();
        assert!(result.from_database);
        assert_eq!(result.chip.name(), expected);
        assert_eq!(
            master
                .commands
                .iter()
                .filter(|(op, _)| *op == opcodes::RDID)
                .map(|(_, len)| *len)
                .collect::<Vec<_>>(),
            reads
        );
        assert!(master.commands.iter().any(|(op, _)| *op == opcodes::RDSFDP));
    }
    // The shared 0x7e identity must honor an explicit choice and must not
    // allow selection of the legacy RES2 entry with a different ID.
    let mut master = IdMaster {
        response: [0x7f, 0x9d, 0x7e, 0x7e],
        reread: None,
        commands: Vec::new(),
        max_read: 3,
    };
    let options = ProbeOptions {
        chip: Some("Pm25LD040(C)"),
        ..Default::default()
    };
    let result = block_on(probe_with_options(&mut master, &db, &options)).unwrap();
    assert_eq!(result.chip.name(), "Pm25LD040(C)");
    let options = ProbeOptions {
        chip: Some("Pm25LV040"),
        ..Default::default()
    };
    let result = block_on(probe_with_options(&mut master, &db, &options)).unwrap();
    assert_eq!(result.chip.name(), "Pm25LV040");
    let options = ProbeOptions {
        chip: Some("Pm25LV512(A)"),
        ..Default::default()
    };
    assert!(matches!(
        block_on(probe_with_options(&mut master, &db, &options)),
        Err(Error::ChipNotFound)
    ));

    for (response, max_read) in [
        ([0x37, 0x20, 0x20, 0], 256),
        ([0x7f, 0x37, 0x20, 0x20], 3),
        ([0x7f, 0x9d, 0x7b, 0x7b], 256),
    ] {
        let mut master = IdMaster {
            response,
            reread: None,
            commands: Vec::new(),
            max_read,
        };
        assert!(matches!(
            block_on(probe_detailed(&mut master, &db)),
            Err(Error::ChipNotFound)
        ));
        assert_eq!(
            master
                .commands
                .iter()
                .filter(|(op, _)| *op == opcodes::RDID)
                .count(),
            1
        );
    }
}

#[test]
fn continuation_reread_failures_abort_probe_and_sfdp_fallback() {
    use futures_lite::future::block_on;
    let db = rflasher_chips::ChipDatabase::new();
    for reread in [Err(Error::SpiTransferFailed), Ok([0x7f, 0x37, 0x21, 0x20])] {
        let mut master = IdMaster {
            response: [0x7f, 0x37, 0x20, 0x20],
            reread: Some(reread),
            commands: Vec::new(),
            max_read: 256,
        };
        assert!(matches!(
            block_on(probe_detailed(&mut master, &db)),
            Err(Error::SpiTransferFailed)
        ));
        assert_eq!(
            master
                .commands
                .iter()
                .filter(|(op, _)| *op == opcodes::RDID)
                .map(|(_, len)| *len)
                .collect::<Vec<_>>(),
            [3, 4]
        );
        let sfdp = crate::sfdp::SfdpInfo::default();
        assert!(matches!(
            block_on(select_probed_chip(
                &mut master,
                &db,
                0x7f,
                0x3720,
                Some(&sfdp),
                None,
            )),
            Err(Error::SpiTransferFailed)
        ));
    }
}

#[derive(Default)]
struct ReadMaster {
    commands: Vec<(u8, crate::spi::IoMode, crate::spi::AddressWidth, u8)>,
    fail_read: bool,
    fail_exit: bool,
    reject_native: bool,
    reject_opcode: Option<u8>,
}

impl SpiMaster for ReadMaster {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::FOUR_BYTE_ADDR | SpiFeatures::DUAL | SpiFeatures::QUAD
    }
    fn max_read_len(&self) -> usize {
        256
    }
    fn max_write_len(&self) -> usize {
        256
    }
    fn probe_opcode(&self, opcode: u8) -> bool {
        !(self.reject_native && opcode == opcodes::READ_4B) && self.reject_opcode != Some(opcode)
    }
    async fn execute(&mut self, cmd: &mut crate::spi::SpiCommand<'_>) -> Result<()> {
        self.commands
            .push((cmd.opcode, cmd.io_mode, cmd.address_width, cmd.dummy_cycles));
        if self.fail_read && matches!(cmd.opcode, opcodes::READ | opcodes::READ_4B) {
            return Err(Error::SpiTransferFailed);
        }
        if self.fail_exit && cmd.opcode == opcodes::EX4B {
            return Err(Error::Timeout);
        }
        cmd.read_buf.fill(0x5a);
        Ok(())
    }
    async fn delay_us(&mut self, _us: u32) {}
}

#[test]
fn program_preflight_checks_every_needed_opcode_before_commands() {
    use futures_lite::future::block_on;
    let mut ctx = nonuniform_context();
    ctx.chip.features |= Features::AAI_WORD;
    for opcode in [opcodes::WREN, opcodes::RDSR, opcodes::PP] {
        let mut master = ReadMaster {
            reject_opcode: Some(opcode),
            ..Default::default()
        };
        assert!(matches!(
            block_on(spi_program(&mut master, &ctx, 0, &[1, 2, 3])),
            Err(Error::OpcodeNotSupported)
        ));
        assert!(master.commands.is_empty());
    }
}

#[test]
fn aai_falls_back_to_byte_program_when_menu_lacks_aai_or_wrdi() {
    use futures_lite::future::block_on;
    let mut ctx = nonuniform_context();
    ctx.chip.features |= Features::AAI_WORD;
    ctx.chip.write_granularity = WriteGranularity::Page;
    for opcode in [opcodes::AAI_WP, opcodes::WRDI] {
        let mut master = ReadMaster {
            reject_opcode: Some(opcode),
            ..Default::default()
        };
        block_on(spi_program(&mut master, &ctx, 0, &[1, 2, 3])).unwrap();
        assert_eq!(
            master
                .commands
                .iter()
                .filter(|c| c.0 == opcodes::PP)
                .count(),
            3
        );
        assert!(!master.commands.iter().any(|c| c.0 == opcodes::AAI_WP));
    }
}

#[test]
fn erase_preflight_checks_prerequisites_before_commands() {
    use futures_lite::future::block_on;
    let ctx = nonuniform_context();
    let op = OptimalEraseOp {
        start: 0,
        size: 8,
        erase_block: ctx.chip.erase_blocks[0].clone(),
    };
    for opcode in [opcodes::WREN, opcodes::RDSR, 0x20] {
        let mut master = ReadMaster {
            reject_opcode: Some(opcode),
            ..Default::default()
        };
        assert!(matches!(
            block_on(erase_spi_operation(&mut master, &ctx, &op)),
            Err(Error::OpcodeNotSupported)
        ));
        assert!(master.commands.is_empty());
    }
}

#[test]
fn automatic_reads_use_only_single_io() {
    use crate::spi::{AddressWidth, IoMode};
    use futures_lite::future::block_on;
    for native in [false, true] {
        let mut ctx = nonuniform_context();
        ctx.chip.features = Features::DUAL_IO
            | Features::QUAD_IO
            | Features::FOUR_BYTE_DUAL_OUT_READ
            | Features::FOUR_BYTE_DUAL_IO_READ
            | Features::FOUR_BYTE_QUAD_OUT_READ
            | Features::FOUR_BYTE_QUAD_IO_READ;
        if native {
            ctx.address_mode = AddressMode::FourByte;
            ctx.chip.features |= Features::FOUR_BYTE_READ;
        }
        let mut master = ReadMaster::default();
        let mut buf = [0; 2];
        block_on(spi_read(&mut master, &ctx, 0, &mut buf)).unwrap();
        assert_eq!(buf, [0x5a; 2]);
        assert_eq!(
            master.commands,
            vec![(
                if native {
                    opcodes::READ_4B
                } else {
                    opcodes::READ
                },
                IoMode::Single,
                if native {
                    AddressWidth::FourByte
                } else {
                    AddressWidth::ThreeByte
                },
                0,
            )]
        );
    }
}

#[test]
fn automatic_read_exits_four_byte_mode_and_preserves_errors() {
    use futures_lite::future::block_on;
    for (fail_read, fail_exit, expected) in [
        (false, false, Ok(())),
        (false, true, Err(Error::Timeout)),
        (true, false, Err(Error::SpiTransferFailed)),
        (true, true, Err(Error::SpiTransferFailed)),
    ] {
        let mut ctx = nonuniform_context();
        ctx.address_mode = AddressMode::FourByte;
        ctx.chip.features = Features::FOUR_BYTE_ENTER
            | Features::FOUR_BYTE_READ
            | Features::DUAL_IO
            | Features::QUAD_IO;
        let mut master = ReadMaster {
            fail_read,
            fail_exit,
            reject_native: true,
            ..Default::default()
        };
        assert_eq!(
            block_on(spi_read(&mut master, &ctx, 0, &mut [0; 1])),
            expected
        );
        assert_eq!(
            master.commands.iter().map(|c| c.0).collect::<Vec<_>>(),
            vec![opcodes::EN4B, opcodes::READ, opcodes::EX4B]
        );
    }
}

struct EraseMaster {
    commands: Vec<(u8, Option<u32>)>,
    fail_erase: bool,
}

impl SpiMaster for EraseMaster {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::FOUR_BYTE_ADDR
    }
    fn max_read_len(&self) -> usize {
        256
    }
    fn max_write_len(&self) -> usize {
        256
    }
    async fn execute(&mut self, cmd: &mut crate::spi::SpiCommand<'_>) -> Result<()> {
        self.commands.push((cmd.opcode, cmd.address));
        if self.fail_erase && cmd.opcode == 0x20 {
            return Err(Error::SpiTransferFailed);
        }
        if cmd.opcode == crate::spi::opcodes::RDSR {
            cmd.read_buf.fill(0);
        } else {
            cmd.read_buf.fill(0xff);
        }
        Ok(())
    }
    async fn delay_us(&mut self, _us: u32) {}
}

fn nonuniform_context() -> FlashContext {
    use crate::chip::{ChipTestStatus, EraseRegion, FlashChip};
    FlashContext::new(FlashChip {
        vendor: "test".into(),
        name: "test".into(),
        jedec_manufacturer: 0,
        jedec_device: 0,
        ids: Default::default(),
        total_size: 48,
        page_size: 16,
        features: Features::empty(),
        unlock: Default::default(),
        voltage_min_mv: 2700,
        voltage_max_mv: 3600,
        write_granularity: WriteGranularity::Byte,
        erase_blocks: vec![EraseBlock::with_regions(
            0x20,
            &[EraseRegion::new(8, 2), EraseRegion::new(16, 2)],
        )]
        .into(),
        tested: ChipTestStatus::default(),
    })
}

#[test]
fn spi_erase_uses_physical_nonuniform_boundaries() {
    use crate::flash::{FlashDevice, SpiFlashDevice};
    use futures_lite::future::block_on;
    let master = EraseMaster {
        commands: vec![],
        fail_erase: false,
    };
    let mut device = SpiFlashDevice::new(master, nonuniform_context());
    block_on(device.erase(8, 24)).unwrap();
    let erases: Vec<_> = device
        .into_parts()
        .0
        .commands
        .into_iter()
        .filter(|(opcode, _)| *opcode == 0x20)
        .map(|(_, addr)| addr)
        .collect();
    assert_eq!(erases, vec![Some(8), Some(16)]);
}

#[test]
fn writability_is_judged_by_what_the_chip_and_programmer_share() {
    use crate::flash::SpiFlashDevice;

    struct Menu(&'static [u8]);
    impl SpiMaster for Menu {
        fn features(&self) -> SpiFeatures {
            SpiFeatures::empty()
        }
        fn max_read_len(&self) -> usize {
            64
        }
        fn max_write_len(&self) -> usize {
            64
        }
        fn probe_opcode(&self, opcode: u8) -> bool {
            self.0.contains(&opcode)
        }
        async fn execute(&mut self, _: &mut crate::spi::SpiCommand<'_>) -> Result<()> {
            Err(Error::SpiTransferFailed)
        }
        async fn delay_us(&mut self, _us: u32) {}
    }
    let db = rflasher_chips::ChipDatabase::new();
    let sst = db.find_by_name("SST25VF016B")[0].clone();
    let writable =
        |menu| SpiFlashDevice::new(Menu(menu), FlashContext::new(sst.clone())).check_writable();
    // Word programming alone programs the chip; so does byte programming.
    writable(&[0x06, 0x05, 0xad, 0x04, 0x20]).unwrap();
    writable(&[0x06, 0x05, 0x02, 0x20]).unwrap();
    // No way to program, to erase a block, or to poll completion.
    for menu in [
        &[0x06, 0x05, 0x20][..],
        &[0x06, 0x05, 0x02, 0xc7],
        &[0x06, 0x02, 0x20],
    ] {
        assert_eq!(
            writable(menu),
            Err(Error::OpcodeNotSupported),
            "{menu:02x?}"
        );
    }
}

#[test]
fn erases_are_planned_with_erasers_the_programmer_can_send() {
    use crate::flash::{SpiFlashDevice, unified};
    use futures_lite::future::block_on;

    // Like a locked Intel opcode menu holding only the sector erase.
    struct SectorOnly(EraseMaster);
    impl SpiMaster for SectorOnly {
        fn features(&self) -> SpiFeatures {
            self.0.features()
        }
        fn max_read_len(&self) -> usize {
            self.0.max_read_len()
        }
        fn max_write_len(&self) -> usize {
            self.0.max_write_len()
        }
        fn probe_opcode(&self, opcode: u8) -> bool {
            !matches!(opcode, 0xd8 | 0xc7)
        }
        async fn execute(&mut self, cmd: &mut crate::spi::SpiCommand<'_>) -> Result<()> {
            self.0.execute(cmd).await
        }
        async fn delay_us(&mut self, _us: u32) {}
    }

    // A catalog descriptor keeps every eraser the chip has; nothing
    // pruned it for this programmer as database probing would.
    let mut ctx = nonuniform_context();
    ctx.chip.erase_blocks = vec![
        EraseBlock::with_count(0x20, 16, 3),
        EraseBlock::new(0xd8, 48),
        EraseBlock::new(0xc7, 48),
    ]
    .into();
    let master = SectorOnly(EraseMaster {
        commands: vec![],
        fail_erase: false,
    });
    let mut device = SpiFlashDevice::new(master, ctx);
    let whole = crate::layout::Region::new("all", 0, 47);
    block_on(unified::erase_region(&mut device, &whole)).unwrap();
    let erases: Vec<_> = (device.into_parts().0.0.commands.into_iter())
        .filter(|(opcode, _)| matches!(opcode, 0x20 | 0xd8 | 0xc7))
        .collect();
    assert_eq!(
        erases,
        vec![(0x20, Some(0)), (0x20, Some(16)), (0x20, Some(32))]
    );
}

#[test]
fn exact_planned_opcode_survives_dispatch() {
    use crate::flash::{FlashDevice, SpiFlashDevice};
    use futures_lite::future::block_on;
    let mut ctx = nonuniform_context();
    ctx.chip.erase_blocks = vec![
        EraseBlock::with_count(0x20, 16, 3),
        EraseBlock::new(0xc7, 48),
    ]
    .into();
    let op = OptimalEraseOp {
        start: 0,
        size: 48,
        erase_block: ctx.chip.erase_blocks[0].clone(),
    };
    let master = EraseMaster {
        commands: vec![],
        fail_erase: false,
    };
    let mut device = SpiFlashDevice::new(master, ctx);
    assert_eq!(
        block_on(device.erase_operation(&op)),
        Ok(crate::flash::EraseOutcome::Unverified)
    );
    let commands = device.into_parts().0.commands;
    assert_eq!(commands.iter().filter(|(code, _)| *code == 0x20).count(), 3);
    assert!(!commands.iter().any(|(code, _)| *code == 0xc7));
}

#[test]
fn nonuniform_erase_rejects_mid_block_before_spi_commands() {
    use futures_lite::future::block_on;
    let mut ctx = nonuniform_context();
    ctx.chip.features = Features::SST26_BPR;
    let mut master = EraseMaster {
        commands: vec![],
        fail_erase: false,
    };
    assert_eq!(
        block_on(erase_spi_range(&mut master, &ctx, 24, 16)),
        Err(Error::InvalidAlignment)
    );
    // An invalid request must not unlock the chip's block protection.
    assert!(master.commands.is_empty());

    block_on(erase_spi_range(&mut master, &ctx, 8, 24)).unwrap();
    assert!(master.commands.contains(&(opcodes::SE_20, Some(8))));
}

#[test]
fn spi_erase_exits_four_byte_mode_on_failure() {
    use futures_lite::future::block_on;
    let mut ctx = nonuniform_context();
    ctx.address_mode = AddressMode::FourByte;
    ctx.chip.features = Features::FOUR_BYTE_ENTER;
    let mut master = EraseMaster {
        commands: vec![],
        fail_erase: true,
    };
    assert_eq!(
        block_on(erase_spi_range(&mut master, &ctx, 8, 24)),
        Err(Error::SpiTransferFailed)
    );
    assert_eq!(master.commands.last().unwrap().0, crate::spi::opcodes::EX4B);
}

#[test]
fn single_addressed_block_erase_keeps_its_address() {
    use futures_lite::future::block_on;
    let mut ctx = nonuniform_context();
    ctx.chip.erase_blocks = vec![EraseBlock::new(0xD8, 48)].into();
    let plan = plan_optimal_erase_region(ctx.chip.erase_blocks(), 48, 0, 47).unwrap();
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0].erase_block.opcode, 0xD8);

    let mut master = EraseMaster {
        commands: vec![],
        fail_erase: false,
    };
    block_on(erase_spi_range(&mut master, &ctx, 0, 48)).unwrap();
    assert!(master.commands.contains(&(0xD8, Some(0))));
    assert!(!master.commands.contains(&(0xD8, None)));
}

#[test]
fn spi_chip_erase_only_uses_addressless_opcode() {
    use futures_lite::future::block_on;
    for opcode in [0x60, 0x62, 0xC7] {
        let mut ctx = nonuniform_context();
        ctx.chip.erase_blocks = vec![EraseBlock::new(opcode, 48)].into();
        let mut master = EraseMaster {
            commands: vec![],
            fail_erase: false,
        };
        block_on(erase_spi_range(&mut master, &ctx, 0, 48)).unwrap();
        assert!(master.commands.contains(&(opcode, None)));
        assert_eq!(
            block_on(erase_spi_range(&mut master, &ctx, 0, 16)),
            Err(Error::InvalidAlignment)
        );
    }
}

#[test]
fn full_chip_erase_uses_chip_erase_opcode_even_with_sector_erase_available() {
    use futures_lite::future::block_on;
    let mut ctx = nonuniform_context();
    ctx.chip.erase_blocks = vec![
        EraseBlock::with_count(0x20, 16, 3),
        EraseBlock::new(0xC7, 48),
    ]
    .into();
    let mut master = EraseMaster {
        commands: vec![],
        fail_erase: false,
    };
    block_on(erase_spi_range(&mut master, &ctx, 0, 48)).unwrap();
    assert!(master.commands.contains(&(0xC7, None)));
    assert!(master.commands.iter().all(|(opcode, _)| *opcode != 0x20));

    // Anything short of the whole chip still walks addressed sectors.
    master.commands.clear();
    block_on(erase_spi_range(&mut master, &ctx, 16, 16)).unwrap();
    assert!(master.commands.contains(&(0x20, Some(16))));
    assert!(master.commands.iter().all(|(opcode, _)| *opcode != 0xC7));
}

#[test]
fn erase_timing_scales_from_sector_to_die_erase() {
    let (sector_poll, sector_timeout) = protocol::erase_timing(4096);
    let (block_poll, block_timeout) = protocol::erase_timing(64 * 1024);
    let (die_poll, die_timeout) = protocol::erase_timing(32 * 1024 * 1024);
    assert!(sector_poll < block_poll && block_poll < die_poll);
    assert!(sector_timeout < block_timeout && block_timeout < die_timeout);
    // Micron die erase takes up to ~480 s; block erase up to ~4 s.
    assert!(die_timeout >= 480_000_000);
    assert!(block_timeout >= 4_000_000 && sector_timeout >= 800_000);
}

/// Create test erase blocks for a chip of given size
/// These have proper block counts so they aren't detected as chip erase
fn test_erase_blocks_4k_64k(flash_size: u32) -> Vec<EraseBlock> {
    vec![
        EraseBlock::with_count(opcodes::SE_20, 4096, flash_size / 4096), // 4KB sectors
        EraseBlock::with_count(opcodes::BE_D8, 65536, flash_size / 65536), // 64KB blocks
    ]
}

/// Create test erase blocks with 4KB, 32KB, and 64KB options
fn test_erase_blocks_4k_32k_64k(flash_size: u32) -> Vec<EraseBlock> {
    vec![
        EraseBlock::with_count(opcodes::SE_20, 4096, flash_size / 4096), // 4KB sectors
        EraseBlock::with_count(opcodes::BE_52, 32768, flash_size / 32768), // 32KB blocks
        EraseBlock::with_count(opcodes::BE_D8, 65536, flash_size / 65536), // 64KB blocks
    ]
}

// =========================================================================
// Tests for smart write functions
// =========================================================================

#[test]
fn test_need_erase_no_change() {
    // No change means no erase needed
    let have = [0xAA, 0xBB, 0xCC, 0xDD];
    let want = [0xAA, 0xBB, 0xCC, 0xDD];
    assert!(!need_erase(&have, &want, WriteGranularity::Byte));
}

#[test]
fn test_need_erase_already_erased() {
    // Changing erased bytes doesn't need erase
    let have = [0xFF, 0xFF, 0xFF, 0xFF];
    let want = [0xAA, 0xBB, 0xCC, 0xDD];
    assert!(!need_erase(&have, &want, WriteGranularity::Byte));
}

#[test]
fn test_need_erase_bit_clear_only() {
    // For bit granularity: 0xF0 -> 0xE0 is just clearing bit 4, OK
    let have = [0xF0];
    let want = [0xE0];
    assert!(!need_erase(&have, &want, WriteGranularity::Bit));
}

#[test]
fn test_need_erase_bit_set_needed() {
    // For bit granularity: 0xE0 -> 0xF0 needs setting bit 4, requires erase
    let have = [0xE0];
    let want = [0xF0];
    assert!(need_erase(&have, &want, WriteGranularity::Bit));
}

#[test]
fn test_need_erase_byte_granularity() {
    // For byte granularity: changing non-erased byte requires erase
    let have = [0xAA];
    let want = [0xBB];
    assert!(need_erase(&have, &want, WriteGranularity::Byte));
}

#[test]
fn test_need_write_no_change() {
    let have = [0xAA, 0xBB, 0xCC];
    let want = [0xAA, 0xBB, 0xCC];
    assert!(!need_write(&have, &want));
}

#[test]
fn test_need_write_with_change() {
    let have = [0xAA, 0xBB, 0xCC];
    let want = [0xAA, 0x00, 0xCC];
    assert!(need_write(&have, &want));
}

#[test]
fn test_get_next_write_range_no_changes() {
    let have = [0xAA, 0xBB, 0xCC, 0xDD];
    let want = [0xAA, 0xBB, 0xCC, 0xDD];
    assert!(get_next_write_range(&have, &want, 0).is_none());
}

#[test]
fn test_get_next_write_range_single_byte() {
    let have = [0xAA, 0xBB, 0xCC, 0xDD];
    let want = [0xAA, 0x00, 0xCC, 0xDD];

    let range = get_next_write_range(&have, &want, 0).unwrap();
    assert_eq!(range.start, 1);
    assert_eq!(range.len, 1);
}

#[test]
fn test_get_next_write_range_contiguous() {
    let have = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
    let want = [0xAA, 0x00, 0x00, 0x00, 0xEE];

    let range = get_next_write_range(&have, &want, 0).unwrap();
    assert_eq!(range.start, 1);
    assert_eq!(range.len, 3);
}

#[test]
fn test_get_next_write_range_multiple_ranges() {
    let have = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    let want = [0x00, 0xBB, 0xCC, 0x00, 0xEE, 0xFF];

    // First range at 0
    let range1 = get_next_write_range(&have, &want, 0).unwrap();
    assert_eq!(range1.start, 0);
    assert_eq!(range1.len, 1);

    // Second range at 3
    let range2 = get_next_write_range(&have, &want, range1.start + range1.len).unwrap();
    assert_eq!(range2.start, 3);
    assert_eq!(range2.len, 1);

    // No more ranges
    assert!(get_next_write_range(&have, &want, range2.start + range2.len).is_none());
}

#[test]
fn test_get_all_write_ranges() {
    let have = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00, 0x11];
    let want = [0x00, 0xBB, 0xCC, 0x00, 0x00, 0xFF, 0x00, 0x22];

    let ranges = get_all_write_ranges(&have, &want);
    assert_eq!(ranges.len(), 3);
    assert_eq!(ranges[0], WriteRange { start: 0, len: 1 });
    assert_eq!(ranges[1], WriteRange { start: 3, len: 2 });
    assert_eq!(ranges[2], WriteRange { start: 7, len: 1 });
}

// =========================================================================
// Tests for coalesce_write_ranges
// =========================================================================

#[test]
fn test_coalesce_page_size_1_is_noop() {
    let ranges = vec![
        WriteRange { start: 1, len: 3 },
        WriteRange { start: 10, len: 5 },
    ];
    let result = coalesce_write_ranges(&ranges, 1, 1024);
    assert_eq!(result, ranges);
}

#[test]
fn test_coalesce_empty_ranges() {
    let result = coalesce_write_ranges(&[], 256, 65536);
    assert!(result.is_empty());
}

#[test]
fn test_coalesce_single_range_aligned() {
    // Already page-aligned range should stay the same
    let ranges = vec![WriteRange {
        start: 256,
        len: 512,
    }];
    let result = coalesce_write_ranges(&ranges, 256, 65536);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].start, 256);
    assert_eq!(result[0].len, 512);
}

#[test]
fn test_coalesce_single_range_unaligned() {
    // Unaligned range should be padded to page boundaries
    let ranges = vec![WriteRange {
        start: 100,
        len: 50,
    }];
    let result = coalesce_write_ranges(&ranges, 256, 65536);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].start, 0); // Aligned down from 100
    assert_eq!(result[0].len, 256); // Aligned up to cover byte 149
}

#[test]
fn test_coalesce_range_crossing_page_boundary() {
    // Range crossing a page boundary should expand to cover both pages
    let ranges = vec![WriteRange {
        start: 200,
        len: 100,
    }]; // bytes 200-299, crosses page boundary at 256
    let result = coalesce_write_ranges(&ranges, 256, 65536);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].start, 0); // Aligned down from 200
    assert_eq!(result[0].len, 512); // Two pages: 0-255, 256-511
}

#[test]
fn test_coalesce_merges_adjacent_after_alignment() {
    // Two ranges in different pages that become adjacent after alignment
    let ranges = vec![
        WriteRange {
            start: 100,
            len: 10,
        }, // In page 0 (0-255)
        WriteRange {
            start: 300,
            len: 10,
        }, // In page 1 (256-511)
    ];
    let result = coalesce_write_ranges(&ranges, 256, 65536);
    // After alignment: [0, 256) and [256, 512) -> adjacent, merged to [0, 512)
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].start, 0);
    assert_eq!(result[0].len, 512);
}

#[test]
fn test_coalesce_preserves_gap_between_distant_ranges() {
    // Two ranges far enough apart that they stay separate after alignment
    let ranges = vec![
        WriteRange {
            start: 100,
            len: 10,
        }, // In page 0
        WriteRange {
            start: 600,
            len: 10,
        }, // In page 2 (512-767)
    ];
    let result = coalesce_write_ranges(&ranges, 256, 65536);
    // After alignment: [0, 256) and [512, 768) -> gap at [256, 512), not merged
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].start, 0);
    assert_eq!(result[0].len, 256);
    assert_eq!(result[1].start, 512);
    assert_eq!(result[1].len, 256);
}

#[test]
fn test_coalesce_clamps_to_total_size() {
    // Range near end of flash should be clamped
    let ranges = vec![WriteRange {
        start: 1000,
        len: 10,
    }];
    let result = coalesce_write_ranges(&ranges, 256, 1024);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].start, 768); // Aligned down from 1000
    assert_eq!(result[0].len, 1024 - 768); // Clamped to total_size
}

#[test]
fn test_coalesce_many_small_ranges_in_same_page() {
    // Many small ranges in the same page -> one page write
    let ranges = vec![
        WriteRange { start: 10, len: 1 },
        WriteRange { start: 50, len: 2 },
        WriteRange { start: 100, len: 5 },
        WriteRange { start: 200, len: 3 },
    ];
    let result = coalesce_write_ranges(&ranges, 256, 65536);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].start, 0);
    assert_eq!(result[0].len, 256);
}

#[test]
fn test_coalesce_typical_firmware_pattern() {
    // Simulate: erased flash (0xFF) vs firmware with scattered 0xFF bytes.
    // The byte-level comparison produces many small ranges broken by 0xFF matches.
    // After coalescing to 256-byte pages, they should merge into fewer large ranges.
    let ranges = vec![
        WriteRange { start: 0, len: 100 }, // Data in page 0
        WriteRange {
            start: 105,
            len: 150,
        }, // More data in page 0 (gap was 0xFF)
        WriteRange {
            start: 256,
            len: 200,
        }, // Data in page 1
        WriteRange {
            start: 460,
            len: 50,
        }, // More data in page 1 (gap was 0xFF)
        WriteRange {
            start: 512,
            len: 256,
        }, // Full page 2
        WriteRange {
            start: 1024,
            len: 10,
        }, // Isolated write in page 4
    ];
    let result = coalesce_write_ranges(&ranges, 256, 65536);
    // Pages 0,1,2 should merge into one range [0, 768)
    // Page 4 is separate: [1024, 1280)
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].start, 0);
    assert_eq!(result[0].len, 768); // 3 pages
    assert_eq!(result[1].start, 1024);
    assert_eq!(result[1].len, 256); // 1 page
}

// =========================================================================
// Tests for optimal erase algorithm
// =========================================================================

#[test]
fn test_optimal_erase_single_small_block() {
    // Erasing exactly one 4KB block should use the 4KB eraser
    let erase_blocks = test_erase_blocks_4k_64k(1024 * 1024); // 1MB flash

    let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 4095).unwrap();

    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].start, 0);
    assert_eq!(ops[0].size, 4096);
    assert_eq!(ops[0].erase_block.opcode, opcodes::SE_20);
}

#[test]
fn test_optimal_erase_promotes_to_larger_block() {
    // Erasing 40KB starting at 0 should use: 1x32KB + 2x4KB (if 32KB available)
    // or with just 4KB/64KB: all 4KB blocks (no promotion since <50% of 64KB)
    let erase_blocks = test_erase_blocks_4k_64k(1024 * 1024); // 1MB flash

    // 40KB = 10 x 4KB blocks, but that's <50% of 64KB, so no promotion
    let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 40959).unwrap();

    // Should be 10 x 4KB blocks
    assert_eq!(ops.len(), 10);
    assert!(ops.iter().all(|op| op.size == 4096));
}

#[test]
fn test_optimal_erase_full_64kb_block() {
    // Erasing exactly 64KB should use a single 64KB eraser
    let erase_blocks = test_erase_blocks_4k_64k(1024 * 1024); // 1MB flash

    let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 65535).unwrap();

    // >50% of 64KB needs erasing (100%), so should promote to 64KB
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].start, 0);
    assert_eq!(ops[0].size, 65536);
    assert_eq!(ops[0].erase_block.opcode, opcodes::BE_D8);
}

#[test]
fn test_optimal_erase_more_than_half_promotes() {
    // Erasing 36KB (>50% of 64KB = 32KB) should promote to 64KB
    let erase_blocks = test_erase_blocks_4k_64k(1024 * 1024); // 1MB flash

    // 36KB = 9 x 4KB, which is >50% of 64KB (32KB = 8 blocks)
    // But it starts at 0 and ends at 36KB-1, which is <64KB
    // The 64KB block at 0 covers 0-64KB, but we only need 0-36KB
    // Since 36KB < 64KB (end of region), the 64KB block extends past our region
    // The algorithm should NOT promote because the block would erase outside region
    let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 36863).unwrap();

    // Should be 9 x 4KB blocks (no promotion because 64KB extends past 36KB)
    assert_eq!(ops.len(), 9);
    assert!(ops.iter().all(|op| op.size == 4096));
}

#[test]
fn test_optimal_erase_aligned_larger_block() {
    // Erasing 64KB exactly should promote
    let erase_blocks = test_erase_blocks_4k_32k_64k(1024 * 1024); // 1MB flash

    let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 65535).unwrap();

    // Should use a single 64KB erase
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].size, 65536);
}

#[test]
fn test_optimal_erase_with_32kb_blocks() {
    // Erasing 48KB should use: 1x32KB + 4x4KB
    let erase_blocks = test_erase_blocks_4k_32k_64k(1024 * 1024); // 1MB flash

    // 48KB at offset 0: should promote first 32KB (8x4KB) to 32KB block
    // remaining 16KB (4x4KB) can be promoted to 32KB? No, 16KB < 16KB (50% of 32KB)
    // Wait, 16KB = 50%, so it's exactly at the boundary. Let's test >50%:
    let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 0, 49151).unwrap(); // 48KB

    // 48KB < 50% of 64KB? 48KB > 32KB (50% of 64KB), but ends before 64KB
    // First 32KB: 8x4KB, but >50% of 32KB (16KB), and block is within region
    // So first 32KB should be promoted
    // Second 16KB: 4x4KB, exactly 50% of 32KB... depends on strict >
    // With >, 16KB is not >16KB, so no promotion, should be 4x4KB

    // Expected: 1x32KB + 4x4KB = 5 ops
    let total_erased: u32 = ops.iter().map(|op| op.size).sum();
    assert_eq!(total_erased, 49152, "Should erase exactly 48KB");

    // Should have a 32KB block at the start
    assert!(
        ops.iter().any(|op| op.size == 32768),
        "Should have a 32KB erase"
    );
}

#[test]
fn test_optimal_erase_smart_no_erase_needed() {
    // When flash already matches, no erase should be planned
    let have = vec![0xAA; 65536];
    let want = vec![0xAA; 65536];

    let erase_blocks = test_erase_blocks_4k_64k(65536); // 64KB flash

    let ops = plan_optimal_erase(
        &erase_blocks,
        65536,
        Some(&have),
        Some(&want),
        0,
        65535,
        WriteGranularity::Byte,
    )
    .unwrap();

    assert!(ops.is_empty(), "No erase needed when contents match");
}

#[test]
fn test_optimal_erase_smart_erased_flash() {
    // Writing to erased flash shouldn't need erase
    let have = vec![0xFF; 65536];
    let want = vec![0xAA; 65536];

    let erase_blocks = test_erase_blocks_4k_64k(65536); // 64KB flash

    let ops = plan_optimal_erase(
        &erase_blocks,
        65536,
        Some(&have),
        Some(&want),
        0,
        65535,
        WriteGranularity::Byte,
    )
    .unwrap();

    assert!(
        ops.is_empty(),
        "No erase needed when flash is already erased"
    );
}

#[test]
fn test_optimal_erase_smart_partial_erase() {
    // Only erase the blocks that need it
    let mut have = vec![0xFF; 65536];
    have[0..4096].fill(0xAA); // First 4KB has data

    let mut want = vec![0xFF; 65536];
    want[0..4096].fill(0xBB); // Want to change first 4KB

    let erase_blocks = test_erase_blocks_4k_64k(65536); // 64KB flash

    let ops = plan_optimal_erase(
        &erase_blocks,
        65536,
        Some(&have),
        Some(&want),
        0,
        65535,
        WriteGranularity::Byte,
    )
    .unwrap();

    // Only the first 4KB block should need erasing
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].start, 0);
    assert_eq!(ops[0].size, 4096);
}

#[test]
fn test_optimal_erase_offset_region() {
    // Test erasing a region that doesn't start at 0
    let erase_blocks = test_erase_blocks_4k_64k(1024 * 1024); // 1MB flash

    // Erase 8KB starting at 64KB offset
    let ops = plan_optimal_erase_region(&erase_blocks, 1024 * 1024, 65536, 73727).unwrap();

    // Should be 2 x 4KB blocks
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0].start, 65536);
    assert_eq!(ops[0].size, 4096);
    assert_eq!(ops[1].start, 69632);
    assert_eq!(ops[1].size, 4096);
}

#[test]
fn test_optimal_erase_empty_blocks() {
    // Empty erase blocks should return empty result
    assert!(matches!(
        plan_optimal_erase_region(&[], 65536, 0, 4095),
        Err(Error::InvalidAlignment)
    ));
}

#[test]
fn test_optimal_erase_chip_erase_when_beneficial() {
    // When erasing >50% of the chip and covering the full chip, use chip erase
    let flash_size = 1024 * 1024; // 1MB
    let mut erase_blocks = test_erase_blocks_4k_64k(flash_size);
    // Add chip erase
    erase_blocks.push(EraseBlock::new(0xC7, flash_size)); // Chip erase

    // Erase the entire chip
    let ops = plan_optimal_erase_region(&erase_blocks, flash_size, 0, flash_size - 1).unwrap();

    // Should use a single chip erase
    assert_eq!(ops.len(), 1, "Should use chip erase for full chip");
    assert_eq!(ops[0].start, 0);
    assert_eq!(ops[0].size, flash_size);
    assert_eq!(ops[0].erase_block.opcode, 0xC7);
}

#[test]
fn test_optimal_erase_chip_erase_smart_when_most_needs_erasing() {
    // When >50% needs erasing with smart erase, use chip erase
    // Use 256KB (4 x 64KB blocks) to ensure 64KB erase isn't mistaken for chip erase
    let flash_size = 256 * 1024; // 256KB
    let mut erase_blocks = test_erase_blocks_4k_64k(flash_size);
    erase_blocks.push(EraseBlock::new(0xC7, flash_size)); // Chip erase

    // Create have/want where 75% needs erasing (non-erased data changing)
    // First 75% has 0xAA that needs to change to 0xBB
    // Last 25% is unchanged (both have and want are 0xFF)
    let erase_portion = (flash_size as usize * 3) / 4; // 75%
    let mut have = vec![0xFF; flash_size as usize];
    let mut want = vec![0xFF; flash_size as usize];
    have[..erase_portion].fill(0xAA);
    want[..erase_portion].fill(0xBB);

    let ops = plan_optimal_erase(
        &erase_blocks,
        flash_size,
        Some(&have),
        Some(&want),
        0,
        flash_size - 1,
        WriteGranularity::Byte,
    )
    .unwrap();

    // Should use chip erase since >50% needs erasing
    assert_eq!(
        ops.len(),
        1,
        "Should use chip erase when >50% needs erasing, got {} ops",
        ops.len()
    );
    assert_eq!(ops[0].erase_block.opcode, 0xC7);
}

#[test]
fn test_optimal_erase_no_chip_erase_when_less_than_half() {
    // When <50% needs erasing, don't use chip erase
    // Use 256KB to ensure 64KB erase isn't mistaken for chip erase
    let flash_size = 256 * 1024; // 256KB
    let mut erase_blocks = test_erase_blocks_4k_64k(flash_size);
    erase_blocks.push(EraseBlock::new(0xC7, flash_size)); // Chip erase

    // Create have/want where only 40% needs erasing
    let mut have = vec![0xFF; flash_size as usize];
    let mut want = vec![0xFF; flash_size as usize];
    // Only the first 40% needs erasing (non-erased data changing)
    let erase_portion = (flash_size as usize * 2) / 5; // 40%
    have[..erase_portion].fill(0xAA);
    want[..erase_portion].fill(0xBB);

    let ops = plan_optimal_erase(
        &erase_blocks,
        flash_size,
        Some(&have),
        Some(&want),
        0,
        flash_size - 1,
        WriteGranularity::Byte,
    )
    .unwrap();

    // Should NOT use chip erase since <50% needs erasing
    assert!(
        !ops.iter().any(|op| op.erase_block.opcode == 0xC7),
        "Should not use chip erase when <50% needs erasing"
    );
    // Should use smaller block erases
    assert!(
        ops.len() > 1,
        "Should use multiple block erases, got {}",
        ops.len()
    );
}

#[test]
fn test_optimal_erase_no_chip_erase_for_partial_region() {
    // When erasing a partial region (not full chip), don't use chip erase
    let flash_size = 1024 * 1024; // 1MB
    let mut erase_blocks = test_erase_blocks_4k_64k(flash_size);
    erase_blocks.push(EraseBlock::new(0xC7, flash_size)); // Chip erase

    // Erase only 60% of the chip (which is >50%, but not full chip)
    let region_end = (flash_size as f32 * 0.6) as u32 - 1;
    let ops = plan_optimal_erase_region(&erase_blocks, flash_size, 0, region_end).unwrap();

    // Should NOT use chip erase since region doesn't cover full chip
    assert!(
        !ops.iter().any(|op| op.erase_block.opcode == 0xC7),
        "Should not use chip erase for partial region"
    );
}

use super::*;
use crate::spi::{AddressWidth, SpiCommand, opcodes};
use futures_lite::future::block_on;

struct Master {
    caps: SpiFeatures,
    denied: Vec<u8>,
    fail: Option<u8>,
    commands: Vec<(u8, AddressWidth, Option<u32>)>,
}
impl Default for Master {
    fn default() -> Self {
        Self {
            caps: SpiFeatures::FOUR_BYTE_ADDR,
            denied: vec![],
            fail: None,
            commands: vec![],
        }
    }
}
impl SpiMaster for Master {
    fn features(&self) -> SpiFeatures {
        self.caps
    }
    fn max_read_len(&self) -> usize {
        256
    }
    fn max_write_len(&self) -> usize {
        256
    }
    fn probe_opcode(&self, opcode: u8) -> bool {
        !self.denied.contains(&opcode)
    }
    async fn execute(&mut self, command: &mut SpiCommand<'_>) -> Result<()> {
        self.commands
            .push((command.opcode, command.address_width, command.address));
        if self.fail == Some(command.opcode) {
            return Err(Error::SpiTransferFailed);
        }
        command.read_buf.fill(0);
        Ok(())
    }
    async fn delay_us(&mut self, _: u32) {}
}

#[test]
fn mode_entered_legacy_erase_is_selected_before_native_not_after_failure() {
    let db = rflasher_chips::ChipDatabase::new();
    let chip = db
        .chips()
        .iter()
        .find(|chip| chip.name() == "MX66L51235F/MX25L51245G")
        .unwrap()
        .clone();
    assert!(chip.features.contains(Features::FOUR_BYTE_ENTER));
    let ctx = FlashContext::new(chip);
    let block = ctx
        .chip
        .erase_blocks()
        .iter()
        .find(|block| block.opcode == 0xd8)
        .unwrap()
        .clone();
    assert_eq!(block.opcode_4b, Some(0xdc)); // Keep the real silicon's capability.
    let op = OptimalEraseOp {
        start: 0x1000000,
        size: 65536,
        erase_block: block,
    };
    for fail in [None, Some(0xd8)] {
        let mut master = Master {
            fail,
            ..Default::default()
        };
        validate_spi_erase_operation(&master, &ctx, &op).unwrap();
        let result = block_on(erase_spi_unprotected(&mut master, &ctx, &op));
        assert_eq!(result.is_err(), fail.is_some());
        assert!(
            master
                .commands
                .contains(&(0xd8, AddressWidth::FourByte, Some(0x1000000)))
        );
        assert!(!master.commands.iter().any(|(opcode, _, _)| *opcode == 0xdc));
        assert_eq!(master.commands.first().unwrap().0, opcodes::EN4B);
        assert_eq!(master.commands.last().unwrap().0, opcodes::EX4B);
    }
}

#[test]
fn native_erase_remains_available_when_mode_switching_or_legacy_opcode_is_unusable() {
    let features = Features::FOUR_BYTE_ENTER | Features::FOUR_BYTE_ADDR;
    let block = EraseBlock::with_regions_and_4b(
        0xd8,
        Some(0xdc),
        &[crate::chip::EraseRegion::new(65536, 1024)],
    );
    for denied in [vec![opcodes::EN4B], vec![opcodes::EX4B], vec![0xd8]] {
        let master = Master {
            denied,
            ..Default::default()
        };
        let (opcode, addressing, enter) =
            select_erase_addressing(&master, features, true, &block).unwrap();
        assert_eq!(opcode, 0xdc);
        assert!(matches!(addressing, CommandAddressing::FourByte));
        assert!(!enter);
    }
    let master = Master {
        caps: SpiFeatures::FOUR_BYTE_ADDR | SpiFeatures::NO_4BA_MODES,
        ..Default::default()
    };
    assert_eq!(
        select_erase_addressing(&master, features, true, &block)
            .unwrap()
            .0,
        0xdc
    );
    assert_eq!(
        select_erase_addressing(&Master::default(), Features::empty(), true, &block)
            .unwrap()
            .0,
        0xdc
    );
}

#[test]
fn unavailable_mode_exit_fails_preflight_without_native_alternative() {
    let master = Master {
        denied: vec![opcodes::EX4B],
        ..Default::default()
    };
    let block = EraseBlock::with_count(0xd8, 65536, 1024);
    assert!(check_spi_eraser(&master, Features::FOUR_BYTE_ENTER, true, &block).is_err());
    assert!(master.commands.is_empty());
}

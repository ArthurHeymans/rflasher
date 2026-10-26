use super::*;
use crate::host::tests::{FakeHost, FakeSpiCommand};
use futures_lite::future::block_on;
use rflasher_core::flash::{FlashContext, FlashDevice, SpiFlashDevice};

fn controller() -> Spi100Controller<FakeHost> {
    Spi100Controller::new_with_host(FakeHost::default(), 0xfed8_0200, None, 0).unwrap()
}

#[test]
fn w25q256jw_reads_use_native_four_byte_commands() {
    let db = rflasher_chips::ChipDatabase::new();
    let chip = db.find_by_name("W25Q256JW")[0].clone();
    let ctx = FlashContext::new(chip);

    for addr in [0, 0x00ff_fff0, 0x0100_1234] {
        let controller = controller();
        assert_eq!(
            Controller::features(&controller),
            SpiFeatures::FOUR_BYTE_ADDR | SpiFeatures::NO_4BA_MODES
        );
        assert_eq!(
            SpiMaster::features(&controller),
            Controller::features(&controller)
        );

        let mut device = SpiFlashDevice::new(controller, ctx.clone());
        let mut data = [0; SPI100_MAX_DATA_READ + 3];
        block_on(device.read(addr, &mut data)).unwrap();
        let (controller, _) = device.into_parts();

        assert_eq!(
            controller.spibar.spi_commands(),
            vec![
                FakeSpiCommand {
                    opcode: opcodes::READ_4B,
                    write_data: addr.to_be_bytes().to_vec(),
                    read_len: SPI100_MAX_DATA_READ,
                },
                FakeSpiCommand {
                    opcode: opcodes::READ_4B,
                    write_data: (addr + SPI100_MAX_DATA_READ as u32).to_be_bytes().to_vec(),
                    read_len: 3,
                },
            ]
        );
    }
}

#[test]
fn controller_read_fallback_uses_chip_size_for_addressing() {
    let db = rflasher_chips::ChipDatabase::new();
    let small = db.find_by_name("W25Q128.V")[0].clone();
    let large = db.find_by_name("W25Q256JW")[0].clone();
    for (chip, addr, opcode, address) in [
        (&small, 0x1234, opcodes::READ, vec![0, 0x12, 0x34]),
        (&large, 0x1234, opcodes::READ_4B, vec![0, 0, 0x12, 0x34]),
        (
            &large,
            0x0100_1234,
            opcodes::READ_4B,
            vec![1, 0, 0x12, 0x34],
        ),
    ] {
        for no_4ba_mmap in [false, true] {
            let mut controller = controller();
            controller.no_4ba_mmap = no_4ba_mmap;
            controller.read(chip, addr, &mut [0; 4]).unwrap();
            assert_eq!(
                controller.spibar.spi_commands(),
                vec![FakeSpiCommand {
                    opcode,
                    write_data: address.clone(),
                    read_len: 4,
                }]
            );
        }
    }
}

#[test]
fn reads_the_controller_cannot_address_are_refused_before_any_command() {
    let db = rflasher_chips::ChipDatabase::new();
    let mut large = db.find_by_name("W25Q256JW")[0].clone();
    large
        .features
        .remove(rflasher_core::chip::Features::FOUR_BYTE_READ);
    let mut controller = controller();
    assert!(controller.read(&large, 0, &mut [0; 4]).is_err());
    // The opaque interface knows only the size, not native 4BA support.
    assert_eq!(
        Controller::controller_read(&mut controller, 0, &mut [0; 4], 32 << 20),
        Err(CoreError::ChipNotSupported)
    );
    assert!(controller.spibar.spi_commands().is_empty());
}

#[test]
fn partial_dummy_bytes_are_refused_before_any_command() {
    let mut controller = controller();
    let mut data = [0; 4];
    let mut cmd = SpiCommand::read_3b(opcodes::FAST_READ, 0, &mut data).with_dummy_cycles(7);
    assert_eq!(
        Controller::execute(&mut controller, &mut cmd),
        Err(CoreError::OpcodeNotSupported)
    );
    assert!(controller.spibar.spi_commands().is_empty());
}

#[test]
fn sfdp_dummy_byte_is_transmitted_before_receiving_data() {
    let mut controller = controller();
    controller.spibar.set_spi_response(b"SFDP");
    let mut data = [0; 4];
    block_on(rflasher_core::protocol::read_sfdp(
        &mut controller,
        0,
        &mut data,
    ))
    .unwrap();
    assert_eq!(&data, b"SFDP");
    assert_eq!(
        controller.spibar.spi_commands(),
        vec![FakeSpiCommand {
            opcode: opcodes::RDSFDP,
            write_data: vec![0, 0, 0, 0xff],
            read_len: 4,
        }]
    );
}

#[test]
fn fifo_limits_include_address_dummy_and_data_bytes() {
    let mut controller = controller();
    let mut data = [0; SPI100_MAX_DATA_READ];
    let mut cmd = SpiCommand::read_3b(opcodes::RDSFDP, 0, &mut data).with_dummy_cycles(8);
    Controller::execute(&mut controller, &mut cmd).unwrap();
    assert_eq!(controller.spibar.spi_commands().len(), 1);

    let mut data = [0; SPI100_MAX_DATA_READ + 1];
    let mut cmd = SpiCommand::read_3b(opcodes::RDSFDP, 0, &mut data).with_dummy_cycles(8);
    assert!(Controller::execute(&mut controller, &mut cmd).is_err());

    let data = [0xab; SPI100_MAX_DATA_WRITE];
    let mut cmd = SpiCommand::write_4b(opcodes::PP_4B, 0x0100_0000, &data);
    Controller::execute(&mut controller, &mut cmd).unwrap();
    let commands = controller.spibar.spi_commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(&commands[1].write_data[..4], &[1, 0, 0, 0]);
    assert_eq!(&commands[1].write_data[4..], &data);

    let data = [0xab; SPI100_MAX_DATA_WRITE + 1];
    let mut cmd = SpiCommand::write_4b(opcodes::PP_4B, 0x0100_0000, &data);
    assert!(Controller::execute(&mut controller, &mut cmd).is_err());
    assert_eq!(controller.spibar.spi_commands().len(), 2);
}

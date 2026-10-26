use super::*;
use crate::host::tests::{FakeHost, FakeSpiCommand};
use futures_lite::future::block_on;
use rflasher_core::flash::{FlashContext, FlashDevice, SpiFlashDevice};

fn controller() -> Spi100Controller<FakeHost> {
    Spi100Controller::new_with_host(FakeHost::default(), 0xfed8_0200, None, 0).unwrap()
}

fn context() -> FlashContext {
    let db = rflasher_chips::ChipDatabase::new();
    FlashContext::new(db.find_by_name("W25Q256JW")[0].clone())
}

fn mapped_controller() -> Spi100Controller<FakeHost> {
    let mut controller =
        Spi100Controller::new_with_host(FakeHost::default(), 0xfed8_0200, Some(0xffff_ff00), 256)
            .unwrap();
    let data: Vec<u8> = (0..256).map(|i| (i * 17) as u8).collect();
    controller.memory.as_ref().unwrap().set_bytes(0, &data);
    controller.spibar.set32(regs::ROM2_ADDR_OVERRIDE, 0x14c0);
    controller.spibar.set32(regs::ADDR32_CTRL0, 1);
    controller.check_4ba();
    controller
}

#[test]
fn w25q256jw_reads_use_native_four_byte_commands() {
    let ctx = context();

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
    for (chip_size, addr, opcode, address) in [
        (16 << 20, 0x1234, opcodes::READ, vec![0, 0x12, 0x34]),
        (32 << 20, 0x1234, opcodes::READ_4B, vec![0, 0, 0x12, 0x34]),
        (
            32 << 20,
            0x0100_1234,
            opcodes::READ_4B,
            vec![1, 0, 0x12, 0x34],
        ),
    ] {
        for no_4ba_mmap in [false, true] {
            let mut controller = controller();
            controller.no_4ba_mmap = no_4ba_mmap;
            controller.read(chip_size, addr, &mut [0; 4]).unwrap();
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

#[test]
fn fifo_reads_use_aligned_words_and_preserve_unaligned_responses() {
    let response: Vec<u8> = (0..67).map(|i| (i * 17) as u8).collect();
    for tx_len in 0..=4 {
        let controller = controller();
        controller.spibar.set_spi_response(&response);
        let mut data = [0; 67];
        controller
            .send_command(&[0; 5][..1 + tx_len], &mut data)
            .unwrap();
        assert_eq!(&data[..], &response);
        let reads: Vec<_> = controller
            .spibar
            .reads()
            .into_iter()
            .filter(|&(offset, _)| offset >= regs::FIFO_BASE)
            .collect();
        let prefix = (4 - tx_len % 4) % 4;
        assert_eq!(
            reads.iter().filter(|&&(_, width)| width == 4).count(),
            (67 - prefix) / 4
        );
        assert!(reads.iter().all(|&(offset, width)| {
            offset >= regs::FIFO_BASE + tx_len && offset + width <= regs::FIFO_BASE + tx_len + 67
        }));
    }
}

#[test]
fn flash_device_reads_use_the_checked_mapping_and_prepare_chip_mode() {
    use rflasher_core::chip::Features;

    for needs_wren in [false, true] {
        let mut ctx = context();
        if needs_wren {
            ctx.chip.features.remove(Features::FOUR_BYTE_ENTER);
            ctx.chip.features.insert(Features::FOUR_BYTE_ENTER_WREN);
        }
        let addr = ctx.chip.total_size - 255;
        let mut device = SpiFlashDevice::new(mapped_controller(), ctx);
        let mut data = [0; 75];
        block_on(device.read(addr, &mut data)).unwrap();
        let expected: Vec<u8> = (1..76).map(|i| (i * 17) as u8).collect();
        assert_eq!(&data[..], &expected);
        let (controller, _) = device.into_parts();
        let opcodes: Vec<_> = controller
            .spibar
            .spi_commands()
            .iter()
            .map(|c| c.opcode)
            .collect();
        assert_eq!(
            opcodes,
            if needs_wren {
                vec![opcodes::WREN, opcodes::EN4B]
            } else {
                vec![opcodes::EN4B]
            }
        );
        assert!(
            controller
                .memory
                .as_ref()
                .unwrap()
                .reads()
                .iter()
                .any(|&(_, width)| width == 8)
        );
    }
}

#[test]
fn reads_crossing_the_mapping_boundary_use_spi_only_for_the_prefix() {
    let controller = mapped_controller();
    controller.spibar.set_spi_response(&[0x5a; 17]);
    let ctx = context();
    let addr = ctx.chip.total_size - 256 - 17;
    let mut device = SpiFlashDevice::new(controller, ctx);
    let mut data = [0; 67];
    block_on(device.read(addr, &mut data)).unwrap();
    assert_eq!(&data[..17], &[0x5a; 17]);
    assert_eq!(
        &data[17..],
        &(0..50).map(|i| (i * 17) as u8).collect::<Vec<_>>()
    );
    let (controller, _) = device.into_parts();
    assert_eq!(
        controller.spibar.spi_commands(),
        vec![
            FakeSpiCommand {
                opcode: opcodes::EN4B,
                write_data: vec![],
                read_len: 0
            },
            FakeSpiCommand {
                opcode: opcodes::READ_4B,
                write_data: addr.to_be_bytes().to_vec(),
                read_len: 17
            },
        ]
    );
}

#[test]
fn unsafe_or_unusable_mappings_keep_using_native_spi_reads() {
    use rflasher_core::chip::Features;

    for (override_reg, addr32, page, supports_mode, low_address) in [
        (0, 1, 0, true, false),
        (0x14c0, 0, 0, true, false),
        (0x14c0, 1, 1, true, false),
        (0x14c0, 1, 0, false, false),
        (0x14c0, 1, 0, true, true),
    ] {
        let mut controller = mapped_controller();
        controller
            .spibar
            .set32(regs::ROM2_ADDR_OVERRIDE, override_reg);
        controller.spibar.set32(regs::ADDR32_CTRL0, addr32);
        controller.spibar.set32(regs::ADDR32_CTRL3, page);
        controller.check_4ba();
        controller.spibar.set_spi_response(&[0xcc; 16]);
        let mut ctx = context();
        if !supports_mode {
            ctx.chip
                .features
                .remove(Features::FOUR_BYTE_ENTER | Features::FOUR_BYTE_ENTER_WREN);
        }
        let addr = if low_address {
            0
        } else {
            ctx.chip.total_size - 256
        };
        let mut device = SpiFlashDevice::new(controller, ctx);
        let mut data = [0; 16];
        block_on(device.read(addr, &mut data)).unwrap();
        assert_eq!(data, [0xcc; 16]);
        let (controller, _) = device.into_parts();
        assert_eq!(
            controller.spibar.spi_commands(),
            vec![FakeSpiCommand {
                opcode: opcodes::READ_4B,
                write_data: addr.to_be_bytes().to_vec(),
                read_len: 16,
            }]
        );
        assert!(controller.memory.as_ref().unwrap().reads().is_empty());
    }
}

#[test]
fn mapped_read_errors_are_not_retried_as_spi_reads() {
    let controller = mapped_controller();
    controller
        .spibar
        .set32(regs::SPI_CNTRL0, spi_cntrl0_bits::ILLEGAL_ACCESS);
    let ctx = context();
    let addr = ctx.chip.total_size - 256;
    let mut device = SpiFlashDevice::new(controller, ctx);
    assert_eq!(
        block_on(device.read(addr, &mut [0; 16])),
        Err(CoreError::IoError)
    );
    let (controller, _) = device.into_parts();
    assert_eq!(controller.spibar.read8(regs::CMD_CODE), opcodes::EN4B);
    assert!(controller.memory.as_ref().unwrap().reads().is_empty());
}

#[test]
fn mapped_reads_validate_bounds_and_capabilities_before_preparing_chip_mode() {
    let ctx = context();
    let addr = ctx.chip.total_size - 1;
    let mut device = SpiFlashDevice::new(mapped_controller(), ctx);
    assert_eq!(
        block_on(device.read(addr, &mut [0; 2])),
        Err(CoreError::AddressOutOfBounds)
    );
    let (controller, _) = device.into_parts();
    assert!(controller.spibar.spi_commands().is_empty());
    assert!(controller.memory.as_ref().unwrap().reads().is_empty());

    let mut ctx = context();
    ctx.chip
        .features
        .remove(rflasher_core::chip::Features::FOUR_BYTE_READ);
    let addr = ctx.chip.total_size - 256;
    let mut device = SpiFlashDevice::new(mapped_controller(), ctx);
    assert_eq!(
        block_on(device.read(addr, &mut [0; 16])),
        Err(CoreError::ChipNotSupported)
    );
    let (controller, _) = device.into_parts();
    assert!(controller.spibar.spi_commands().is_empty());
    assert!(controller.memory.as_ref().unwrap().reads().is_empty());
}

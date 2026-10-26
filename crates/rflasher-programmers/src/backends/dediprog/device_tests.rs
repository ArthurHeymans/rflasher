use super::*;
use futures_lite::future::block_on;

#[test]
fn only_exact_status_reads_retry_control_stalls() {
    block_on(async {
        for (command, read_len) in [
            (&[opcodes::RDSR][..], 1),
            (&[opcodes::RDSR][..], 2),
            (&[opcodes::RDSR, 0][..], 1),
            (&[opcodes::RDSR2][..], 1),
            (&[opcodes::WREN][..], 0),
            (&[opcodes::WRDI][..], 0),
            (&[opcodes::WRSR, 0][..], 0),
            (&[opcodes::AAI_WP, 0, 0, 0, 0, 0][..], 0),
            (&[opcodes::PP, 0, 0, 0, 0][..], 0),
            (&[opcodes::SE_20, 0, 0, 0][..], 0),
        ] {
            let mut attempts = 0;
            let result =
                transceive_with_status_retry(command, read_len, STATUS_ACCESS_TIMEOUT, || {
                    attempts += 1;
                    std::future::ready(if attempts <= 2 {
                        Err(DediprogError::UsbTransfer(TransferError::Stall))
                    } else {
                        // Preserve the live WIP/AAI/protection bits for the caller.
                        Ok(vec![0x43; read_len])
                    })
                })
                .await;
            if command == [opcodes::RDSR] && read_len == 1 {
                assert_eq!(result.unwrap(), [0x43]);
                assert_eq!(attempts, 3);
            } else {
                assert!(matches!(
                    result,
                    Err(DediprogError::UsbTransfer(TransferError::Stall))
                ));
                assert_eq!(attempts, 1);
            }
        }
    });
}

#[test]
fn status_retries_are_bounded_and_preserve_failures() {
    block_on(async {
        let mut attempts = 0;
        let result =
            transceive_with_status_retry(&[opcodes::RDSR], 1, Duration::from_millis(1), || {
                attempts += 1;
                std::future::ready(Err(DediprogError::UsbTransfer(TransferError::Stall)))
            })
            .await;
        assert!(matches!(result, Err(DediprogError::Timeout)));
        assert_eq!(attempts, 1);

        for error in [
            TransferError::Disconnected,
            TransferError::Cancelled,
            TransferError::Fault,
            TransferError::InvalidArgument,
            TransferError::Unknown(42),
        ] {
            let mut attempts = 0;
            let result =
                transceive_with_status_retry(&[opcodes::RDSR], 1, STATUS_ACCESS_TIMEOUT, || {
                    attempts += 1;
                    std::future::ready(Err(DediprogError::UsbTransfer(error)))
                })
                .await;
            assert!(matches!(result, Err(DediprogError::UsbTransfer(e)) if e == error));
            assert_eq!(attempts, 1);
        }
        for response in [
            Err(DediprogError::Timeout),
            Err(DediprogError::TransferFailed("unclassified failure".into())),
            Ok(vec![]),
        ] {
            let mut responses = vec![response].into_iter();
            let result =
                transceive_with_status_retry(&[opcodes::RDSR], 1, STATUS_ACCESS_TIMEOUT, || {
                    std::future::ready(responses.next().expect("failure must not be retried"))
                })
                .await;
            assert!(matches!(
                result,
                Err(DediprogError::Timeout
                    | DediprogError::TransferFailed(_)
                    | DediprogError::InvalidResponse(_))
            ));
        }
    });
}

fn context(name: &str) -> FlashContext {
    let db = rflasher_chips::ChipDatabase::new();
    FlashContext::new(db.find_by_name(name)[0].clone())
}

/// The command packet and wValue/wIndex for one bulk transfer.
fn packet(protocol: Protocol, cmd: BulkCommand, start: u32) -> (Vec<u8>, u16, u16) {
    let mut packet = [0; MAX_CMD_SIZE];
    let (mut value, mut index) = (0, 0);
    let len =
        Dediprog::prepare_rw_cmd(protocol, &mut packet, &mut value, &mut index, cmd, start, 2)
            .unwrap();
    (packet[..len].to_vec(), value, index)
}

#[test]
fn bulk_split_keeps_unaligned_ranges_on_spi() {
    for (addr, len, block, expected) in [
        (0, 512, 512, (0, 512)),
        (1, 1024, 512, (511, 512)),
        (0, 513, 512, (0, 512)),
        (1, 1, 256, (1, 0)),
        (1, 512, 256, (255, 256)),
        (0, 0, 256, (0, 0)),
        ((1 << 24) + 1, 1024, 512, (511, 512)),
        (u32::MAX, 1, 256, (1, 0)),
    ] {
        assert_eq!(
            BulkCommand::split(addr, len, block),
            expected,
            "{addr:#x}+{len}"
        );
    }
}

#[test]
fn large_chips_use_only_native_four_byte_bulk_commands() {
    // Generic accesses above 16 MiB may leave the chip in another address
    // mode or extended-address bank, which three-byte bulk commands rely on.
    let mut ctx = context("W25Q256JW");
    for protocol in [Protocol::V1, Protocol::V2, Protocol::V3] {
        if let Some(cmd) = BulkCommand::read(protocol, &ctx) {
            assert!(cmd.four_byte, "{protocol:?}");
        }
        if let Some(cmd) = BulkCommand::write(protocol, &ctx) {
            assert!(cmd.four_byte, "{protocol:?}");
        }
    }
    assert_eq!(BulkCommand::read(Protocol::V1, &ctx), None);
    assert_eq!(BulkCommand::write(Protocol::V1, &ctx), None);
    ctx.chip.features.remove(Features::FOUR_BYTE_FAST_READ);
    // V2 has no READ_4B bulk mode.
    assert_eq!(BulkCommand::read(Protocol::V2, &ctx), None);
    ctx.chip
        .features
        .remove(Features::FOUR_BYTE_READ | Features::FOUR_BYTE_PROGRAM);
    assert_eq!(BulkCommand::read(Protocol::V3, &ctx), None);
    assert_eq!(BulkCommand::write(Protocol::V3, &ctx), None);
}

#[test]
fn four_byte_bulk_commands_match_flashprog() {
    let ctx = context("W25Q256JW");
    let addr = 0x0123_4500u32;
    let le = addr.to_le_bytes();
    for (protocol, read, write) in [
        // READ_MODE_4B_ADDR_FAST_0x0C with FAST_READ_4B.
        (
            Protocol::V2,
            [&[2, 0, 0, 5, 0x0c, 0][..], &le].concat(),
            [&[2, 0, 0, 11, 0x12, 0][..], &le].concat(),
        ),
        // Configurable read with four address bytes and 8 dummy clocks.
        (
            Protocol::V3,
            [&[2, 0, 0, 9, 0x0c, 0][..], &le, &[4, 4]].concat(),
            [&[2, 0, 0, 9, 0x12, 0][..], &le, &[0, 1, 0, 0]].concat(),
        ),
    ] {
        let cmd = BulkCommand::read(protocol, &ctx).unwrap();
        assert_eq!(packet(protocol, cmd, addr), (read, 0, 0));
        let cmd = BulkCommand::write(protocol, &ctx).unwrap();
        assert_eq!(packet(protocol, cmd, addr), (write, 0, 0));
    }

    // Protocol V3 falls back to READ_4B without dummy clocks.
    let mut ctx = context("W25Q256JW");
    ctx.chip.features.remove(Features::FOUR_BYTE_FAST_READ);
    let cmd = BulkCommand::read(Protocol::V3, &ctx).unwrap();
    assert_eq!(
        packet(Protocol::V3, cmd, addr).0,
        [&[2, 0, 0, 9, 0x13, 0][..], &le, &[4, 0]].concat()
    );
}

#[test]
fn three_byte_commands_are_refused_beyond_16_mib() {
    let cmd = BulkCommand::read(Protocol::V3, &context("W25Q128.V")).unwrap();
    let mut packet = [0; MAX_CMD_SIZE];
    let (mut value, mut index) = (0, 0);
    assert!(
        Dediprog::prepare_rw_cmd(
            Protocol::V3,
            &mut packet,
            &mut value,
            &mut index,
            cmd,
            1 << 24,
            1
        )
        .is_err()
    );
}

#[test]
fn bulk_write_mode_follows_chip_programming_algorithm() {
    let mode = |ctx: &FlashContext| {
        BulkCommand::write(Protocol::V3, ctx).map(|cmd| (cmd.mode, cmd.four_byte))
    };
    let mut ctx = context("W25Q128.V");
    assert_eq!(mode(&ctx), Some((WriteMode::PagePgm as u8, false)));
    ctx.chip.features.insert(Features::AAI_WORD);
    assert_eq!(mode(&ctx), Some((WriteMode::TwoByteAai as u8, false)));
    ctx.chip.features.remove(Features::AAI_WORD);
    ctx.chip.write_granularity = WriteGranularity::Byte;
    assert_eq!(mode(&ctx), None);
    ctx.chip.write_granularity = WriteGranularity::Page;
    ctx.chip.page_size = 512;
    assert_eq!(mode(&ctx), None);
}

#[test]
fn three_byte_packets_are_unchanged_on_every_protocol() {
    let mut ctx = context("W25Q128.V");
    for (protocol, read, write_len) in [
        (Protocol::V1, vec![2, 0, 0, 1, 0], 5),
        (
            Protocol::V2,
            vec![2, 0, 0, 2, 0x0b, 0, 0x00, 0x34, 0x12, 0],
            10,
        ),
        (
            Protocol::V3,
            vec![2, 0, 0, 9, 0x0b, 0, 0x00, 0x34, 0x12, 0, 3, 4],
            14,
        ),
    ] {
        let cmd = BulkCommand::read(protocol, &ctx).unwrap();
        assert_eq!(packet(protocol, cmd, 0x123400).0, read);
        for aai in [false, true] {
            ctx.chip.features.set(Features::AAI_WORD, aai);
            let cmd = BulkCommand::write(protocol, &ctx).unwrap();
            let (packet, value, index) = packet(protocol, cmd, 0x123400);
            assert_eq!(packet.len(), write_len);
            assert_eq!(&packet[..5], &[2, 0, 0, cmd.mode, 0]);
            if protocol == Protocol::V1 {
                assert_eq!((value, index), (0x3400, 0x12));
            } else {
                assert_eq!((value, index), (0, 0));
                assert_eq!(&packet[6..10], &0x123400u32.to_le_bytes());
            }
        }
        ctx.chip.features.remove(Features::AAI_WORD);
    }
}

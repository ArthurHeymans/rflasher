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

#[test]
fn write_splitting_preserves_bulk_and_residual_lengths() {
    for (addr, len, bulk_supported, expected) in [
        (0, 512, true, (0, 512)),
        (1, 512, true, (255, 256)),
        (1, 1, true, (1, 0)),
        (0, 513, true, (0, 512)),
        (0, 512, false, (512, 0)),
    ] {
        assert_eq!(
            Dediprog::write_lengths(addr, len, bulk_supported),
            Ok(expected)
        );
    }
}

#[test]
fn slow_write_ranges_must_fit_three_byte_addresses_before_any_programming() {
    const LIMIT: u32 = 1 << 24;
    for bulk_supported in [false, true] {
        // The last addressable byte is valid; an empty write touches nothing.
        assert_eq!(
            Dediprog::write_lengths(LIMIT - 1, 1, bulk_supported),
            Ok((1, 0))
        );
        assert_eq!(
            Dediprog::write_lengths(LIMIT, 0, bulk_supported),
            Ok((0, 0))
        );
        for (addr, len) in [
            (LIMIT, 1),
            (LIMIT + 1, 256),
            (LIMIT - 1, 2),
            // A valid bulk prefix must not be sent before rejecting its tail.
            (LIMIT - 256, 257),
            (u32::MAX, 1),
        ] {
            assert_eq!(
                Dediprog::write_lengths(addr, len, bulk_supported),
                Err(CoreError::WriteError { addr })
            );
        }
    }
    assert_eq!(
        Dediprog::write_lengths(LIMIT - 512, 512, false),
        Ok((512, 0))
    );
    assert_eq!(
        Dediprog::write_lengths(LIMIT, 512, false),
        Err(CoreError::WriteError { addr: LIMIT })
    );
}

#[test]
fn bulk_write_packets_preserve_aai_mode_on_every_protocol() {
    for (protocol, expected_len) in [(Protocol::V1, 5), (Protocol::V2, 10), (Protocol::V3, 14)] {
        for mode in [WriteMode::PagePgm, WriteMode::TwoByteAai] {
            let mut packet = [0; MAX_CMD_SIZE];
            let (mut value, mut index) = (0, 0);
            let len = Dediprog::prepare_rw_cmd(
                protocol,
                &mut packet,
                &mut value,
                &mut index,
                false,
                mode as u8,
                0x123400,
                2,
            )
            .unwrap();
            assert_eq!(len, expected_len);
            assert_eq!(&packet[..5], &[2, 0, 0, mode as u8, 0]);
            if protocol == Protocol::V1 {
                assert_eq!((value, index), (0x3400, 0x12));
            } else {
                assert_eq!((value, index), (0, 0));
                assert_eq!(&packet[6..10], &0x123400u32.to_le_bytes());
            }
        }
    }
}

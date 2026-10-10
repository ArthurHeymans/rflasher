use super::*;

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

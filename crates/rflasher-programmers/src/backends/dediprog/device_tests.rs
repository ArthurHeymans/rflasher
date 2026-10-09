use super::*;

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

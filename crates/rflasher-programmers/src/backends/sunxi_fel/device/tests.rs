use super::*;

#[test]
fn payload_headers_pair_native_opcodes_with_four_byte_addresses() {
    for (opcode, opcode_4b) in [
        (opcodes::READ, opcodes::READ_4B),
        (opcodes::PP, opcodes::PP_4B),
    ] {
        let (header, len) = addressed_header(opcode, opcode_4b, 0x0012_3456, false);
        assert_eq!(&header[..len], &[opcode, 0x12, 0x34, 0x56]);
        for addr in [0x0012_3456, 0x0112_3456] {
            let (header, len) = addressed_header(opcode, opcode_4b, addr, true);
            assert_eq!(len, 5);
            assert_eq!(header[0], opcode_4b);
            assert_eq!(&header[1..], &addr.to_be_bytes());
        }
    }
}

#[test]
fn writer_policy_keeps_incompatible_chips_on_spi() {
    let db = rflasher_chips::ChipDatabase::new();
    let mut chip = db.find_by_name("W25Q256JW")[0].clone();
    assert!(SunxiFel::supports_bulk_write(&chip));
    chip.features.remove(Features::FOUR_BYTE_PROGRAM);
    assert!(!SunxiFel::supports_bulk_write(&chip));
    // A three-byte chip needs no native four-byte program command.
    chip.total_size = 8 << 20;
    assert!(SunxiFel::supports_bulk_write(&chip));
    chip.features.insert(Features::AAI_WORD);
    assert!(!SunxiFel::supports_bulk_write(&chip));
    chip.features.remove(Features::AAI_WORD);
    chip.write_granularity = WriteGranularity::Byte;
    assert!(!SunxiFel::supports_bulk_write(&chip));
    chip.write_granularity = WriteGranularity::Page;
    chip.page_size = 512;
    assert!(!SunxiFel::supports_bulk_write(&chip));
}

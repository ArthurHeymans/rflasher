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
fn bulk_policy_keeps_incompatible_chips_on_spi() {
    let supports = |chip: &rflasher_core::chip::FlashChip| {
        let ctx = FlashContext::new(chip.clone());
        (
            SunxiFel::supports_bulk_read(&ctx),
            SunxiFel::supports_bulk_write(&ctx),
        )
    };
    let db = rflasher_chips::ChipDatabase::new();
    let mut chip = db.find_by_name("W25Q256JW")[0].clone();
    assert_eq!(supports(&chip), (true, true));
    chip.features.remove(Features::FOUR_BYTE_PROGRAM);
    assert_eq!(supports(&chip), (true, false));
    chip.features.remove(Features::FOUR_BYTE_READ);
    assert_eq!(supports(&chip), (false, false));
    // A three-byte chip needs no native four-byte commands.
    chip.total_size = 8 << 20;
    assert_eq!(supports(&chip), (true, true));
    chip.features.insert(Features::AAI_WORD);
    assert_eq!(supports(&chip), (true, false));
    chip.features.remove(Features::AAI_WORD);
    chip.write_granularity = WriteGranularity::Byte;
    assert_eq!(supports(&chip), (true, false));
    chip.write_granularity = WriteGranularity::Page;
    chip.page_size = 512;
    assert_eq!(supports(&chip), (true, false));
}

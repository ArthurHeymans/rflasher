use super::*;
use crate::flash::{ProbeOptions, probe_with_options};
use futures_lite::future::block_on;

struct Master {
    rdid: [u8; 3],
    rems: [u8; 2],
    at25f: [u8; 2],
    res: [u8; 3],
    fail: Option<u8>,
    unsupported: Option<u8>,
    commands: Vec<(u8, crate::spi::AddressWidth, usize)>,
}

impl Default for Master {
    fn default() -> Self {
        Self {
            rdid: [0xff; 3],
            rems: [0xff; 2],
            at25f: [0xff; 2],
            res: [0xff; 3],
            fail: None,
            unsupported: None,
            commands: Vec::new(),
        }
    }
}

impl SpiMaster for Master {
    fn features(&self) -> crate::programmer::SpiFeatures {
        crate::programmer::SpiFeatures::empty()
    }
    fn max_read_len(&self) -> usize {
        256
    }
    fn max_write_len(&self) -> usize {
        256
    }
    async fn execute(&mut self, command: &mut SpiCommand<'_>) -> Result<()> {
        self.commands.push((
            command.opcode,
            command.address_width,
            command.read_buf.len(),
        ));
        if self.fail == Some(command.opcode) {
            return Err(Error::SpiTransferFailed);
        }
        if self.unsupported == Some(command.opcode) {
            return Err(Error::OpcodeNotSupported);
        }
        let data: &[u8] = match command.opcode {
            0x9f => &self.rdid,
            0x90 => &self.rems,
            0x15 => &self.at25f,
            0xab => &self.res,
            0x5a => {
                command.read_buf.fill(0xff);
                return Ok(());
            }
            _ => panic!("unexpected probe opcode {:x}", command.opcode),
        };
        command
            .read_buf
            .copy_from_slice(&data[..command.read_buf.len()]);
        Ok(())
    }
    async fn delay_us(&mut self, _: u32) {}
}

#[test]
fn multiple_id_methods_on_one_chip_corroborate_and_disambiguate() {
    let mut db = rflasher_chips::ChipDatabase::empty();
    db.load_ron(
        r#"(vendor: "Test", manufacturer_id: 0xbf, chips: [
        (name: "Both", ids: [Rems(id: 0xBF48), Res2(id: 0xBF48)], total_size: KiB(64),
         erase_blocks: [(opcode: 0xc7, regions: [(size: KiB(64), count: 1)])]),
        (name: "OnlyRems", ids: [Rems(id: 0xBF48)], total_size: KiB(128),
         erase_blocks: [(opcode: 0xc7, regions: [(size: KiB(128), count: 1)])]),
    ])"#,
    )
    .unwrap();
    let mut master = Master {
        rems: [0xbf, 0x48],
        res: [0xbf, 0x48, 0],
        ..Default::default()
    };
    let result = block_on(probe_with_options(
        &mut master,
        &db,
        &ProbeOptions::default(),
    ))
    .unwrap();
    assert_eq!(result.chip.name(), "Both");
    assert_eq!(result.chip.ids.len(), 2);
    assert_eq!(result.identity, Some(ChipIdentity::Rems { id: 0xbf48 }));
    assert_eq!(
        master
            .commands
            .iter()
            .filter(|(opcode, _, _)| *opcode == 0x90)
            .count(),
        1
    );
    assert_eq!(
        master
            .commands
            .iter()
            .filter(|(opcode, _, _)| *opcode == 0xab)
            .count(),
        1
    );
}

#[test]
fn sst_rems_is_not_a_truncated_rdid() {
    let db = rflasher_chips::ChipDatabase::new();
    let mut master = Master {
        rdid: [0xbf, 0x48, 0xbf],
        rems: [0xbf, 0x48],
        ..Default::default()
    };
    let result = block_on(probe_with_options(
        &mut master,
        &db,
        &ProbeOptions::default(),
    ))
    .unwrap();
    assert_eq!(result.identity.unwrap(), ChipIdentity::Rems { id: 0xbf48 });
    assert_eq!(result.chip.jedec_device, 0x48);
    assert_eq!(result.chip.total_size, 65536);
    assert!(
        master
            .commands
            .contains(&(0x90, crate::spi::AddressWidth::ThreeByte, 2))
    );
    assert_eq!(
        master
            .commands
            .iter()
            .filter(|(op, _, _)| *op == 0x90)
            .count(),
        1
    );
    master.rems = [0xff; 2];
    assert!(matches!(
        block_on(probe_with_options(
            &mut master,
            &db,
            &ProbeOptions::default()
        )),
        Err(Error::ChipNotFound)
    ));
}

#[test]
fn genuine_rdid_is_not_overridden_by_legacy_reply_or_requested_name() {
    let db = rflasher_chips::ChipDatabase::new();
    let mut master = Master {
        rdid: [0xbf, 0x25, 0x41],
        rems: [0xbf, 0x48],
        ..Default::default()
    };
    let result = block_on(probe_with_options(
        &mut master,
        &db,
        &ProbeOptions::default(),
    ))
    .unwrap();
    assert_eq!(result.chip.name(), "SST25VF016B");
    assert!(!master.commands.iter().any(|(op, _, _)| *op == 0x90));
    let options = ProbeOptions {
        chip: Some("SST25VF512"),
        ..Default::default()
    };
    assert!(matches!(
        block_on(probe_with_options(&mut master, &db, &options)),
        Err(Error::ChipNotFound)
    ));
}

#[test]
fn legacy_transport_failures_abort_instead_of_selecting_another_method() {
    let db = rflasher_chips::ChipDatabase::new();
    for opcode in [0x90, 0x15, 0xab] {
        let mut master = Master {
            fail: Some(opcode),
            res: [0xbf, 0x44, 0],
            ..Default::default()
        };
        assert!(matches!(
            block_on(probe_with_options(
                &mut master,
                &db,
                &ProbeOptions::default()
            )),
            Err(Error::SpiTransferFailed)
        ));
    }
}

#[test]
fn legacy_method_conflicts_require_explicit_selection() {
    let db = rflasher_chips::ChipDatabase::new();
    let mut master = Master {
        rems: [0xbf, 0x48],
        res: [0xbf, 0x44, 0],
        ..Default::default()
    };
    assert!(matches!(
        block_on(probe_with_options(
            &mut master,
            &db,
            &ProbeOptions::default()
        )),
        Err(Error::ChipAmbiguous)
    ));
    let options = ProbeOptions {
        chip: Some("SST25VF512"),
        ..Default::default()
    };
    let result = block_on(probe_with_options(&mut master, &db, &options)).unwrap();
    assert_eq!(result.chip.name(), "SST25VF512");
}

#[test]
fn atmel_uses_its_own_identity_namespace() {
    let db = rflasher_chips::ChipDatabase::new();
    let mut master = Master {
        at25f: [0x1f, 0x63],
        ..Default::default()
    };
    let options = ProbeOptions {
        chip: Some("AT25F2048"),
        ..Default::default()
    };
    let result = block_on(probe_with_options(&mut master, &db, &options)).unwrap();
    assert_eq!(result.identity.unwrap(), ChipIdentity::At25f { id: 0x1f63 });
}

#[test]
fn unsupported_method_does_not_count_as_transport_failure() {
    let db = rflasher_chips::ChipDatabase::new();
    let mut master = Master {
        unsupported: Some(0x90),
        res: [0xbf, 0x44, 0],
        ..Default::default()
    };
    let result = block_on(probe_with_options(
        &mut master,
        &db,
        &ProbeOptions::default(),
    ))
    .unwrap();
    assert_eq!(result.chip.name(), "SST25LF040A");
    assert_eq!(result.identity.unwrap(), ChipIdentity::Res2 { id: 0xbf44 });
}

#[test]
fn one_byte_signature_is_suppressed_by_unknown_rdid_or_rems() {
    let mut db = rflasher_chips::ChipDatabase::empty();
    db.load_ron(
        r#"(vendor: "Test", manufacturer_id: 1, chips: [(
        name: "Signature", ids: [Res1(id: 0x12)], total_size: KiB(64),
        erase_blocks: [(opcode: 0xc7, regions: [(size: KiB(64), count: 1)])],
    )])"#,
    )
    .unwrap();
    let mut master = Master {
        res: [0x12, 0xff, 0xff],
        ..Default::default()
    };
    let result = block_on(probe_with_options(
        &mut master,
        &db,
        &ProbeOptions::default(),
    ))
    .unwrap();
    assert_eq!(result.identity.unwrap(), ChipIdentity::Res1 { id: 0x12 });
    master.rems = [0x42, 0x12];
    assert!(matches!(
        block_on(probe_with_options(
            &mut master,
            &db,
            &ProbeOptions::default()
        )),
        Err(Error::ChipNotFound)
    ));
    master.rems = [0xff; 2];
    master.rdid = [1, 2, 3];
    assert!(matches!(
        block_on(probe_with_options(
            &mut master,
            &db,
            &ProbeOptions::default()
        )),
        Err(Error::ChipNotFound)
    ));
}

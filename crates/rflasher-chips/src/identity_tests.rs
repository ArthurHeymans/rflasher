use super::*;
use rflasher_chip_types::ChipIdentity;

#[test]
fn compiled_and_runtime_catalogs_keep_probe_namespaces() {
    let compiled = ChipDatabase::new();
    let mut runtime = ChipDatabase::empty();
    runtime
        .load_ron(include_str!("../data/vendors/sst.ron"))
        .unwrap();
    for database in [&compiled, &runtime] {
        let identity = ChipIdentity::Rems { id: 0xbf48 };
        let names: Vec<_> = (0..)
            .map_while(|n| database.find_nth_by_identity(&identity, n))
            .map(FlashChip::name)
            .collect();
        assert_eq!(names, ["SST25VF512", "SST25VF512A", "SST25VF512(A)"]);
        for device in [0x48, 0x2548, 0x4800, 0x48bf] {
            assert!(database.find_by_jedec_id(0xbf, device).is_none());
        }
        assert!(
            database
                .find_nth_by_identity(&ChipIdentity::Res2 { id: 0xbf48 }, 0)
                .is_none()
        );
        assert!(
            database
                .find_nth_by_identity(&ChipIdentity::Rdid { id: 0x7fbf48 }, 0)
                .is_none()
        );
        let first = database.find_nth_by_identity(&identity, 0).unwrap();
        assert_eq!((first.jedec_manufacturer, first.jedec_device), (0xbf, 0x48));
    }
}

#[test]
fn one_definition_matches_multiple_value_carrying_methods() {
    let mut runtime = ChipDatabase::empty();
    runtime
        .load_ron(include_str!("../data/vendors/sst.ron"))
        .unwrap();
    for database in [ChipDatabase::new(), runtime] {
        let rems: Vec<_> = (0..)
            .map_while(|n| database.find_nth_by_identity(&ChipIdentity::Rems { id: 0xbf44 }, n))
            .collect();
        let res2 = database
            .find_nth_by_identity(&ChipIdentity::Res2 { id: 0xbf44 }, 0)
            .unwrap();
        assert_eq!(res2.name(), "SST25LF040A");
        assert!(rems.iter().any(|chip| core::ptr::eq(*chip, res2)));
        assert_eq!(res2.ids.len(), 2);
    }
}

#[test]
fn sst040_rdid_no_longer_selects_non_b_byte_program_profile() {
    let database = ChipDatabase::new();
    let names: Vec<_> = (0..)
        .map_while(|n| database.find_nth_by_jedec_id(0xbf, 0x258d, n))
        .map(FlashChip::name)
        .collect();
    assert_eq!(names, ["SST25VF040B"]);
}

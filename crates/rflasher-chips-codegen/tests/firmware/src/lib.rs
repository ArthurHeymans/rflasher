#![no_std]

pub mod catalog {
    include!(concat!(env!("OUT_DIR"), "/chips.rs"));
}

pub mod empty {
    include!(concat!(env!("OUT_DIR"), "/empty.rs"));
}

/// The actual firmware adapter path: platform HostAccess -> generic controller
/// -> catalog identification -> the existing SPI flash operation implementation.
/// This is compiled for bare metal, with and without allocation enabled.
pub async fn open<H: rflasher_internal::HostAccess>(
    mut controller: rflasher_internal::IchSpiController<H>,
) -> rflasher_core::error::Result<
    rflasher_core::flash::SpiFlashDevice<rflasher_internal::IchSpiController<H>>,
> {
    let context = rflasher_core::flash::probe_catalog(&mut controller, &catalog::CATALOG).await?;
    Ok(rflasher_core::flash::SpiFlashDevice::new(
        controller, context,
    ))
}

#[cfg(test)]
extern crate std;
#[cfg(test)]
mod integration;

#[cfg(test)]
mod tests {
    use super::{catalog::*, empty};
    use rflasher_core::chip::{
        ChipCatalog, ChipIdentity, ChipName, EraseBlock, EraseRegion, Features, ResolveError,
        Unlock,
    };

    #[test]
    fn generated_chips_preserve_geometry_and_capabilities() {
        const SST_CAPACITY: u32 = CHIPS[3].total_size;
        assert_eq!(SST_CAPACITY, 2 << 20);
        // One profile per identity: three selected entries share EF:4018.
        assert_eq!(CHIPS.len(), 4);
        let winbond = &CHIPS[0];
        assert_eq!(winbond.vendor(), "Winbond");
        assert_eq!(winbond.jedec_id(), 0xef4019);
        assert_eq!(winbond.total_size, 32 << 20);
        assert_eq!(winbond.page_size, 256);
        assert_eq!(winbond.unlock, Unlock::Status);
        assert_eq!(winbond.voltage_min_mv, 2700);
        assert!(winbond.features.contains(Features::FOUR_BYTE_NATIVE));
        assert_eq!(winbond.erase_blocks()[0].opcode_4b, Some(0x21));
        assert_eq!(winbond.erase_blocks()[0].total_size(), 32 << 20);
        let amic = &CHIPS[1];
        assert_eq!(amic.erase_blocks()[0].regions().len(), 5);
        assert_eq!(amic.erase_blocks()[0].total_size(), 128 << 10);
        assert_eq!(
            amic.erase_blocks()[0].block_size_at_boundary(64 << 10),
            Some(32 << 10)
        );
        assert!(amic.erase_blocks()[1].is_chip_erase());
    }

    #[test]
    fn each_identity_resolves_to_the_boards_profile() {
        // The default decided between the generic entry and two explicit
        // profiles when the catalog was generated; only it was shipped.
        let w25q128 = CATALOG.resolve(0xef, 0x4018).unwrap();
        assert_eq!(w25q128.name(), "W25Q128FVJV-WP");
        assert!(core::ptr::eq(w25q128, &CHIPS[2]));
        assert_eq!(
            CHIPS.iter().filter(|c| c.jedec_id() == 0xef4018).count(),
            1
        );
        assert!(core::ptr::eq(
            CATALOG.resolve(0xef, 0x4019).unwrap(),
            &CHIPS[0]
        ));
        assert_eq!(
            CATALOG.resolve(0xff, 0xffff).unwrap_err(),
            ResolveError::Unknown
        );
        assert_eq!(
            CATALOG.resolve(0x37, 0x2021).unwrap_err(),
            ResolveError::Unknown
        );
        let banked = CATALOG
            .resolve_identity(ChipIdentity::BankedJedec {
                bank: 1,
                manufacturer: 0x37,
                device: &[0x20, 0x21],
            })
            .unwrap();
        assert_eq!(banked.name(), "A25L10PT");
        assert!(empty::CHIPS.is_empty());
        assert_eq!(
            empty::CATALOG.resolve(0xef, 0x4018).unwrap_err(),
            ResolveError::Unknown
        );
    }

    #[test]
    fn a_hand_written_catalog_accepts_aliases_and_refuses_conflicts() {
        let first = CHIPS[3].clone();
        let mut second = first.clone();
        second.name = ChipName::borrowed("an alias");
        let mut chips = [first, second];
        assert!(core::ptr::eq(
            ChipCatalog::new(&chips).resolve(0xbf, 0x2541).unwrap(),
            &chips[0]
        ));
        chips[1].page_size = 16;
        assert_eq!(
            ChipCatalog::new(&chips).resolve(0xbf, 0x2541).unwrap_err(),
            ResolveError::Ambiguous
        );
    }

    #[test]
    fn legacy_identity_is_not_interchangeable_with_rdid() {
        let mut chip = CHIPS[3].clone();
        chip.features = Features::RES2_ID;
        chip.jedec_device = 0x41;
        let chips = [chip];
        let catalog = ChipCatalog::new(&chips);
        assert_eq!(
            catalog.resolve(0xbf, 0x41).unwrap_err(),
            ResolveError::Unknown
        );
        assert!(
            catalog
                .resolve_identity(ChipIdentity::Res2 {
                    manufacturer: 0xbf,
                    device: 0x41
                })
                .is_ok()
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn runtime_changes_copy_maps_without_mutating_the_static_catalog() {
        let original = &CHIPS[3];
        let mut chip = original.clone();
        assert_eq!(
            chip.erase_blocks().as_ptr(),
            original.erase_blocks().as_ptr()
        );
        chip.erase_blocks.to_mut()[0].regions.to_mut()[0].count = 1;
        assert_eq!(chip.erase_blocks()[0].regions()[0].count, 1);
        assert_eq!(original.erase_blocks()[0].regions()[0].count, 512);
        assert_ne!(
            chip.erase_blocks().as_ptr(),
            original.erase_blocks().as_ptr()
        );
    }

    #[test]
    fn static_erase_maps_are_compact_and_unbounded() {
        const BLOCK: EraseBlock =
            EraseBlock::from_static_regions(0xd8, None, &[EraseRegion::new(4096, 1); 9]);
        assert_eq!(BLOCK.regions().len(), 9);
        assert_eq!(BLOCK.total_size(), 9 * 4096);
        assert!(core::mem::size_of::<EraseBlock>() <= 40);
    }
}

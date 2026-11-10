//! Flash chip database lookup abstraction.

use super::FlashChip;

/// A source of flash chip definitions.
///
/// Providers may load chip definitions at runtime, compile them into the
/// application, or obtain them from another source. Flash probing only needs
/// JEDEC ID lookup, so the core trait intentionally exposes a small API.
/// This is low-level enumeration, not identification policy: use
/// [`crate::ChipCatalog`] to resolve board defaults and conflicting profiles.
pub trait ChipProvider {
    /// Find a chip by its JEDEC manufacturer and device IDs.
    ///
    /// JEDEC IDs are not unique: several database entries can share one (for
    /// example a boot-block and a uniform-sector variant of the same family).
    /// This returns the first of them; use [`find_nth_by_jedec_id`] to see all
    /// candidates before committing to one.
    ///
    /// [`find_nth_by_jedec_id`]: ChipProvider::find_nth_by_jedec_id
    fn find_by_jedec_id(&self, manufacturer: u8, device: u16) -> Option<&FlashChip>;

    /// Find a candidate for an RDID identity in a JEDEC continuation bank.
    /// `device` contains exactly the identity bytes, not trailing response bytes.
    /// Providers that do not support banked identities return no candidates.
    fn find_nth_by_banked_jedec_id(
        &self,
        _bank: u8,
        _manufacturer: u8,
        _device: &[u8],
        _index: usize,
    ) -> Option<&FlashChip> {
        None
    }

    /// Find the `index`-th candidate for a two-byte legacy RES identity.
    /// Providers without audited RES2 identities return no candidates.
    fn find_nth_by_res2_id(
        &self,
        _manufacturer: u8,
        _device: u8,
        _index: usize,
    ) -> Option<&FlashChip> {
        None
    }

    /// Find the `index`-th (0-based, database order) chip with the given JEDEC IDs.
    ///
    /// Callers enumerate candidates by increasing `index` until `None`. The
    /// default implementation knows only the single chip returned by
    /// [`find_by_jedec_id`](ChipProvider::find_by_jedec_id).
    fn find_nth_by_jedec_id(
        &self,
        manufacturer: u8,
        device: u16,
        index: usize,
    ) -> Option<&FlashChip> {
        if index == 0 {
            self.find_by_jedec_id(manufacturer, device)
        } else {
            None
        }
    }
}

impl<const N: usize> ChipProvider for [FlashChip; N] {
    fn find_by_jedec_id(&self, manufacturer: u8, device: u16) -> Option<&FlashChip> {
        self.as_slice().find_by_jedec_id(manufacturer, device)
    }

    fn find_nth_by_jedec_id(
        &self,
        manufacturer: u8,
        device: u16,
        index: usize,
    ) -> Option<&FlashChip> {
        self.as_slice()
            .find_nth_by_jedec_id(manufacturer, device, index)
    }

    fn find_nth_by_res2_id(
        &self,
        manufacturer: u8,
        device: u8,
        index: usize,
    ) -> Option<&FlashChip> {
        self.as_slice()
            .find_nth_by_res2_id(manufacturer, device, index)
    }

    fn find_nth_by_banked_jedec_id(
        &self,
        bank: u8,
        manufacturer: u8,
        device: &[u8],
        index: usize,
    ) -> Option<&FlashChip> {
        self.as_slice()
            .find_nth_by_banked_jedec_id(bank, manufacturer, device, index)
    }
}

/// Borrowed catalogs work directly with probing, without allocation.
impl ChipProvider for [FlashChip] {
    fn find_by_jedec_id(&self, manufacturer: u8, device: u16) -> Option<&FlashChip> {
        self.find_nth_by_jedec_id(manufacturer, device, 0)
    }

    fn find_nth_by_jedec_id(
        &self,
        manufacturer: u8,
        device: u16,
        index: usize,
    ) -> Option<&FlashChip> {
        self.iter()
            .filter(|c| c.matches_jedec_id(manufacturer, device))
            .nth(index)
    }

    fn find_nth_by_res2_id(
        &self,
        manufacturer: u8,
        device: u8,
        index: usize,
    ) -> Option<&FlashChip> {
        self.iter()
            .filter(|c| {
                c.jedec_manufacturer == manufacturer
                    && c.jedec_device == u16::from(device)
                    && c.features.contains(crate::Features::RES2_ID)
            })
            .nth(index)
    }

    fn find_nth_by_banked_jedec_id(
        &self,
        bank: u8,
        manufacturer: u8,
        device: &[u8],
        index: usize,
    ) -> Option<&FlashChip> {
        // Audited bank-one identities: PMC has one device byte, AMIC two.
        let id = match (bank, manufacturer, device) {
            (1, 0x9d, [id]) => u16::from(*id),
            (1, 0x37, [hi, lo]) => u16::from_be_bytes([*hi, *lo]),
            _ => return None,
        };
        self.iter()
            .filter(|c| {
                c.jedec_manufacturer == manufacturer
                    && c.jedec_device == id
                    && c.features.contains(crate::Features::RDID_CONTINUATION)
                    && !c
                        .features
                        .intersects(crate::Features::RES2_ID | crate::Features::AT25F_ID)
            })
            .nth(index)
    }
}

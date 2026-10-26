//! Flash chip database lookup abstraction.

use super::FlashChip;

/// A source of flash chip definitions.
///
/// Providers may load chip definitions at runtime, compile them into the
/// application, or obtain them from another source. Flash probing only needs
/// JEDEC ID lookup, so the core trait intentionally exposes a small API.
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

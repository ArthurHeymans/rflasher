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

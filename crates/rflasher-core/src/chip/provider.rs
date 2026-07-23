//! Flash chip database lookup abstraction.

use super::FlashChip;

/// A source of flash chip definitions.
///
/// Providers may load chip definitions at runtime, compile them into the
/// application, or obtain them from another source. Flash probing only needs
/// JEDEC ID lookup, so the core trait intentionally exposes a small API.
pub trait ChipProvider {
    /// Find a chip by its JEDEC manufacturer and device IDs.
    fn find_by_jedec_id(&self, manufacturer: u8, device: u16) -> Option<&FlashChip>;
}

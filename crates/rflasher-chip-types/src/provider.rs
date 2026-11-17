//! Flash chip database lookup abstraction.

use super::{ChipIdentity, FlashChip};

/// A source of flash chip definitions.
///
/// Providers may load chip definitions at runtime, compile them into the
/// application, or obtain them from another source. Flash probing only needs
/// JEDEC ID lookup, so the core trait intentionally exposes a small API.
/// This is low-level enumeration, not identification policy: a board's
/// [`crate::ChipCatalog`] holds one approved profile per identity.
pub trait ChipProvider {
    /// Whether the catalog contains audited identities for a probe method.
    /// Defaults retain the legacy RES2 fallback for existing providers.
    fn supports_probe_method(&self, method: super::ProbeMethod) -> bool {
        matches!(method, super::ProbeMethod::Rdid | super::ProbeMethod::Res2)
    }

    /// Find a candidate in an explicit SPI identification namespace.
    /// Existing providers retain their RDID/banked/RES2 implementations; other
    /// methods are opt-in and never reuse ordinary RDID lookup.
    fn find_nth_by_identity(&self, id: &super::ChipIdentity, index: usize) -> Option<&FlashChip> {
        use super::ChipIdentity;
        if !id.is_valid() {
            return None;
        }
        match *id {
            ChipIdentity::Rdid { id } if id >> 24 == 0x7f => self.find_nth_by_banked_jedec_id(
                1,
                (id >> 16) as u8,
                &(id as u16).to_be_bytes(),
                index,
            ),
            ChipIdentity::Rdid { id } if id >> 16 == 0x7f => {
                self.find_nth_by_banked_jedec_id(1, (id >> 8) as u8, &[id as u8], index)
            }
            ChipIdentity::Rdid { id } => {
                self.find_nth_by_jedec_id((id >> 16) as u8, id as u16, index)
            }
            ChipIdentity::Res2 { id } => self.find_nth_by_res2_id((id >> 8) as u8, id as u8, index),
            _ => None,
        }
    }

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
    fn supports_probe_method(&self, method: super::ProbeMethod) -> bool {
        self.as_slice().supports_probe_method(method)
    }

    fn find_nth_by_identity(&self, id: &ChipIdentity, index: usize) -> Option<&FlashChip> {
        self.as_slice().find_nth_by_identity(id, index)
    }

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
    fn supports_probe_method(&self, method: super::ProbeMethod) -> bool {
        self.iter()
            .any(|chip| chip.identities().any(|id| id.method() == method))
    }

    fn find_nth_by_identity(&self, id: &ChipIdentity, index: usize) -> Option<&FlashChip> {
        self.iter()
            .filter(|chip| chip.matches_identity(id))
            .nth(index)
    }

    fn find_by_jedec_id(&self, manufacturer: u8, device: u16) -> Option<&FlashChip> {
        self.find_nth_by_jedec_id(manufacturer, device, 0)
    }

    fn find_nth_by_jedec_id(
        &self,
        manufacturer: u8,
        device: u16,
        index: usize,
    ) -> Option<&FlashChip> {
        if manufacturer == 0x7f {
            return None;
        }
        self.find_nth_by_identity(
            &ChipIdentity::Rdid {
                id: (u32::from(manufacturer) << 16) | u32::from(device),
            },
            index,
        )
    }

    fn find_nth_by_res2_id(
        &self,
        manufacturer: u8,
        device: u8,
        index: usize,
    ) -> Option<&FlashChip> {
        self.find_nth_by_identity(
            &ChipIdentity::Res2 {
                id: (u16::from(manufacturer) << 8) | u16::from(device),
            },
            index,
        )
    }

    fn find_nth_by_banked_jedec_id(
        &self,
        bank: u8,
        manufacturer: u8,
        device: &[u8],
        index: usize,
    ) -> Option<&FlashChip> {
        self.find_nth_by_identity(
            &ChipIdentity::banked_rdid(bank, manufacturer, device)?,
            index,
        )
    }
}

//! Allocation-free identification against a board's chip catalog.

use crate::{ChipIdentity, FlashChip};

/// An identity cannot safely be reduced to one operation profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// No entry matches the observed identity.
    Unknown,
    /// Matching entries disagree on operations.
    Ambiguous,
}

impl core::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "unknown flash chip identity",
            Self::Ambiguous => "ambiguous flash chip identity; keep one profile per identity",
        })
    }
}
impl core::error::Error for ResolveError {}

/// The operation profiles a board accepts, one per chip identity.
///
/// A generated catalog is reduced to that at build time: the board's default
/// profiles decide between entries sharing an identity, and a selection that
/// leaves a conflict fails the build. Every entry of a catalog is approved for
/// use, explicit profiles included. Entries that share an identity and operate
/// alike are aliases; a hand-written catalog whose entries disagree is refused
/// when that identity is observed, rather than answered by list order.
#[derive(Debug, Clone, Copy)]
pub struct ChipCatalog<'a> {
    chips: &'a [FlashChip],
}

impl<'a> ChipCatalog<'a> {
    /// Declare the profiles a board accepts.
    pub const fn new(chips: &'a [FlashChip]) -> Self {
        Self { chips }
    }

    /// The catalog's descriptors.
    pub const fn chips(&self) -> &'a [FlashChip] {
        self.chips
    }

    /// Resolve a standard three-byte JEDEC identity.
    pub fn resolve(&self, manufacturer: u8, device: u16) -> Result<&'a FlashChip, ResolveError> {
        if manufacturer == 0x7f {
            return Err(ResolveError::Unknown);
        }
        self.resolve_identity(ChipIdentity::Rdid {
            id: (u32::from(manufacturer) << 16) | u32::from(device),
        })
    }

    /// Resolve an observed identity without allocation or guessed procedures.
    pub fn resolve_identity(&self, identity: ChipIdentity) -> Result<&'a FlashChip, ResolveError> {
        let mut matches = self.chips.iter().filter(|c| c.matches_identity(&identity));
        let first = matches.next().ok_or(ResolveError::Unknown)?;
        if matches.all(|other| first.is_equivalent_to(other)) {
            Ok(first)
        } else {
            Err(ResolveError::Ambiguous)
        }
    }
}

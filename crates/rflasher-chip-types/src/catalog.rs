//! Allocation-free identification and board-profile selection.

use crate::{ChipProvider, Features, FlashChip};

/// A positively read identity, including the protocol that produced it.
#[derive(Debug, Clone, Copy)]
pub enum ChipIdentity<'a> {
    /// Three-byte, non-continuation RDID response.
    Jedec {
        /// JEDEC manufacturer byte, not a continuation code.
        manufacturer: u8,
        /// Two device identity bytes, big-endian.
        device: u16,
    },
    /// Continuation-banked RDID; device contains exactly the identity bytes.
    BankedJedec {
        /// Number of JEDEC continuation bytes.
        bank: u8,
        /// Manufacturer in that bank.
        manufacturer: u8,
        /// Exactly the device identity bytes, without repeated response bytes.
        device: &'a [u8],
    },
    /// Legacy two-byte RES identity (not interchangeable with RDID).
    Res2 {
        /// Legacy manufacturer identity byte.
        manufacturer: u8,
        /// Legacy device identity byte.
        device: u8,
    },
}

/// An identity cannot safely be reduced to one operation profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// No eligible entry matches the observed identity.
    Unknown,
    /// Matching entries or matching board defaults disagree on operations.
    Ambiguous,
}

impl core::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "unknown flash chip identity",
            Self::Ambiguous => "ambiguous flash chip identity; select a board profile",
        })
    }
}
impl core::error::Error for ResolveError {}

/// Selected descriptors and the board's explicitly approved default profiles.
///
/// Defaults only choose among entries matching the observed identity. They
/// never make an unknown chip writable. Explicit profiles require an exact
/// board selection in `defaults`; merely including them is not authorization.
#[derive(Debug, Clone, Copy)]
pub struct ChipCatalog<'a> {
    chips: &'a [FlashChip],
    defaults: &'a [&'a FlashChip],
}

impl<'a> ChipCatalog<'a> {
    /// Declare a catalog. Default references must point into `chips`.
    /// Generated catalogs establish this relation at build time.
    pub const fn new(chips: &'a [FlashChip], defaults: &'a [&'a FlashChip]) -> Self {
        Self { chips, defaults }
    }

    /// Descriptors for diagnostics or enumeration, not automatic selection.
    pub fn chips(&self) -> &'a [FlashChip] {
        self.chips
    }

    /// Resolve a standard three-byte JEDEC identity.
    pub fn resolve(&self, manufacturer: u8, device: u16) -> Result<&'a FlashChip, ResolveError> {
        self.resolve_identity(ChipIdentity::Jedec {
            manufacturer,
            device,
        })
    }

    /// Resolve an observed identity without allocation or guessed procedures.
    ///
    /// Equivalent aliases are interchangeable. A single matching board default
    /// takes precedence over conflicting candidates. Conflicting matching
    /// defaults remain ambiguous. Non-default explicit profiles are excluded.
    pub fn resolve_identity(
        &self,
        identity: ChipIdentity<'_>,
    ) -> Result<&'a FlashChip, ResolveError> {
        let mut first: Option<&FlashChip> = None;
        let mut preferred: Option<&FlashChip> = None;
        let mut conflict = false;
        for index in 0.. {
            let candidate = match identity {
                ChipIdentity::Jedec {
                    manufacturer,
                    device,
                } => self.chips.find_nth_by_jedec_id(manufacturer, device, index),
                ChipIdentity::BankedJedec {
                    bank,
                    manufacturer,
                    device,
                } => self
                    .chips
                    .find_nth_by_banked_jedec_id(bank, manufacturer, device, index),
                ChipIdentity::Res2 {
                    manufacturer,
                    device,
                } => self.chips.find_nth_by_res2_id(manufacturer, device, index),
            };
            let Some(candidate) = candidate else {
                break;
            };
            let is_default = self.defaults.iter().any(|d| core::ptr::eq(*d, candidate));
            if is_default {
                if preferred.is_some_and(|p| !p.is_equivalent_to(candidate)) {
                    return Err(ResolveError::Ambiguous);
                }
                preferred.get_or_insert(candidate);
            }
            if candidate.features.contains(Features::EXPLICIT_PROFILE) && !is_default {
                continue;
            }
            if first.is_some_and(|f| !f.is_equivalent_to(candidate)) {
                conflict = true;
            }
            first.get_or_insert(candidate);
        }
        if let Some(preferred) = preferred {
            return Ok(preferred);
        }
        if conflict {
            return Err(ResolveError::Ambiguous);
        }
        first.ok_or(ResolveError::Unknown)
    }
}

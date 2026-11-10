//! Allocation-free identification against a board's chip catalog.

use crate::{Features, FlashChip};

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

impl FlashChip {
    /// Whether this descriptor is identified by `identity`, read with the
    /// protocol the descriptor expects.
    #[must_use]
    pub fn matches_identity(&self, identity: ChipIdentity<'_>) -> bool {
        match identity {
            ChipIdentity::Jedec {
                manufacturer,
                device,
            } => self.matches_jedec_id(manufacturer, device),
            ChipIdentity::BankedJedec {
                bank,
                manufacturer,
                device,
            } => {
                // Audited bank-one identities: PMC has one device byte, AMIC two.
                let id = match (bank, manufacturer, device) {
                    (1, 0x9d, [id]) => u16::from(*id),
                    (1, 0x37, [hi, lo]) => u16::from_be_bytes([*hi, *lo]),
                    _ => return false,
                };
                self.jedec_manufacturer == manufacturer
                    && self.jedec_device == id
                    && self.features.contains(Features::RDID_CONTINUATION)
                    && !self
                        .features
                        .intersects(Features::RES2_ID | Features::AT25F_ID)
            }
            ChipIdentity::Res2 {
                manufacturer,
                device,
            } => {
                self.jedec_manufacturer == manufacturer
                    && self.jedec_device == u16::from(device)
                    && self.features.contains(Features::RES2_ID)
            }
        }
    }
}

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
        self.resolve_identity(ChipIdentity::Jedec {
            manufacturer,
            device,
        })
    }

    /// Resolve an observed identity without allocation or guessed procedures.
    pub fn resolve_identity(
        &self,
        identity: ChipIdentity<'_>,
    ) -> Result<&'a FlashChip, ResolveError> {
        let mut matches = self.chips.iter().filter(|c| c.matches_identity(identity));
        let first = matches.next().ok_or(ResolveError::Unknown)?;
        if matches.all(|other| first.is_equivalent_to(other)) {
            Ok(first)
        } else {
            Err(ResolveError::Ambiguous)
        }
    }
}

//! SPI identity methods carry their values; a chip can advertise several.

/// Command and response format used to identify a chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeMethod {
    /// JEDEC RDID (0x9f), including a continuation prefix.
    Rdid,
    /// Manufacturer/device ID (0x90 + three address bytes).
    Rems,
    /// One-byte electronic signature (0xab).
    Res1,
    /// Two-byte manufacturer/device signature (0xab).
    Res2,
    /// Three-byte continuation/manufacturer/device signature (0xab).
    Res3,
    /// Older Atmel manufacturer/device ID (0x15).
    At25f,
}

/// An exact SPI identity in its command namespace, in wire byte order.
///
/// REMS/RES2/AT25F values include both manufacturer and device bytes. RDID
/// includes any leading 0x7f: e.g. EF4018, 7F9D21 or 7F372020. A repeated
/// trailing byte is not part of a shorter identity. Multiple entries in a
/// chip's `ids` list are alternative ways to identify that same chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ChipIdentity {
    /// Three-byte JEDEC ID, or up to four bytes with a continuation prefix.
    Rdid {
        /// Complete RDID identity.
        id: u32,
    },
    /// Two-byte manufacturer/device identity.
    Rems {
        /// Manufacturer in the high byte, device in the low byte.
        id: u16,
    },
    /// One-byte device signature (no manufacturer).
    Res1 {
        /// Electronic signature.
        id: u8,
    },
    /// Two-byte manufacturer/device signature.
    Res2 {
        /// Manufacturer in the high byte, device in the low byte.
        id: u16,
    },
    /// Three-byte continuation/manufacturer/device signature.
    Res3 {
        /// Complete signature, including its 0x7f prefix.
        id: u32,
    },
    /// Two-byte older Atmel identity.
    At25f {
        /// Manufacturer in the high byte, device in the low byte.
        id: u16,
    },
}

impl ChipIdentity {
    /// Adapt old catalog identification flags without reinterpreting namespaces.
    pub const fn from_legacy(
        manufacturer: u8,
        device: u16,
        at25f: bool,
        res2: bool,
        banked: bool,
    ) -> Option<Self> {
        let pair = ((manufacturer as u16) << 8) | device;
        if at25f {
            if device <= 0xff {
                Some(Self::At25f { id: pair })
            } else {
                None
            }
        } else if res2 {
            if device <= 0xff {
                Some(Self::Res2 { id: pair })
            } else {
                None
            }
        } else if banked {
            if manufacturer == 0x9d {
                if device <= 0xff {
                    Some(Self::Rdid {
                        id: 0x7f0000 | pair as u32,
                    })
                } else {
                    None
                }
            } else {
                Some(Self::Rdid {
                    id: 0x7f000000 | ((manufacturer as u32) << 16) | device as u32,
                })
            }
        } else {
            Some(Self::Rdid {
                id: ((manufacturer as u32) << 16) | device as u32,
            })
        }
    }

    /// Frame a bank-one response using exactly its meaningful device bytes.
    pub const fn banked_rdid(bank: u8, manufacturer: u8, device: &[u8]) -> Option<Self> {
        match (bank, device) {
            (1, [id]) => Some(Self::Rdid {
                id: 0x7f0000 | ((manufacturer as u32) << 8) | *id as u32,
            }),
            (1, [hi, lo]) => Some(Self::Rdid {
                id: 0x7f000000 | ((manufacturer as u32) << 16) | ((*hi as u32) << 8) | *lo as u32,
            }),
            _ => None,
        }
    }

    /// The identification command namespace.
    pub const fn method(self) -> ProbeMethod {
        match self {
            Self::Rdid { .. } => ProbeMethod::Rdid,
            Self::Rems { .. } => ProbeMethod::Rems,
            Self::Res1 { .. } => ProbeMethod::Res1,
            Self::Res2 { .. } => ProbeMethod::Res2,
            Self::Res3 { .. } => ProbeMethod::Res3,
            Self::At25f { .. } => ProbeMethod::At25f,
        }
    }

    /// Whether a value fits the documented response framing.
    pub const fn is_valid(self) -> bool {
        match self {
            Self::Rdid { id } => id <= 0xff_ffff || id >> 24 == 0x7f,
            Self::Res3 { id } => id <= 0xff_ffff && id >> 16 == 0x7f,
            _ => true,
        }
    }

    /// Manufacturer/device values for catalog diagnostics, excluding prefixes.
    pub const fn manufacturer_device(self) -> (u8, u16) {
        match self {
            Self::Rdid { id } if id >> 16 == 0x7f => ((id >> 8) as u8, (id & 0xff) as u16),
            Self::Rdid { id } => ((id >> 16) as u8, id as u16),
            Self::Rems { id } | Self::Res2 { id } | Self::At25f { id } => {
                ((id >> 8) as u8, id & 0xff)
            }
            Self::Res1 { id } => (0, id as u16),
            Self::Res3 { id } => ((id >> 8) as u8, (id & 0xff) as u16),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChipProvider, FlashChip};
    use core::cell::Cell;

    struct Provider(Cell<usize>);
    impl ChipProvider for Provider {
        fn find_by_jedec_id(&self, _: u8, _: u16) -> Option<&FlashChip> {
            None
        }
        fn find_nth_by_banked_jedec_id(
            &self,
            _: u8,
            _: u8,
            _: &[u8],
            _: usize,
        ) -> Option<&FlashChip> {
            self.0.set(self.0.get() + 1);
            None
        }
        fn find_nth_by_res2_id(&self, _: u8, _: u8, _: usize) -> Option<&FlashChip> {
            self.0.set(self.0.get() + 1);
            None
        }
    }

    #[test]
    fn compatibility_lookup_rejects_invalid_rdid_framing() {
        let provider = Provider(Cell::new(0));
        assert!(
            provider
                .find_nth_by_identity(&ChipIdentity::Rdid { id: 0x12345678 }, 0)
                .is_none()
        );
        assert_eq!(provider.0.get(), 0);
        assert!(
            provider
                .find_nth_by_identity(&ChipIdentity::Res2 { id: 0xbf44 }, 0)
                .is_none()
        );
        assert_eq!(provider.0.get(), 1);
    }
}

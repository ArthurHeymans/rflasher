//! The regions of an erase operation.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::ops::Deref;

use crate::EraseRegion;

/// The regions one erase opcode covers, in address order.
///
/// Nearly every eraser has a single region of equal blocks. That one is held
/// inline, so it can be built at run time without a heap: from a controller's
/// block size, say. Maps with several regions borrow a static table or, with
/// `alloc`, own their storage. All three compare and read alike.
#[derive(Debug, Clone)]
pub struct EraseRegions(Repr);

#[derive(Debug, Clone)]
enum Repr {
    Uniform([EraseRegion; 1]),
    Static(&'static [EraseRegion]),
    #[cfg(feature = "alloc")]
    Owned(Vec<EraseRegion>),
}

impl EraseRegions {
    /// One region of `count` blocks of `size` bytes, without allocation.
    pub const fn uniform(size: u32, count: u32) -> Self {
        Self(Repr::Uniform([EraseRegion::new(size, count)]))
    }

    /// Borrow a complete static map, without allocation or capacity limits.
    pub const fn borrowed(regions: &'static [EraseRegion]) -> Self {
        Self(Repr::Static(regions))
    }

    /// Read the regions independently of their storage.
    pub fn as_slice(&self) -> &[EraseRegion] {
        match &self.0 {
            Repr::Uniform(region) => region,
            Repr::Static(regions) => regions,
            #[cfg(feature = "alloc")]
            Repr::Owned(regions) => regions,
        }
    }

    /// Mutate owned runtime data, copying first if the map was inline or borrowed.
    #[cfg(feature = "alloc")]
    pub fn to_mut(&mut self) -> &mut Vec<EraseRegion> {
        if !matches!(self.0, Repr::Owned(_)) {
            self.0 = Repr::Owned(self.as_slice().to_vec());
        }
        match &mut self.0 {
            Repr::Owned(regions) => regions,
            _ => unreachable!("just made owned"),
        }
    }
}

impl Deref for EraseRegions {
    type Target = [EraseRegion];
    fn deref(&self) -> &[EraseRegion] {
        self.as_slice()
    }
}

#[cfg(feature = "alloc")]
impl core::ops::DerefMut for EraseRegions {
    fn deref_mut(&mut self) -> &mut [EraseRegion] {
        self.to_mut()
    }
}

impl PartialEq for EraseRegions {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}
impl Eq for EraseRegions {}

impl Default for EraseRegions {
    fn default() -> Self {
        Self::borrowed(&[])
    }
}

#[cfg(feature = "alloc")]
impl From<Vec<EraseRegion>> for EraseRegions {
    fn from(regions: Vec<EraseRegion>) -> Self {
        match regions[..] {
            [region] => Self(Repr::Uniform([region])),
            _ => Self(Repr::Owned(regions)),
        }
    }
}

#[cfg(feature = "alloc")]
impl FromIterator<EraseRegion> for EraseRegions {
    fn from_iter<I: IntoIterator<Item = EraseRegion>>(regions: I) -> Self {
        regions.into_iter().collect::<Vec<_>>().into()
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for EraseRegions {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_slice().serialize(serializer)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for EraseRegions {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<EraseRegion>::deserialize(deserializer).map(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_does_not_change_what_a_map_is() {
        static TABLE: [EraseRegion; 1] = [EraseRegion::new(4096, 16)];
        let inline = EraseRegions::uniform(4096, 16);
        assert_eq!(inline, EraseRegions::borrowed(&TABLE));
        assert_eq!(inline.as_slice(), &TABLE);
        assert_ne!(inline, EraseRegions::uniform(4096, 8));
        assert!(EraseRegions::default().is_empty());
        // No larger than a borrowed slice, or than a vector with `alloc`.
        assert!(core::mem::size_of::<EraseRegions>() <= 3 * core::mem::size_of::<usize>());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn mutation_takes_ownership_of_inline_and_borrowed_maps() {
        static TABLE: [EraseRegion; 2] = [EraseRegion::new(8, 2), EraseRegion::new(16, 2)];
        for mut regions in [EraseRegions::uniform(8, 2), EraseRegions::borrowed(&TABLE)] {
            regions.to_mut()[0].count = 1;
            assert_eq!(regions[0], EraseRegion::new(8, 1));
        }
        assert_eq!(TABLE[0].count, 2);
        let one: EraseRegions = alloc::vec![EraseRegion::new(8, 2)].into();
        assert!(matches!(one.0, Repr::Uniform(_)));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serialized_as_a_plain_sequence() {
        let maps = [
            alloc::vec![EraseRegion::new(8, 2)],
            alloc::vec![EraseRegion::new(8, 2), EraseRegion::new(16, 2)],
        ];
        for map in maps {
            let encoded = ron::to_string(&EraseRegions::from(map.clone())).unwrap();
            assert_eq!(encoded, ron::to_string(&map).unwrap());
            let decoded: EraseRegions = ron::from_str(&encoded).unwrap();
            assert_eq!(decoded.as_slice(), map);
        }
    }
}

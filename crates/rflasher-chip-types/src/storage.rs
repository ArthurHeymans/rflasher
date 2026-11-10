//! Storage behind chip descriptors. Allocation adds ownership, not a second model.

#[cfg(feature = "alloc")]
use alloc::{borrow::Cow, string::String, vec::Vec};
use core::{fmt, ops::Deref};

/// A chip name borrowed from a static catalog, or owned by a runtime database.
///
/// Cloning a borrowed name never allocates, even with `alloc` enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
pub struct ChipName {
    #[cfg(feature = "alloc")]
    inner: Cow<'static, str>,
    #[cfg(not(feature = "alloc"))]
    inner: &'static str,
}

impl ChipName {
    /// Read the name independently of its storage.
    pub fn as_str(&self) -> &str {
        self
    }

    /// Borrow a name without allocation or runtime initialization.
    pub const fn borrowed(name: &'static str) -> Self {
        Self {
            #[cfg(feature = "alloc")]
            inner: Cow::Borrowed(name),
            #[cfg(not(feature = "alloc"))]
            inner: name,
        }
    }
}

impl Deref for ChipName {
    type Target = str;
    fn deref(&self) -> &str {
        // The borrow is only needed to reach through the owning storage.
        #[cfg(feature = "alloc")]
        return &self.inner;
        #[cfg(not(feature = "alloc"))]
        self.inner
    }
}

impl fmt::Display for ChipName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self)
    }
}

impl PartialEq<&str> for ChipName {
    fn eq(&self, other: &&str) -> bool {
        &**self == *other
    }
}

#[cfg(feature = "alloc")]
impl From<String> for ChipName {
    fn from(name: String) -> Self {
        Self {
            inner: Cow::Owned(name),
        }
    }
}

#[cfg(feature = "alloc")]
impl From<&str> for ChipName {
    fn from(name: &str) -> Self {
        String::from(name).into()
    }
}

#[cfg(not(feature = "alloc"))]
impl From<&'static str> for ChipName {
    fn from(name: &'static str) -> Self {
        Self::borrowed(name)
    }
}

/// An immutable static descriptor slice, optionally owning runtime data.
///
/// Uses `Cow` when allocation is available. The public borrowed constructor
/// and read-only slice API are identical with and without `alloc`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(transparent)
)]
pub struct ChipSlice<T: Clone + 'static> {
    #[cfg(feature = "alloc")]
    inner: Cow<'static, [T]>,
    #[cfg(not(feature = "alloc"))]
    inner: &'static [T],
}

impl<T: Clone + 'static> ChipSlice<T> {
    /// Read the descriptor slice independently of its storage.
    pub fn as_slice(&self) -> &[T] {
        self
    }

    /// Borrow a complete static map, without allocation or capacity limits.
    pub const fn borrowed(values: &'static [T]) -> Self {
        Self {
            #[cfg(feature = "alloc")]
            inner: Cow::Borrowed(values),
            #[cfg(not(feature = "alloc"))]
            inner: values,
        }
    }

    /// Mutate owned runtime data, copying first if this was a borrowed catalog.
    #[cfg(feature = "alloc")]
    pub fn to_mut(&mut self) -> &mut Vec<T> {
        self.inner.to_mut()
    }
}

impl<T: Clone + 'static> Deref for ChipSlice<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        // The borrow is only needed to reach through the owning storage.
        #[cfg(feature = "alloc")]
        return &self.inner;
        #[cfg(not(feature = "alloc"))]
        self.inner
    }
}

impl<T: Clone + 'static> Default for ChipSlice<T> {
    fn default() -> Self {
        Self::borrowed(&[])
    }
}

#[cfg(feature = "alloc")]
impl<T: Clone + 'static> From<Vec<T>> for ChipSlice<T> {
    fn from(values: Vec<T>) -> Self {
        Self {
            inner: Cow::Owned(values),
        }
    }
}

#[cfg(feature = "alloc")]
impl<T: Clone + 'static> FromIterator<T> for ChipSlice<T> {
    fn from_iter<I: IntoIterator<Item = T>>(values: I) -> Self {
        values.into_iter().collect::<Vec<_>>().into()
    }
}

#[cfg(feature = "alloc")]
impl<T: Clone + 'static> core::ops::DerefMut for ChipSlice<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        self.to_mut()
    }
}

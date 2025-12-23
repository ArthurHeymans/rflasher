//! Layout types
//!
//! Core types for flash memory layouts that work in no_std environments.

#[cfg(feature = "alloc")]
use alloc::string::String;
#[cfg(feature = "alloc")]
use alloc::vec::Vec;

/// A named region within a flash chip
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "std", derive(serde::Serialize, serde::Deserialize))]
pub struct Region {
    /// Name of the region
    #[cfg(feature = "alloc")]
    pub name: String,
    /// Start address (inclusive)
    pub start: u32,
    /// End address (inclusive)
    pub end: u32,
    /// Whether this region is read-only (prevents writes)
    #[cfg_attr(feature = "std", serde(default))]
    pub readonly: bool,
    /// Whether this region is dangerous to modify (shows warning)
    #[cfg_attr(feature = "std", serde(default))]
    pub dangerous: bool,
    /// Whether this region is included in operations
    #[cfg_attr(feature = "std", serde(skip))]
    pub included: bool,
}

impl Region {
    /// Create a new region
    #[cfg(feature = "alloc")]
    pub fn new(name: impl Into<String>, start: u32, end: u32) -> Self {
        Self {
            name: name.into(),
            start,
            end,
            readonly: false,
            dangerous: false,
            included: false,
        }
    }

    /// Get the size of this region in bytes
    pub fn size(&self) -> u32 {
        self.end - self.start + 1
    }

    /// Check if an address is within this region
    pub fn contains(&self, addr: u32) -> bool {
        addr >= self.start && addr <= self.end
    }

    /// Check if this region overlaps with another
    pub fn overlaps(&self, other: &Region) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    /// Check if this region is aligned to the given boundary
    pub fn is_aligned(&self, alignment: u32) -> bool {
        self.start.is_multiple_of(alignment) && (self.end + 1).is_multiple_of(alignment)
    }
}

/// Source of the layout information
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutSource {
    /// Layout loaded from a TOML file
    Toml,
    /// Layout parsed from Intel Flash Descriptor
    Ifd,
    /// Layout parsed from FMAP structure
    Fmap,
    /// Layout created manually
    Manual,
}

/// A flash memory layout containing named regions
#[derive(Debug, Clone)]
#[cfg(feature = "alloc")]
pub struct Layout {
    /// Optional name for this layout
    pub name: Option<String>,
    /// Expected chip size (for validation)
    pub chip_size: Option<u32>,
    /// Source of this layout
    pub source: LayoutSource,
    /// Regions in this layout
    pub regions: Vec<Region>,
}

#[cfg(feature = "alloc")]
impl Layout {
    /// Create a new empty layout
    pub fn new() -> Self {
        Self {
            name: None,
            chip_size: None,
            source: LayoutSource::Manual,
            regions: Vec::new(),
        }
    }

    /// Create a layout with a specific source
    pub fn with_source(source: LayoutSource) -> Self {
        Self {
            name: None,
            chip_size: None,
            source,
            regions: Vec::new(),
        }
    }

    /// Add a region to the layout
    pub fn add_region(&mut self, region: Region) {
        self.regions.push(region);
    }

    /// Find a region by name (case-insensitive)
    pub fn find_region(&self, name: &str) -> Option<&Region> {
        self.regions
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(name))
    }

    /// Find a region by name (case-insensitive), mutable
    pub fn find_region_mut(&mut self, name: &str) -> Option<&mut Region> {
        self.regions
            .iter_mut()
            .find(|r| r.name.eq_ignore_ascii_case(name))
    }

    /// Mark a region as included
    pub fn include_region(&mut self, name: &str) -> Result<(), LayoutError> {
        match self.find_region_mut(name) {
            Some(region) => {
                region.included = true;
                Ok(())
            }
            None => Err(LayoutError::RegionNotFound),
        }
    }

    /// Mark a region as excluded
    pub fn exclude_region(&mut self, name: &str) -> Result<(), LayoutError> {
        match self.find_region_mut(name) {
            Some(region) => {
                region.included = false;
                Ok(())
            }
            None => Err(LayoutError::RegionNotFound),
        }
    }

    /// Include all regions
    pub fn include_all(&mut self) {
        for region in &mut self.regions {
            region.included = true;
        }
    }

    /// Exclude all regions
    pub fn exclude_all(&mut self) {
        for region in &mut self.regions {
            region.included = false;
        }
    }

    /// Get all included regions
    pub fn included_regions(&self) -> impl Iterator<Item = &Region> {
        self.regions.iter().filter(|r| r.included)
    }

    /// Check if any regions are included
    pub fn has_included_regions(&self) -> bool {
        self.regions.iter().any(|r| r.included)
    }

    /// Get the number of regions
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    /// Check if the layout is empty
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Sort regions by start address
    pub fn sort_by_address(&mut self) {
        self.regions.sort_by_key(|r| r.start);
    }

    /// Validate the layout against a chip size
    pub fn validate(&self, chip_size: u32) -> Result<(), LayoutError> {
        // Check chip size matches if specified
        if let Some(expected) = self.chip_size {
            if expected != chip_size {
                return Err(LayoutError::ChipSizeMismatch {
                    expected,
                    actual: chip_size,
                });
            }
        }

        // Check all regions are within chip bounds
        for region in &self.regions {
            if region.end >= chip_size {
                return Err(LayoutError::RegionOutOfBounds);
            }
            if region.start > region.end {
                return Err(LayoutError::InvalidRegion);
            }
        }

        // Check for overlapping regions and duplicate names
        for (i, r1) in self.regions.iter().enumerate() {
            for r2 in self.regions.iter().skip(i + 1) {
                if r1.overlaps(r2) {
                    return Err(LayoutError::OverlappingRegions);
                }
                if r1.name.eq_ignore_ascii_case(&r2.name) {
                    return Err(LayoutError::DuplicateRegionName);
                }
            }
        }

        Ok(())
    }

    /// Get dangerous regions that are included
    pub fn dangerous_included(&self) -> Vec<&Region> {
        self.regions
            .iter()
            .filter(|r| r.included && r.dangerous)
            .collect()
    }

    /// Get readonly regions that are included
    pub fn readonly_included(&self) -> Vec<&Region> {
        self.regions
            .iter()
            .filter(|r| r.included && r.readonly)
            .collect()
    }

    /// Update a region's end address
    pub fn update_region_end(&mut self, name: &str, new_end: u32) -> Result<(), LayoutError> {
        match self.find_region_mut(name) {
            Some(region) => {
                if new_end < region.start {
                    return Err(LayoutError::InvalidRegion);
                }
                region.end = new_end;
                Ok(())
            }
            None => Err(LayoutError::RegionNotFound),
        }
    }
}

#[cfg(feature = "alloc")]
impl Default for Layout {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors that can occur when working with layouts
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    /// Region not found by name
    RegionNotFound,
    /// Region extends beyond chip size
    RegionOutOfBounds,
    /// Region has invalid bounds (start > end)
    InvalidRegion,
    /// Two regions overlap
    OverlappingRegions,
    /// Two regions have the same name
    DuplicateRegionName,
    /// Chip size doesn't match expected
    ChipSizeMismatch {
        /// Expected chip size
        expected: u32,
        /// Actual chip size
        actual: u32,
    },
    /// Failed to parse layout file
    ParseError,
    /// Invalid IFD signature
    InvalidIfdSignature,
    /// Invalid FMAP signature
    InvalidFmapSignature,
    /// FMAP version not supported
    UnsupportedFmapVersion,
    /// I/O error
    IoError,
}

#[cfg(feature = "std")]
impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::RegionNotFound => write!(f, "region not found"),
            Self::DuplicateRegionName => write!(f, "duplicate region name"),
            Self::RegionOutOfBounds => write!(f, "region extends beyond chip size"),
            Self::InvalidRegion => write!(f, "invalid region bounds"),
            Self::OverlappingRegions => write!(f, "overlapping regions"),
            Self::ChipSizeMismatch { expected, actual } => {
                write!(
                    f,
                    "chip size mismatch: expected {} bytes, got {} bytes",
                    expected, actual
                )
            }
            Self::ParseError => write!(f, "failed to parse layout"),
            Self::InvalidIfdSignature => write!(f, "invalid Intel Flash Descriptor signature"),
            Self::InvalidFmapSignature => write!(f, "invalid FMAP signature"),
            Self::UnsupportedFmapVersion => write!(f, "unsupported FMAP version"),
            Self::IoError => write!(f, "I/O error"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for LayoutError {}

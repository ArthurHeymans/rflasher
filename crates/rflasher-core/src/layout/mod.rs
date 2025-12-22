//! Flash layout support
//!
//! This module provides support for flash memory layouts, which define
//! named regions within a flash chip. Layouts can be:
//!
//! - Loaded from TOML files
//! - Parsed from Intel Flash Descriptors (IFD)
//! - Parsed from FMAP structures (Chromebook-style)
//!
//! # Region Operations
//!
//! Layouts support include/exclude filtering to operate on specific regions:
//!
//! ```ignore
//! let mut layout = Layout::from_toml_file("layout.toml")?;
//! layout.include_region("bios")?;
//! layout.exclude_region("me")?;
//! ```

mod types;

#[cfg(feature = "std")]
mod fmap;
#[cfg(feature = "std")]
mod ifd;
#[cfg(feature = "std")]
mod toml;

pub use types::*;

#[cfg(feature = "std")]
pub use fmap::{fmap_offset, has_fmap, parse_fmap, parse_fmap_at};
#[cfg(feature = "std")]
pub use ifd::{has_ifd, parse_ifd};

//! Flash layout support
//!
//! This module provides support for flash memory layouts, which define
//! named regions within a flash chip. Layouts can be:
//!
//! - Loaded from TOML files
//! - Parsed from Intel Flash Descriptors (IFD)
//! - Parsed from FMAP structures (Chromebook-style)
//! - Read directly from the flash chip
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
//!
//! # Reading from Flash
//!
//! Layouts can be read directly from a flash chip:
//!
//! ```ignore
//! let ctx = flash::probe(master, db)?;
//! // Auto-detect IFD or FMAP
//! let layout = read_layout_from_flash(master, &ctx)?;
//! // Or read specifically:
//! let ifd_layout = read_ifd_from_flash(master, &ctx)?;
//! let fmap_layout = read_fmap_from_flash(master, &ctx)?;
//! ```

mod types;

#[cfg(feature = "std")]
mod flash;
#[cfg(feature = "std")]
mod fmap;
#[cfg(feature = "std")]
mod ifd;
#[cfg(feature = "std")]
mod toml;

pub use types::*;

#[cfg(feature = "std")]
pub use flash::{read_fmap_from_flash, read_ifd_from_flash, read_layout_from_flash};
#[cfg(feature = "std")]
pub use fmap::{fmap_offset, has_fmap, parse_fmap, parse_fmap_at};
#[cfg(feature = "std")]
pub use ifd::{has_ifd, parse_ifd};

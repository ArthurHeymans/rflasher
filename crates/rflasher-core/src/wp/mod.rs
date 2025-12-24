//! Write protection support
//!
//! This module provides types and functions for working with flash chip
//! write protection.
//!
//! # Overview
//!
//! SPI flash chips implement write protection through status register bits:
//!
//! - **BP bits (Block Protect)**: Select how much of the chip is protected
//! - **TB bit (Top/Bottom)**: Select whether protection is from top or bottom
//! - **SEC bit (Sector/Block)**: Select 4K sector or 64K block granularity
//! - **CMP bit (Complement)**: Invert the protected region
//! - **SRP/SRL bits**: Control the protection mode (hardware, power-cycle, permanent)
//!
//! # Example
//!
//! ```ignore
//! use rflasher_core::wp::{
//!     read_wp_config, write_wp_config, WpConfig, WpMode, WpRange,
//!     WpRegBitMap, RangeDecoder, WriteOptions,
//! };
//!
//! // Use standard Winbond register layout
//! let bit_map = WpRegBitMap::winbond_standard();
//!
//! // Read current protection status
//! let config = read_wp_config(&mut spi, &bit_map, chip_size, RangeDecoder::Spi25)?;
//! println!("Mode: {}, Range: {}", config.mode, config.range);
//!
//! // Disable write protection
//! let new_config = WpConfig::new(WpMode::Disabled, WpRange::none());
//! write_wp_config(&mut spi, &new_config, &bit_map, chip_size, RangeDecoder::Spi25, WriteOptions::default())?;
//! ```

mod ops;
mod ranges;
mod types;

pub use ops::*;
pub use ranges::*;
pub use types::*;

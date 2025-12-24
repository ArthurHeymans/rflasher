//! SFDP (Serial Flash Discoverable Parameters) parsing
//!
//! This module implements parsing of SFDP data structures as defined by
//! JEDEC JESD216 (through revision H). SFDP provides a standardized way
//! for flash chips to describe their capabilities.
//!
//! # Overview
//!
//! SFDP data is stored in a reserved area of the flash chip and can be
//! read using the RDSFDP command (0x5A). The structure contains:
//!
//! - An SFDP header with signature and revision info
//! - One or more parameter headers describing available tables
//! - Parameter tables containing capability information
//!
//! # Usage
//!
//! ```ignore
//! use rflasher_core::sfdp;
//! use rflasher_core::programmer::SpiMaster;
//!
//! fn probe_sfdp<M: SpiMaster>(master: &mut M) {
//!     match sfdp::probe(master) {
//!         Ok(info) => {
//!             println!("Flash size: {} bytes", info.total_size);
//!             println!("Page size: {} bytes", info.page_size);
//!         }
//!         Err(_) => println!("SFDP not supported"),
//!     }
//! }
//! ```

mod parser;
mod types;

pub use parser::*;
pub use types::*;

//! High-level flash operations
//!
//! The API is async on every target.

mod diff;
mod erase_plan;
#[cfg(feature = "std")]
mod probe;
mod progress;
mod spi_ops;
#[cfg(all(test, feature = "std"))]
mod tests;

#[cfg(feature = "alloc")]
pub use diff::{WriteRange, WriteStats, coalesce_write_ranges, get_all_write_ranges, need_write};
pub use erase_plan::OptimalEraseOp;
#[cfg(feature = "alloc")]
pub use erase_plan::{plan_optimal_erase, plan_optimal_erase_region};
#[cfg(feature = "std")]
pub(crate) use probe::select_probed_chip;
#[cfg(feature = "std")]
pub use probe::{ProbeOptions, ProbeResult, probe_detailed, probe_with_options};
#[cfg(feature = "alloc")]
pub use progress::{NoProgress, WriteProgress};
pub use spi_ops::{check_erased_range, read, select_erase_block, spi_program, spi_read, write};
pub(crate) use spi_ops::{
    check_spi_writable, erase_spi_operation, erase_spi_range, spi_eraser_usable,
    validate_spi_erase_operation, validate_spi_program,
};

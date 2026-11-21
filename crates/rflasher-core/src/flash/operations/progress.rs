//! Progress reporting trait for high-level flash operations.

#[cfg(feature = "alloc")]
use super::diff::WriteStats;

// =============================================================================
// Smart write operations - minimize erase/write based on content comparison
// =============================================================================

/// Callback for progress reporting during smart write operations
#[cfg(feature = "alloc")]
pub trait WriteProgress {
    /// Called when starting to read current flash contents
    fn reading(&mut self, total_bytes: usize);

    /// Called to update read progress
    fn read_progress(&mut self, bytes_read: usize);

    /// Called when starting erase operations
    fn erasing(&mut self, blocks_to_erase: usize, bytes_to_erase: usize);

    /// Called after each block is erased
    fn erase_progress(&mut self, blocks_erased: usize, bytes_erased: usize);

    /// Called when starting write operations
    fn writing(&mut self, bytes_to_write: usize);

    /// Called to update write progress
    fn write_progress(&mut self, bytes_written: usize);

    /// Called when the operation is complete
    fn complete(&mut self, stats: &WriteStats);
}

/// A no-op progress reporter
#[cfg(feature = "alloc")]
pub struct NoProgress;

#[cfg(feature = "alloc")]
impl WriteProgress for NoProgress {
    fn reading(&mut self, _total_bytes: usize) {}
    fn read_progress(&mut self, _bytes_read: usize) {}
    fn erasing(&mut self, _blocks_to_erase: usize, _bytes_to_erase: usize) {}
    fn erase_progress(&mut self, _blocks_erased: usize, _bytes_erased: usize) {}
    fn writing(&mut self, _bytes_to_write: usize) {}
    fn write_progress(&mut self, _bytes_written: usize) {}
    fn complete(&mut self, _stats: &WriteStats) {}
}

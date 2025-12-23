//! Opaque flash device adapter
//!
//! This module provides `OpaqueFlashDevice`, an adapter that implements
//! `FlashDevice` for opaque programmers like Intel internal flash controller.

use crate::chip::{EraseBlock, WriteGranularity};
use crate::error::{Error, Result};
use crate::flash::device::FlashDevice;
use crate::programmer::OpaqueMaster;
use crate::spi::opcodes;

/// Default erase block size for opaque programmers (4KB)
///
/// Most opaque programmers (like Intel PCH hardware sequencing) use 4KB
/// as the minimum erase block size.
const DEFAULT_ERASE_BLOCK_SIZE: u32 = 4096;

/// Flash device adapter for opaque programmers
///
/// This wraps an `OpaqueMaster` implementation to provide the unified
/// `FlashDevice` interface. Opaque programmers don't expose raw SPI access,
/// so we don't have chip metadata from JEDEC probing. Instead, we use
/// fixed defaults for erase granularity and write granularity.
///
/// # Example
///
/// ```ignore
/// use rflasher_core::flash::OpaqueFlashDevice;
/// use rflasher_core::programmer::OpaqueMaster;
///
/// fn read_flash<M: OpaqueMaster>(master: &mut M) {
///     let mut device = OpaqueFlashDevice::new(master);
///
///     let mut buf = [0u8; 4096];
///     device.read(0, &mut buf).unwrap();
/// }
/// ```
pub struct OpaqueFlashDevice<'a, M: OpaqueMaster + ?Sized> {
    master: &'a mut M,
    /// Cached flash size
    size: u32,
    /// Erase block size (defaults to 4KB)
    erase_block_size: u32,
    /// Static erase block array for the FlashDevice trait
    erase_blocks: [EraseBlock; 1],
}

impl<'a, M: OpaqueMaster + ?Sized> OpaqueFlashDevice<'a, M> {
    /// Create a new opaque flash device adapter
    ///
    /// # Arguments
    /// * `master` - The opaque master to use for communication
    pub fn new(master: &'a mut M) -> Self {
        let size = master.size() as u32;
        Self {
            master,
            size,
            erase_block_size: DEFAULT_ERASE_BLOCK_SIZE,
            erase_blocks: [EraseBlock::new(opcodes::SE_20, DEFAULT_ERASE_BLOCK_SIZE)],
        }
    }

    /// Create a new opaque flash device adapter with a specified size
    ///
    /// Use this when the master doesn't know the flash size (returns 0)
    /// but you've determined it through other means (e.g., reading IFD).
    ///
    /// # Arguments
    /// * `master` - The opaque master to use for communication
    /// * `size` - Flash size in bytes
    pub fn with_size(master: &'a mut M, size: u32) -> Self {
        Self {
            master,
            size,
            erase_block_size: DEFAULT_ERASE_BLOCK_SIZE,
            erase_blocks: [EraseBlock::new(opcodes::SE_20, DEFAULT_ERASE_BLOCK_SIZE)],
        }
    }

    /// Set a custom erase block size
    ///
    /// # Arguments
    /// * `size` - Erase block size in bytes
    pub fn set_erase_block_size(&mut self, size: u32) {
        self.erase_block_size = size;
        self.erase_blocks = [EraseBlock::new(opcodes::SE_20, size)];
    }

    /// Get a reference to the underlying opaque master
    pub fn master(&mut self) -> &mut M {
        self.master
    }
}

impl<M: OpaqueMaster + ?Sized> FlashDevice for OpaqueFlashDevice<'_, M> {
    fn size(&self) -> u32 {
        self.size
    }

    fn erase_granularity(&self) -> u32 {
        self.erase_block_size
    }

    fn write_granularity(&self) -> WriteGranularity {
        // Opaque programmers typically have bit-level write granularity
        // (you can change any bit from 1 to 0 without erasing first)
        WriteGranularity::Bit
    }

    fn erase_blocks(&self) -> &[EraseBlock] {
        &self.erase_blocks
    }

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        if !self.is_valid_range(addr, buf.len()) {
            return Err(Error::AddressOutOfBounds);
        }
        self.master.read(addr, buf)
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        if !self.is_valid_range(addr, data.len()) {
            return Err(Error::AddressOutOfBounds);
        }
        self.master.write(addr, data)
    }

    fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
        if !self.is_valid_range(addr, len as usize) {
            return Err(Error::AddressOutOfBounds);
        }
        // Note: We don't check alignment here because the opaque master
        // may handle unaligned erases internally (or return an error)
        self.master.erase(addr, len)
    }
}

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
/// The device can either borrow (`&mut M`) or own (`M`) the master,
/// depending on how it's constructed.
///
/// # Example (borrowing)
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
///
/// # Example (owning)
///
/// ```ignore
/// use rflasher_core::flash::OpaqueFlashDevice;
/// use rflasher_internal::InternalProgrammer;
///
/// fn create_flash_handle() -> OpaqueFlashDevice<InternalProgrammer> {
///     let master = InternalProgrammer::new().unwrap();
///     let size = 16 * 1024 * 1024; // 16 MiB
///     OpaqueFlashDevice::new_owned(master, size)
/// }
/// ```
pub enum OpaqueFlashDevice<M: OpaqueMaster + 'static> {
    /// Borrowed master (for backwards compatibility)
    Borrowed {
        /// Pointer to borrowed opaque master
        master: *mut dyn OpaqueMaster,
        /// Flash size in bytes
        size: u32,
        /// Erase block size in bytes
        erase_block_size: u32,
        /// Erase block descriptors
        erase_blocks: [EraseBlock; 1],
        /// Phantom data for lifetime safety
        _marker: core::marker::PhantomData<&'static mut M>,
    },
    /// Owned master (for new code)
    Owned {
        /// Owned opaque master
        master: M,
        /// Flash size in bytes
        size: u32,
        /// Erase block size in bytes
        erase_block_size: u32,
        /// Erase block descriptors
        erase_blocks: [EraseBlock; 1],
    },
}

impl<M: OpaqueMaster> OpaqueFlashDevice<M> {
    /// Create a new opaque flash device adapter (borrowing the master)
    ///
    /// # Arguments
    /// * `master` - The opaque master to use for communication
    pub fn new(master: &mut M) -> OpaqueFlashDevice<M> {
        let size = master.size() as u32;
        OpaqueFlashDevice::Borrowed {
            master: master as *mut M as *mut dyn OpaqueMaster,
            size,
            erase_block_size: DEFAULT_ERASE_BLOCK_SIZE,
            erase_blocks: [EraseBlock::new(opcodes::SE_20, DEFAULT_ERASE_BLOCK_SIZE)],
            _marker: core::marker::PhantomData,
        }
    }

    /// Create a new opaque flash device adapter with a specified size (borrowing)
    ///
    /// Use this when the master doesn't know the flash size (returns 0)
    /// but you've determined it through other means (e.g., reading IFD).
    ///
    /// # Arguments
    /// * `master` - The opaque master to use for communication
    /// * `size` - Flash size in bytes
    pub fn with_size(master: &mut M, size: u32) -> OpaqueFlashDevice<M> {
        OpaqueFlashDevice::Borrowed {
            master: master as *mut M as *mut dyn OpaqueMaster,
            size,
            erase_block_size: DEFAULT_ERASE_BLOCK_SIZE,
            erase_blocks: [EraseBlock::new(opcodes::SE_20, DEFAULT_ERASE_BLOCK_SIZE)],
            _marker: core::marker::PhantomData,
        }
    }

    /// Create a new opaque flash device adapter (owning the master)
    ///
    /// # Arguments
    /// * `master` - The opaque master to take ownership of
    /// * `size` - Flash size in bytes
    pub fn new_owned(master: M, size: u32) -> Self {
        OpaqueFlashDevice::Owned {
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
        let erase_block = EraseBlock::new(opcodes::SE_20, size);
        match self {
            OpaqueFlashDevice::Borrowed {
                erase_block_size,
                erase_blocks,
                ..
            } => {
                *erase_block_size = size;
                *erase_blocks = [erase_block];
            }
            OpaqueFlashDevice::Owned {
                erase_block_size,
                erase_blocks,
                ..
            } => {
                *erase_block_size = size;
                *erase_blocks = [erase_block];
            }
        }
    }

    /// Get a mutable reference to the underlying opaque master
    pub fn master(&mut self) -> &mut dyn OpaqueMaster {
        match self {
            OpaqueFlashDevice::Borrowed { master, .. } => unsafe { &mut **master },
            OpaqueFlashDevice::Owned { master, .. } => master,
        }
    }
}

impl<M: OpaqueMaster> FlashDevice for OpaqueFlashDevice<M> {
    fn size(&self) -> u32 {
        match self {
            OpaqueFlashDevice::Borrowed { size, .. } => *size,
            OpaqueFlashDevice::Owned { size, .. } => *size,
        }
    }

    fn erase_granularity(&self) -> u32 {
        match self {
            OpaqueFlashDevice::Borrowed {
                erase_block_size, ..
            } => *erase_block_size,
            OpaqueFlashDevice::Owned {
                erase_block_size, ..
            } => *erase_block_size,
        }
    }

    fn write_granularity(&self) -> WriteGranularity {
        // Opaque programmers typically have bit-level write granularity
        // (you can change any bit from 1 to 0 without erasing first)
        WriteGranularity::Bit
    }

    fn erase_blocks(&self) -> &[EraseBlock] {
        match self {
            OpaqueFlashDevice::Borrowed { erase_blocks, .. } => erase_blocks,
            OpaqueFlashDevice::Owned { erase_blocks, .. } => erase_blocks,
        }
    }

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        if !self.is_valid_range(addr, buf.len()) {
            return Err(Error::AddressOutOfBounds);
        }
        self.master().read(addr, buf)
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        if !self.is_valid_range(addr, data.len()) {
            return Err(Error::AddressOutOfBounds);
        }
        self.master().write(addr, data)
    }

    fn erase(&mut self, addr: u32, len: u32) -> Result<()> {
        if !self.is_valid_range(addr, len as usize) {
            return Err(Error::AddressOutOfBounds);
        }
        // Note: We don't check alignment here because the opaque master
        // may handle unaligned erases internally (or return an error)
        self.master().erase(addr, len)
    }
}

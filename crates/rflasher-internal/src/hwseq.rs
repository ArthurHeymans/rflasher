//! Address-based flash access through Intel hardware sequencing.

use crate::controller::Controller;
use crate::error::InternalError;
use rflasher_core::chip::{EraseBlock, WriteGranularity};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::flash::FlashDevice;
use rflasher_core::programmer::FlashAccess;
use rflasher_core::spi::opcodes;

/// Flash behind Intel hardware sequencing.
///
/// The controller runs the flash protocol itself, so no chip has to be
/// identified: the size comes from the flash descriptor or the caller, and
/// the erase block size from the controller, which always erases whole
/// hardware blocks. Needs neither `std` nor a heap.
pub struct HwseqFlash<C: Controller> {
    controller: C,
    size: u32,
    erase_blocks: [EraseBlock; 1],
}

#[cfg(feature = "alloc")]
fn descriptor_size(header: &[u8], physical_size: Option<u32>) -> Result<u32, InternalError> {
    let region_end = rflasher_core::layout::parse_ifd(header)
        .ok()
        .and_then(|layout| layout.regions.iter().map(|r| r.end).max())
        .and_then(|end| end.checked_add(1))
        .filter(|&size| size > 0)
        .ok_or(InternalError::InvalidDescriptor)?;
    match physical_size {
        Some(size) if size >= region_end => Ok(size),
        Some(_) => Err(InternalError::InvalidDescriptor),
        None => {
            log::warn!(
                "Physical flash density is unknown; using the last descriptor region endpoint"
            );
            Ok(region_end)
        }
    }
}

impl<C: Controller> HwseqFlash<C> {
    /// Size the flash from its descriptor: the component density, or the end
    /// of the last region where the density is unknown.
    #[cfg(feature = "alloc")]
    pub fn new(mut controller: C) -> Result<Self, InternalError> {
        let mut header = [0; 4096];
        controller
            .controller_read(0, &mut header, 0)
            .map_err(|_| InternalError::Io("Cannot read the flash descriptor"))?;
        let size = descriptor_size(&header, controller.physical_flash_size()?)?;
        log::info!("Flash size: {} bytes", size);
        Self::with_size(controller, size)
    }

    /// Use a flash size the caller knows, such as a board's.
    ///
    /// Refuses a controller that does not erase by address itself (anything
    /// but Intel hardware sequencing), since nothing here knows the chip, and
    /// a size its erase blocks do not divide.
    pub fn with_size(controller: C, size: u32) -> Result<Self, InternalError> {
        let block = controller
            .erase_block_size(size)?
            .ok_or(InternalError::NotSupported(
                "Controller does not erase by address; identify the chip instead",
            ))?;
        if size == 0 || block == 0 || !size.is_multiple_of(block) {
            return Err(InternalError::NotSupported(
                "Flash size is not a multiple of the hardware erase block",
            ));
        }
        log::info!("Hardware sequencing erase block size: {} bytes", block);
        Ok(Self {
            controller,
            size,
            // The opcode is never sent; the controller picks its own.
            erase_blocks: [EraseBlock::with_count(opcodes::SE_20, block, size / block)],
        })
    }

    /// The controller, e.g. to manage its BIOS write enable around operations.
    pub fn controller(&mut self) -> &mut C {
        &mut self.controller
    }

    /// Give the controller back.
    pub fn into_controller(self) -> C {
        self.controller
    }

    fn check_writable(&self, addr: u32, len: usize) -> CoreResult<()> {
        if !self.is_valid_range(addr, len) {
            Err(CoreError::AddressOutOfBounds)
        } else {
            Ok(())
        }
    }
}

#[cfg(all(test, feature = "alloc"))]
#[test]
fn descriptor_size_preserves_unallocated_tail() {
    let mut header = [0u8; 4096];
    header[0..4].copy_from_slice(&0x0ff0_a55au32.to_le_bytes());
    header[4..8].copy_from_slice(&((2u32 << 24) | (4 << 16)).to_le_bytes());
    header[0x40..0x44].copy_from_slice(&0u32.to_le_bytes());
    header[0x44..0x48].copy_from_slice(&((0x7ffu32 << 16) | 1).to_le_bytes());
    header[0x48..0x4c].copy_from_slice(&0x7fffu32.to_le_bytes());
    assert_eq!(descriptor_size(&header, Some(16 << 20)).unwrap(), 16 << 20);
    assert_eq!(descriptor_size(&header, None).unwrap(), 8 << 20);
    assert!(descriptor_size(&header, Some(4 << 20)).is_err());
    header[0] = 0;
    assert!(descriptor_size(&header, Some(16 << 20)).is_err());
}

impl<C: Controller> FlashDevice for HwseqFlash<C> {
    fn size(&self) -> u32 {
        self.size
    }

    fn erase_granularity(&self) -> u32 {
        self.erase_blocks[0].min_block_size()
    }

    fn write_granularity(&self) -> WriteGranularity {
        WriteGranularity::Bit
    }

    fn erase_blocks(&self) -> &[EraseBlock] {
        &self.erase_blocks
    }

    fn check_access(&self, access: FlashAccess, addr: u32, len: u32) -> CoreResult<()> {
        self.controller.check_access(access, addr, len)
    }

    async fn read(&mut self, addr: u32, buf: &mut [u8]) -> CoreResult<()> {
        if !self.is_valid_range(addr, buf.len()) {
            return Err(CoreError::AddressOutOfBounds);
        }
        self.controller
            .controller_read(addr, buf, self.size as usize)
    }

    fn validate_write(&self, addr: u32, len: usize) -> CoreResult<()> {
        self.check_writable(addr, len)
    }

    async fn write(&mut self, addr: u32, data: &[u8]) -> CoreResult<()> {
        self.check_writable(addr, data.len())?;
        self.controller.controller_write(addr, data)
    }

    async fn erase(&mut self, addr: u32, len: u32) -> CoreResult<()> {
        self.check_writable(addr, len as usize)?;
        self.controller.controller_erase(addr, len)
    }
}

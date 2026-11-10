//! Flash context - runtime state for flash operations.

use crate::chip::FlashChip;

/// Address mode currently in use
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddressMode {
    /// 3-byte addressing (up to 16 MiB)
    #[default]
    ThreeByte,
    /// 4-byte addressing (up to 4 GiB)
    FourByte,
}

/// Runtime state for either a borrowed catalog descriptor or an owned chip.
///
/// The representation and API do not change with `alloc`. Cloning a static
/// descriptor into a context borrows its names and erase maps without a heap.
#[derive(Debug, Clone)]
pub struct FlashContext {
    /// Identified flash chip and its operation metadata.
    pub chip: FlashChip,
    /// Current address mode.
    pub address_mode: AddressMode,
}

impl FlashContext {
    /// Create a context from an identified descriptor.
    pub fn new(chip: FlashChip) -> Self {
        let address_mode = if chip.requires_4byte_addr() {
            AddressMode::FourByte
        } else {
            AddressMode::ThreeByte
        };
        Self { chip, address_mode }
    }

    /// Get the page size for this chip.
    pub fn page_size(&self) -> usize {
        self.chip.page_size as usize
    }

    /// Get the total size of this chip.
    pub fn total_size(&self) -> usize {
        self.chip.total_size as usize
    }

    /// Check if an address is valid for this chip.
    pub fn is_valid_address(&self, addr: u32) -> bool {
        addr < self.chip.total_size
    }

    /// Check if an entire range is valid for this chip.
    pub fn is_valid_range(&self, addr: u32, len: usize) -> bool {
        addr < self.chip.total_size && len as u64 <= u64::from(self.chip.total_size - addr)
    }
}

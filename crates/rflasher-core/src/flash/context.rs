//! Flash context - runtime state for flash operations

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

/// Runtime context for flash operations
///
/// This structure holds the state needed to interact with a specific
/// flash chip through a programmer.
#[derive(Debug)]
pub struct FlashContext {
    /// The identified flash chip
    pub chip: &'static FlashChip,
    /// Current address mode
    pub address_mode: AddressMode,
    /// Whether to use native 4-byte commands or mode switching
    pub use_native_4byte: bool,
}

impl FlashContext {
    /// Create a new flash context for the given chip
    pub fn new(chip: &'static FlashChip) -> Self {
        let address_mode = if chip.requires_4byte_addr() {
            AddressMode::FourByte
        } else {
            AddressMode::ThreeByte
        };

        let use_native_4byte = chip
            .features
            .contains(crate::chip::Features::FOUR_BYTE_NATIVE);

        Self {
            chip,
            address_mode,
            use_native_4byte,
        }
    }

    /// Get the page size for this chip
    pub fn page_size(&self) -> usize {
        self.chip.page_size as usize
    }

    /// Get the total size of this chip
    pub fn total_size(&self) -> usize {
        self.chip.total_size as usize
    }

    /// Check if an address is valid for this chip
    pub fn is_valid_address(&self, addr: u32) -> bool {
        addr < self.chip.total_size
    }

    /// Check if an address range is valid for this chip
    pub fn is_valid_range(&self, addr: u32, len: usize) -> bool {
        if addr >= self.chip.total_size {
            return false;
        }
        let end = addr as u64 + len as u64;
        end <= self.chip.total_size as u64
    }
}

//! FlashHandle - unified abstraction over Flash + Programmer
//!
//! This is similar to flashprog's `struct flashctx` which combines
//! chip information and programmer access into a single handle.

use rflasher_core::chip::FlashChip;
use rflasher_core::flash::{FlashContext, FlashDevice};

/// Chip information available from a FlashHandle
#[derive(Debug, Clone)]
pub struct ChipInfo {
    /// Vendor name (e.g., "Winbond")
    pub vendor: String,
    /// Chip name (e.g., "W25Q128.V")
    pub name: String,
    /// JEDEC manufacturer ID
    pub jedec_manufacturer: u8,
    /// JEDEC device ID
    pub jedec_device: u16,
    /// Total size in bytes
    pub total_size: u32,
    /// Page size in bytes
    pub page_size: u16,
    /// Full chip details (optional, for advanced use)
    pub chip: Option<FlashChip>,
}

impl From<&FlashContext> for ChipInfo {
    fn from(ctx: &FlashContext) -> Self {
        Self {
            vendor: ctx.chip.vendor.clone(),
            name: ctx.chip.name.clone(),
            jedec_manufacturer: ctx.chip.jedec_manufacturer,
            jedec_device: ctx.chip.jedec_device,
            total_size: ctx.chip.total_size,
            page_size: ctx.chip.page_size,
            chip: Some(ctx.chip.clone()),
        }
    }
}

/// Unified flash programming handle
///
/// This abstraction hides whether the underlying programmer is SPI-based
/// (CH341A, FTDI, etc.) or opaque (Intel internal). The CLI works only
/// with this type and never needs to know about SpiMaster or OpaqueMaster.
///
/// Similar to flashprog's `struct flashctx`.
///
/// The handle owns the flash device (which includes the programmer).
pub struct FlashHandle {
    /// The underlying flash device (type-erased, owned)
    device: Box<dyn FlashDevice>,
    /// Chip information (only available for SPI programmers where we probed)
    chip_info: Option<ChipInfo>,
}

impl FlashHandle {
    /// Create a new handle with chip information (SPI programmers)
    pub(crate) fn with_chip_info(device: Box<dyn FlashDevice>, chip_info: ChipInfo) -> Self {
        Self {
            device,
            chip_info: Some(chip_info),
        }
    }

    /// Create a new handle without chip information (opaque programmers)
    pub(crate) fn without_chip_info(device: Box<dyn FlashDevice>) -> Self {
        Self {
            device,
            chip_info: None,
        }
    }

    /// Get chip information, if available
    ///
    /// Returns `Some` for SPI programmers where we successfully probed the chip.
    /// Returns `None` for opaque programmers (e.g., Intel internal in hwseq mode).
    pub fn chip_info(&self) -> Option<&ChipInfo> {
        self.chip_info.as_ref()
    }

    /// Get flash size in bytes
    pub fn size(&self) -> u32 {
        self.device.size()
    }

    /// Read data from flash
    ///
    /// # Arguments
    /// * `addr` - Starting address (must be < flash size)
    /// * `buf` - Buffer to read into
    pub fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), Box<dyn std::error::Error>> {
        self.device.read(addr, buf).map_err(Into::into)
    }

    /// Write data to flash
    ///
    /// Note: This performs a raw write. For proper flash programming with
    /// erase and verification, use the higher-level functions in the commands module.
    ///
    /// # Arguments
    /// * `addr` - Starting address (must be < flash size)
    /// * `data` - Data to write
    pub fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
        self.device.write(addr, data).map_err(Into::into)
    }

    /// Erase flash region
    ///
    /// # Arguments
    /// * `addr` - Starting address (must be < flash size)
    /// * `len` - Length to erase in bytes
    pub fn erase(&mut self, addr: u32, len: u32) -> Result<(), Box<dyn std::error::Error>> {
        self.device.erase(addr, len).map_err(Into::into)
    }

    /// Get mutable reference to the underlying FlashDevice
    ///
    /// This is used by command implementations that need the FlashDevice trait.
    pub fn as_device_mut(&mut self) -> &mut dyn FlashDevice {
        self.device.as_mut()
    }

    /// Search for and read FMAP layout from flash
    ///
    /// This uses a binary search strategy (checking power-of-2 aligned offsets)
    /// followed by a linear search fallback if needed. This matches flashprog's
    /// approach for finding FMAP structures in flash chips.
    ///
    /// # Returns
    /// The parsed Layout from FMAP, or an error if no FMAP is found.
    ///
    /// # Example
    /// ```ignore
    /// let layout = handle.read_fmap()?;
    /// println!("Found {} regions", layout.len());
    /// ```
    pub fn read_fmap(
        &mut self,
    ) -> Result<rflasher_core::layout::Layout, Box<dyn std::error::Error>> {
        use rflasher_core::layout::search_fmap;

        log::debug!(
            "Chip size: {} bytes ({} MiB)",
            self.size(),
            self.size() / (1024 * 1024)
        );
        log::debug!("Searching for FMAP in flash chip...");

        let layout = search_fmap(self)?;
        log::debug!("Found FMAP with {} regions", layout.len());

        Ok(layout)
    }
}

/// Implement FmapSearchable for FlashHandle to enable generic FMAP search
impl rflasher_core::layout::FmapSearchable for FlashHandle {
    fn size(&self) -> u32 {
        self.device.size()
    }

    fn read_at(
        &mut self,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<(), rflasher_core::layout::LayoutError> {
        self.read(offset, buf)
            .map_err(|_| rflasher_core::layout::LayoutError::IoError)
    }
}

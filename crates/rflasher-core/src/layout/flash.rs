//! Functions to read layouts from flash chips
//!
//! These functions read IFD (Intel Flash Descriptor) and FMAP (Flash Map)
//! structures directly from a flash chip, rather than from a file.

use alloc::string::ToString;
use std::vec;

use crate::flash::{self, FlashContext};
use crate::programmer::SpiMaster;

use super::{Layout, LayoutError, fmap, has_ifd, ifd};

/// Size of the IFD region (first 4KB of flash)
const IFD_SIZE: usize = 0x1000;

/// Thin adapter exposing an `(&mut SpiMaster, &FlashContext)` pair to
/// [`fmap::search_fmap`] via [`fmap::FmapSearchable`].
struct SpiFlashFmapReader<'a, M: SpiMaster + ?Sized> {
    master: &'a mut M,
    ctx: &'a FlashContext,
}

impl<M: SpiMaster + ?Sized> fmap::FmapSearchable for SpiFlashFmapReader<'_, M> {
    fn size(&self) -> u32 {
        self.ctx.total_size() as u32
    }

    async fn read_at(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), LayoutError> {
        flash::read(self.master, self.ctx, offset, buf)
            .await
            .map_err(|e| LayoutError::IoError(e.to_string()))
    }
}

/// Read Intel Flash Descriptor from flash chip and parse into Layout
///
/// The IFD is located at the beginning of the flash (first 4KB).
/// This function reads that region and parses the descriptor.
///
/// # Arguments
/// * `master` - SPI master for flash communication
/// * `ctx` - Flash context with chip information
///
/// # Returns
/// A Layout parsed from the IFD, or an error if the IFD is not found or invalid.
///
/// # Example
/// ```ignore
/// let ctx = flash::probe(master, db)?;
/// let layout = read_ifd_from_flash(master, &ctx)?;
/// println!("Found {} regions", layout.len());
/// ```
pub async fn read_ifd_from_flash<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
) -> std::result::Result<Layout, LayoutError> {
    // IFD is always at the start - read first 4KB
    let mut buf = vec![0u8; IFD_SIZE];

    flash::read(master, ctx, 0, &mut buf)
        .await
        .map_err(|e| LayoutError::IoError(e.to_string()))?;

    if !has_ifd(&buf) {
        return Err(LayoutError::InvalidIfdSignature);
    }

    ifd::parse_ifd(&buf)
}

/// Read FMAP from flash chip and parse into Layout
///
/// Searches the flash for the FMAP signature via [`fmap::search_fmap`] using
/// binary search at power-of-2 offsets followed by a linear fallback.
///
/// # Arguments
/// * `master` - SPI master for flash communication
/// * `ctx` - Flash context with chip information
///
/// # Returns
/// A Layout parsed from the FMAP, or an error if no FMAP is found.
///
/// # Example
/// ```ignore
/// let ctx = flash::probe(master, db)?;
/// let layout = read_fmap_from_flash(master, &ctx)?;
/// println!("Found {} regions", layout.len());
/// ```
pub async fn read_fmap_from_flash<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
) -> std::result::Result<Layout, LayoutError> {
    let mut reader = SpiFlashFmapReader { master, ctx };
    fmap::search_fmap(&mut reader).await
}

/// Auto-detect and read layout from flash (tries IFD first, then FMAP)
///
/// This function attempts to detect the layout format automatically by:
/// 1. Checking for Intel Flash Descriptor at the start of flash
/// 2. If not found, searching for FMAP signature
///
/// # Arguments
/// * `master` - SPI master for flash communication
/// * `ctx` - Flash context with chip information
///
/// # Returns
/// A Layout from either IFD or FMAP, or an error if neither is found.
pub async fn read_layout_from_flash<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
) -> std::result::Result<Layout, LayoutError> {
    // Try IFD first (it's at offset 0, so quick to check)
    if let Ok(layout) = read_ifd_from_flash(master, ctx).await {
        return Ok(layout);
    }

    // Try FMAP
    read_fmap_from_flash(master, ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chip::{ChipTestStatus, EraseBlock, Features, FlashChip, WriteGranularity};
    use crate::error::Result as CoreResult;
    use crate::programmer::SpiFeatures;
    use crate::spi::{SpiCommand, opcodes};
    use alloc::vec::Vec;

    struct CountingSpiFlash {
        data: Vec<u8>,
        bytes_read: usize,
    }

    impl SpiMaster for CountingSpiFlash {
        fn features(&self) -> SpiFeatures {
            SpiFeatures::empty()
        }

        fn max_read_len(&self) -> usize {
            4096
        }

        fn max_write_len(&self) -> usize {
            256
        }

        async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
            match cmd.opcode {
                opcodes::READ => {
                    let addr = cmd.address.unwrap_or(0) as usize;
                    let len = cmd.read_buf.len();
                    cmd.read_buf.copy_from_slice(&self.data[addr..addr + len]);
                    self.bytes_read += len;
                    Ok(())
                }
                _ => Ok(()),
            }
        }

        async fn delay_us(&mut self, _us: u32) {}
    }

    fn test_ctx(size: u32) -> FlashContext {
        let chip = FlashChip {
            vendor: "Test".into(),
            name: "FMAP64K".into(),
            jedec_manufacturer: 0xEF,
            jedec_device: 0x4010,
            ids: Default::default(),
            total_size: size,
            page_size: 256,
            features: Features::empty(),
            unlock: Default::default(),
            voltage_min_mv: 2700,
            voltage_max_mv: 3600,
            write_granularity: WriteGranularity::Page,
            erase_blocks: vec![EraseBlock::with_count(0x20, 4096, size / 4096)].into(),
            tested: ChipTestStatus::default(),
        };
        FlashContext::new(chip)
    }

    fn build_fmap(size: u32, areas: &[(&str, u32, u32)]) -> Vec<u8> {
        let mut out = vec![0u8; 56 + areas.len() * 42];
        out[0..8].copy_from_slice(b"__FMAP__");
        out[8] = 1;
        out[9] = 1;
        out[18..22].copy_from_slice(&size.to_le_bytes());
        out[22..26].copy_from_slice(b"FMAP");
        out[54..56].copy_from_slice(&(areas.len() as u16).to_le_bytes());
        for (i, (name, offset, area_size)) in areas.iter().enumerate() {
            let base = 56 + i * 42;
            out[base..base + 4].copy_from_slice(&offset.to_le_bytes());
            out[base + 4..base + 8].copy_from_slice(&area_size.to_le_bytes());
            out[base + 8..base + 8 + name.len()].copy_from_slice(name.as_bytes());
        }
        out
    }

    #[test]
    fn aligned_nonempty_fmap_avoids_full_chip_read() {
        let size = 64 * 1024;
        let mut rom = vec![0xFFu8; size];
        let table = build_fmap(size as u32, &[("RO", 0, 0x8000), ("RW", 0x8000, 0x8000)]);
        rom[0x8000..0x8000 + table.len()].copy_from_slice(&table);

        let mut master = CountingSpiFlash {
            data: rom,
            bytes_read: 0,
        };
        let ctx = test_ctx(size as u32);
        let layout = futures_lite::future::block_on(read_fmap_from_flash(&mut master, &ctx))
            .expect("aligned FMAP should be found");

        assert_eq!(layout.regions.len(), 2);
        assert_eq!(layout.regions[0].name, "RO");
        assert_eq!(layout.regions[1].name, "RW");
        assert!(
            master.bytes_read < size / 4,
            "expected binary search without full-chip read, read {} bytes",
            master.bytes_read
        );
    }

    #[test]
    fn false_signature_before_valid_aligned_fmap_avoids_full_chip_read() {
        let size = 64 * 1024;
        let mut rom = vec![0xFFu8; size];
        // False signature at 0x8000 (first stride hit) with plausible nareas=1
        // and unsupported major version, so validation reaches UnsupportedFmapVersion.
        let mut bad = build_fmap(size as u32, &[("BAD", 0, 0x1000)]);
        bad[8] = 99;
        rom[0x8000..0x8000 + bad.len()].copy_from_slice(&bad);
        // Valid FMAP at 0x4000 (second stride level)
        let table = build_fmap(size as u32, &[("BIOS", 0, 0x10000)]);
        rom[0x4000..0x4000 + table.len()].copy_from_slice(&table);

        let mut master = CountingSpiFlash {
            data: rom,
            bytes_read: 0,
        };
        let ctx = test_ctx(size as u32);
        let layout = futures_lite::future::block_on(read_fmap_from_flash(&mut master, &ctx))
            .expect("valid FMAP after unsupported-version candidate should be found");

        assert_eq!(layout.regions.len(), 1);
        assert_eq!(layout.regions[0].name, "BIOS");
        assert!(
            master.bytes_read < size / 4,
            "expected binary search recovery without full-chip read, read {} bytes",
            master.bytes_read
        );
    }

    #[test]
    fn unsupported_version_reported_when_no_valid_fmap_exists() {
        let size = 64 * 1024;
        let mut rom = vec![0xFFu8; size];
        let mut bad = build_fmap(size as u32, &[("FUTURE", 0, 0x1000)]);
        bad[8] = 99;
        rom[0x8000..0x8000 + bad.len()].copy_from_slice(&bad);

        let mut master = CountingSpiFlash {
            data: rom,
            bytes_read: 0,
        };
        let ctx = test_ctx(size as u32);
        let err = futures_lite::future::block_on(read_fmap_from_flash(&mut master, &ctx))
            .expect_err("unsupported-version FMAP should fail when no valid table exists");

        assert!(matches!(err, LayoutError::UnsupportedFmapVersion));
    }
}

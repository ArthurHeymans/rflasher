//! Shared preflight for CLI, web and library mutations.

use super::{FlashDevice, operations::plan_optimal_erase_region};
use crate::layout::{Layout, Region};
use crate::{Error, Result};
use alloc::vec;

/// Store a recoverable image before erasing outside the requested region.
/// Returning success promises that the image survives process/power failure.
/// RAM-only implementations must not report success.
pub trait RecoveryBackup {
    /// Persist a full image and synchronize its data and directory metadata.
    fn persist(&mut self, image: &[u8]) -> Result<()>;
}

/// Independent authorizations, never implied by a chip/SFDP override.
#[derive(Default)]
pub struct MutationPolicy<'a> {
    /// Authorize dangerous-region changes and temporary erase/restore.
    pub allow_dangerous: bool,
    /// Authorize a selection covering the entire device.
    pub allow_full_chip: bool,
    /// Durable recovery storage required by cross-region erase footprints.
    pub recovery: Option<&'a mut dyn RecoveryBackup>,
}

/// Validate every included region and erase footprint before any mutation.
pub(super) async fn preflight<D: FlashDevice + ?Sized>(
    device: &mut D,
    layout: &Layout,
    policy: &mut MutationPolicy<'_>,
) -> Result<()> {
    layout
        .validate(device.size())
        .map_err(|_| Error::LayoutError)?;
    let regions: alloc::vec::Vec<_> = layout.included_regions().collect();
    if regions
        .iter()
        .any(|r| r.readonly || (r.dangerous && !policy.allow_dangerous))
    {
        return Err(Error::RegionProtected);
    }
    if regions
        .iter()
        .map(|r| r.end as u64 - r.start as u64 + 1)
        .sum::<u64>()
        == device.size() as u64
        && !policy.allow_full_chip
    {
        log::error!("Full-chip mutation requires explicit authorization");
        return Err(Error::RegionProtected);
    }

    // Identify the descriptor on the device itself, not just in a caller's
    // supplied layout (which could hide or rename dangerous regions).
    let mut protected_regions = layout.regions.clone();
    {
        let mut header = vec![0; (device.size() as usize).min(4096)];
        device.read(0, &mut header).await?;
        if crate::layout::has_ifd(&header) {
            let actual = crate::layout::parse_ifd(&header).map_err(|_| Error::LayoutError)?;
            actual
                .validate(device.size())
                .map_err(|_| Error::LayoutError)?;
            if !policy.allow_dangerous
                && actual.regions.iter().any(|protected| {
                    protected.dangerous && regions.iter().any(|r| overlaps(r, protected))
                })
            {
                return Err(Error::RegionProtected);
            }
            protected_regions.extend(actual.regions);
        }
    }

    let mut cross_boundary = false;
    for region in regions {
        let plan = plan_optimal_erase_region(
            device.erase_blocks(),
            device.size(),
            region.start,
            region.end,
        )?;
        for op in plan {
            device.validate_erase_operation(&op)?;
            if op.start < region.start || op.start as u64 + op.size as u64 > region.end as u64 + 1 {
                cross_boundary = true;
                // An erase/restore still temporarily destroys neighboring
                // protected data. Its footprint needs the same authorization.
                if !policy.allow_dangerous
                    && protected_regions.iter().any(|r| {
                        r.dangerous
                            && (r.start as u64) < op.start as u64 + op.size as u64
                            && r.end >= op.start
                    })
                {
                    return Err(Error::RegionProtected);
                }
                if protected_regions.iter().any(|r| {
                    r.readonly
                        && (r.start as u64) < op.start as u64 + op.size as u64
                        && r.end >= op.start
                }) {
                    return Err(Error::RegionProtected);
                }
            }
        }
    }
    if cross_boundary {
        let sink = policy.recovery.as_mut().ok_or_else(|| {
            log::error!("Erase crosses a region boundary; a durable recovery backup is required");
            Error::RegionProtected
        })?;
        // Snapshot once, before the first destructive command, rather than
        // overwriting recovery data after earlier regions have been changed.
        let mut image = vec![0; device.size() as usize];
        device.read(0, &mut image).await?;
        sink.persist(&image)?;
    }
    Ok(())
}

fn overlaps(a: &Region, b: &Region) -> bool {
    a.start <= b.end && b.start <= a.end
}

pub(super) fn region_layout(start: u32, len: u32) -> Result<Layout> {
    let end = start
        .checked_add(len)
        .and_then(|v| v.checked_sub(1))
        .ok_or(Error::AddressOutOfBounds)?;
    let mut layout = Layout::new();
    let mut region = Region::new("requested", start, end);
    region.included = true;
    layout.add_region(region);
    Ok(layout)
}

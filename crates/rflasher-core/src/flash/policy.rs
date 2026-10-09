//! Shared preflight for CLI, web and library mutations.

use super::{FlashDevice, operations::plan_optimal_erase_region};
use crate::layout::{Layout, Region};
use crate::{Error, Refusal, Result};
use alloc::vec;

/// Store a full-device image before the first mutation, so an interrupted
/// operation (e.g. power loss during an erase/restore of neighboring data)
/// can be recovered. Returning success promises that the image survives
/// process/power failure.
pub trait RecoveryBackup {
    /// Persist a full image and synchronize its data and directory metadata.
    fn persist(&mut self, image: &[u8]) -> Result<()>;
}

/// Independent authorizations, never implied by a chip/SFDP override.
#[derive(Default)]
pub struct MutationPolicy<'a> {
    /// Authorize dangerous-region changes and temporary erase/restore.
    pub allow_dangerous: bool,
    /// Optional durable storage for a full image taken before any mutation.
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
    if let Some(r) = regions.iter().find(|r| r.readonly) {
        return Err(refuse(
            Refusal::ReadOnlyRegion,
            format_args!("region '{}' is read-only", r.name),
        ));
    }
    if let Some(r) = regions
        .iter()
        .find(|r| r.dangerous && !policy.allow_dangerous)
    {
        return Err(refuse(
            Refusal::DangerousRegion,
            format_args!("region '{}' is dangerous; {}", r.name, DANGEROUS_HINT),
        ));
    }
    // Identify the descriptor on the device itself, not just in a caller's
    // supplied layout (which could hide or rename dangerous regions).
    let mut protected_regions = layout.regions.clone();
    {
        let mut header = vec![0; (device.size() as usize).min(4096)];
        device.read(0, &mut header).await?;
        if crate::layout::has_ifd(&header) {
            match crate::layout::parse_ifd(&header)
                .and_then(|ifd| ifd.validate(device.size()).map(|()| ifd))
            {
                Ok(actual) => {
                    if !policy.allow_dangerous
                        && let Some(p) = actual
                            .regions
                            .iter()
                            .find(|p| p.dangerous && regions.iter().any(|r| overlaps(r, p)))
                    {
                        return Err(refuse(
                            Refusal::DangerousRegion,
                            format_args!(
                                "the flash descriptor on the chip marks '{}' as dangerous; {}",
                                p.name, DANGEROUS_HINT
                            ),
                        ));
                    }
                    protected_regions.extend(actual.regions);
                }
                // A damaged descriptor must not prevent reflashing the chip,
                // but without it dangerous regions cannot be located.
                Err(e) if policy.allow_dangerous => {
                    log::warn!("Ignoring unusable flash descriptor on the chip: {:?}", e);
                }
                Err(e) => {
                    return Err(refuse(
                        Refusal::UnusableDescriptor,
                        format_args!(
                            "the chip has an unusable flash descriptor ({:?}), so dangerous \
                             regions cannot be located; {}",
                            e, DANGEROUS_HINT
                        ),
                    ));
                }
            }
        }
    }

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
                // An erase/restore still temporarily destroys neighboring
                // protected data. Its footprint needs the same authorization.
                let touched = |r: &&Region| {
                    (r.start as u64) < op.start as u64 + op.size as u64 && r.end >= op.start
                };
                if let Some(r) = protected_regions
                    .iter()
                    .filter(touched)
                    .find(|r| r.readonly)
                {
                    return Err(refuse(
                        Refusal::ReadOnlyRegion,
                        format_args!(
                            "erasing {:#x}+{:#x} would temporarily erase read-only region '{}'",
                            op.start, op.size, r.name
                        ),
                    ));
                }
                if !policy.allow_dangerous
                    && let Some(r) = protected_regions
                        .iter()
                        .filter(touched)
                        .find(|r| r.dangerous)
                {
                    return Err(refuse(
                        Refusal::DangerousRegion,
                        format_args!(
                            "erasing {:#x}+{:#x} would temporarily erase dangerous region '{}'; {}",
                            op.start, op.size, r.name, DANGEROUS_HINT
                        ),
                    ));
                }
            }
        }
    }
    if let Some(sink) = policy.recovery.as_mut() {
        // Snapshot once, before the first destructive command, rather than
        // overwriting recovery data after earlier regions have been changed.
        let mut image = vec![0; device.size() as usize];
        device.read(0, &mut image).await?;
        sink.persist(&image)?;
    }
    Ok(())
}

const DANGEROUS_HINT: &str =
    "exclude it or authorize dangerous regions (--allow-dangerous-regions)";

/// Log the details of a refusal (region names, addresses) and return the
/// error naming the missing authorization.
fn refuse(refusal: Refusal, details: core::fmt::Arguments<'_>) -> Error {
    log::error!("Refusing to modify flash: {}", details);
    Error::MutationRefused(refusal)
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

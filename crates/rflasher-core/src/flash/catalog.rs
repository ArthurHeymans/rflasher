//! Probing a board-selected catalog, without SFDP synthesis or allocation.

use super::FlashContext;
use crate::chip::{ChipCatalog, ChipIdentity, ResolveError};
use crate::error::{Error, Result};
use crate::programmer::SpiMaster;
use crate::protocol;

/// Identify a connected chip among the profiles a board accepts.
///
/// Reads RDID, looks the identity up without guessing, and borrows the
/// selected descriptor's static data into a context. This complete path is
/// available with or without `alloc`/`std`. An unknown identity never falls
/// back to a default, an SFDP-generated descriptor or a legacy identity.
///
/// Standard RDID and the audited bank-one PMC/AMIC identities are supported.
/// For legacy RES2 parts, explicitly read the appropriate protocol and call
/// [`ChipCatalog::resolve_identity`] with [`ChipIdentity::Res2`].
pub async fn probe_catalog<M: SpiMaster + ?Sized>(
    master: &mut M,
    catalog: &ChipCatalog<'_>,
) -> Result<FlashContext> {
    let (manufacturer, device) = protocol::read_jedec_id(master).await?;
    let chip = if manufacturer != 0x7f {
        catalog.resolve(manufacturer, device)
    } else {
        let banked = protocol::read_banked_jedec_id(master, device).await?;
        catalog.resolve_identity(ChipIdentity::BankedJedec {
            bank: 1,
            manufacturer: banked.manufacturer,
            device: banked.device_bytes(),
        })
    }
    .map_err(|e| match e {
        ResolveError::Unknown => Error::ChipNotFound,
        ResolveError::Ambiguous => Error::ChipAmbiguous,
    })?;
    master.chip_identified(chip);
    Ok(FlashContext::new(chip.clone()))
}

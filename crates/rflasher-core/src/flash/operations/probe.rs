//! Chip probing, JEDEC/SFDP disambiguation, and common-profile synthesis.

#[cfg(feature = "std")]
use alloc::vec::Vec;

#[cfg(feature = "std")]
use crate::chip::{ChipProvider, Features};
#[cfg(feature = "std")]
use crate::error::{Error, Result};
#[cfg(feature = "std")]
use crate::flash::context::FlashContext;
#[cfg(feature = "std")]
use crate::programmer::SpiMaster;
#[cfg(feature = "std")]
use crate::protocol;

#[cfg(feature = "std")]
use super::spi_ops::check_spi_eraser;

/// Result of a comprehensive chip probe
///
/// This structure contains all information gathered during probing,
/// including SFDP data and any mismatches with the database.
#[cfg(feature = "std")]
#[derive(Debug)]
pub struct ProbeResult {
    /// First raw RDID byte (a continuation code for banked identities).
    pub jedec_manufacturer: u8,
    /// Second and third raw RDID bytes, in big-endian order.
    pub jedec_device: u16,
    /// The chip to use for operations
    pub chip: crate::chip::FlashChip,
    /// Whether the chip was found in the database
    pub from_database: bool,
    /// SFDP information (if available)
    pub sfdp: Option<crate::sfdp::SfdpInfo>,
    /// Mismatches between SFDP and database (if both available)
    pub mismatches: Vec<crate::sfdp::SfdpMismatch>,
}

#[cfg(feature = "std")]
impl ProbeResult {
    /// Check if there are any mismatches between SFDP and database
    pub fn has_mismatches(&self) -> bool {
        !self.mismatches.is_empty()
    }

    /// Check if there are critical mismatches (see [`SfdpMismatch::is_critical`])
    ///
    /// [`SfdpMismatch::is_critical`]: crate::sfdp::SfdpMismatch::is_critical
    pub fn has_critical_mismatches(&self) -> bool {
        self.mismatches
            .iter()
            .any(crate::sfdp::SfdpMismatch::is_critical)
    }

    /// Create a FlashContext from this probe result
    pub fn into_context(self) -> FlashContext {
        FlashContext::new(self.chip)
    }
}

/// How a probed chip is matched against the chip database
#[cfg(feature = "std")]
#[derive(Debug, Clone, Copy, Default)]
pub struct ProbeOptions<'a> {
    /// Select the database entry with this name instead of deciding by ID.
    ///
    /// Needed when several entries share the probed JEDEC ID and cannot be
    /// told apart automatically. Matched against entry names with
    /// [`name_matches`](crate::chip::name_matches), i.e. ignoring case and
    /// honouring the database's `A/B`, `(B)` and `.` name patterns.
    pub chip: Option<&'a str>,
    /// Proceed although the database entry contradicts the chip's SFDP data
    /// in a way that makes operating on it unsafe (see
    /// [`SfdpMismatch::is_critical`](crate::sfdp::SfdpMismatch::is_critical)).
    pub force: bool,
}

/// Pick the database entry for a probed JEDEC ID.
///
/// JEDEC IDs are not unique, so several entries may match. Alias entries that
/// behave identically are interchangeable and the first is used. Otherwise the
/// chip's own SFDP data eliminates entries that contradict it on size, page
/// size or the block size of an erase opcode. Any remaining aliases use a safe
/// common operation profile; conflicting essential procedures require explicit
/// selection.
#[cfg(feature = "std")]
pub(super) fn select_database_chip<P>(
    provider: &P,
    manufacturer: u8,
    device: u16,
    sfdp: Option<&crate::sfdp::SfdpInfo>,
    wanted: Option<&str>,
) -> Result<Option<crate::chip::FlashChip>>
where
    P: ChipProvider + ?Sized,
{
    let candidates = (0..)
        .map_while(|i| provider.find_nth_by_jedec_id(manufacturer, device, i))
        .collect();
    select_candidates(candidates, manufacturer, device, sfdp, wanted)
}

#[cfg(feature = "std")]
fn select_candidates(
    mut candidates: Vec<&crate::chip::FlashChip>,
    manufacturer: u8,
    device: u16,
    sfdp: Option<&crate::sfdp::SfdpInfo>,
    wanted: Option<&str>,
) -> Result<Option<crate::chip::FlashChip>> {
    let names = |chips: &[&crate::chip::FlashChip]| -> std::string::String {
        chips
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>()
            .join(", ")
    };

    if let Some(wanted) = wanted {
        let matching: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|c| {
                c.name().eq_ignore_ascii_case(wanted)
                    || (!c.features.contains(Features::EXPLICIT_PROFILE)
                        && crate::chip::name_matches(c.name(), wanted))
            })
            .collect();
        if matching.is_empty() {
            log::error!(
                "No chip definition named \"{}\" matches JEDEC ID {:02X}:{:04X}{}",
                wanted,
                manufacturer,
                device,
                if candidates.is_empty() {
                    std::string::String::new()
                } else {
                    std::format!(" (candidates: {})", names(&candidates))
                }
            );
            return Err(Error::ChipNotFound);
        }
        // Exact database names (including slash/optional patterns) are
        // selectable by the web UI. A loose pattern is not authorization to
        // choose an arbitrary, conflicting procedure.
        if let Some(chosen) = matching
            .iter()
            .find(|c| c.name().eq_ignore_ascii_case(wanted))
        {
            return Ok(Some((*chosen).clone()));
        }
        if matching.len() == 1 || matching.iter().all(|c| c.is_equivalent_to(matching[0])) {
            return Ok(Some(matching[0].clone()));
        }
        return common_operation_profile(&matching)
            .map(Some)
            .ok_or(Error::ChipAmbiguous);
    }

    candidates.retain(|c| !c.features.contains(Features::EXPLICIT_PROFILE));
    let Some(&first) = candidates.first() else {
        return Ok(None);
    };
    if candidates.iter().all(|c| c.is_equivalent_to(first)) {
        if candidates.len() > 1 {
            log::debug!(
                "Interchangeable chip definitions for {:02X}:{:04X}: {}",
                manufacturer,
                device,
                names(&candidates)
            );
        }
        return Ok(Some(first.clone()));
    }

    if let Some(sfdp) = sfdp {
        // Missing optional capabilities are not evidence of chip identity, but
        // facts SFDP states are: size and page size first, then erase opcodes
        // SFDP assigns a different block size than the entry does.
        let score = |c: &crate::chip::FlashChip| {
            let mismatches = crate::sfdp::compare_with_chip(sfdp, c);
            (
                mismatches.iter().filter(|m| m.is_critical()).count(),
                sfdp_erase_conflicts(sfdp, c),
            )
        };
        let best = candidates.iter().map(|c| score(c)).min();
        let best_matches: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|c| Some(score(c)) == best)
            .collect();
        if best_matches
            .iter()
            .all(|c| c.is_equivalent_to(best_matches[0]))
        {
            log::info!(
                "Several chip definitions match {:02X}:{:04X} ({}); chose {} by its SFDP data",
                manufacturer,
                device,
                names(&candidates),
                best_matches[0].name()
            );
            return Ok(Some(best_matches[0].clone()));
        }
        candidates = best_matches;
    }

    if let Some(profile) = common_operation_profile(&candidates) {
        log::warn!(
            "Unresolved aliases {}; using only their common operations",
            names(&candidates)
        );
        return Ok(Some(profile));
    }

    log::error!(
        "Multiple chip definitions match JEDEC ID {:02X}:{:04X} and differ in erase \
         geometry or features: {}. Select one explicitly with --chip.",
        manufacturer,
        device,
        names(&candidates)
    );
    Err(Error::ChipAmbiguous)
}

/// Number of the entry's uniform erase opcodes for which SFDP gives a different
/// block size (e.g. `0x20` erasing 64 KiB on old Macronix parts and 4 KiB on
/// their successors). Such an entry describes a different chip.
#[cfg(feature = "std")]
fn sfdp_erase_conflicts(sfdp: &crate::sfdp::SfdpInfo, chip: &crate::chip::FlashChip) -> usize {
    chip.erase_blocks()
        .iter()
        .filter(|b| !b.is_chip_erase() && b.is_uniform())
        .filter(|b| {
            sfdp.basic_params
                .erase_types
                .iter()
                .any(|e| e.is_valid() && e.opcode == b.opcode && e.size != b.min_block_size())
        })
        .count()
}

/// Build a conservative operational profile, not an arbitrary database row.
/// Only shared commands survive. Missing optional flags do not prove that two
/// aliases require conflicting procedures; essential program/address modes do.
#[cfg(feature = "std")]
pub(super) fn common_operation_profile(
    chips: &[&crate::chip::FlashChip],
) -> Option<crate::chip::FlashChip> {
    let first = *chips.first()?;
    let required = Features::AAI_WORD
        | Features::WRITE_BYTE
        | Features::SST26_BPR
        | Features::WP_CMP
        | Features::WP_CMP_SR2;
    let addressing = Features::PROCEDURE
        - (Features::WRSR_WREN
            | Features::WRSR_EWSR
            | Features::WRSR_EXT
            | Features::WP_BP3
            | Features::WRSR_VOLATILE_WREN
            | Features::WRSR_PERSISTENT_EWSR);
    if chips.iter().any(|c| {
        c.unlock != first.unlock
            || c.total_size != first.total_size
            || c.write_granularity != first.write_granularity
            || c.features & required != first.features & required
            || (first.requires_4byte_addr()
                && c.features & addressing != first.features & addressing)
    }) {
        return None;
    }
    let mut profile = first.clone();
    // A synthesized profile has not itself been hardware-tested.
    profile.tested = crate::chip::ChipTestStatus::default();
    profile.name = chips
        .iter()
        .map(|c| c.name())
        .collect::<Vec<_>>()
        .join(" / ")
        .into();
    profile.page_size = chips.iter().map(|c| c.page_size).min()?;
    if profile.page_size == 0 {
        return None;
    }
    profile.features = chips.iter().fold(first.features, |f, c| f & c.features);
    // WRSR_EXT describes a required wire format, not an optional command.
    // Intersecting it away would turn an extended write into a one-byte WRSR.
    // If any alias may have SR2, retain that hazard even when another alias
    // lacks the descriptive flag. It is not evidence that one-byte WRSR is safe.
    if chips.iter().any(|c| {
        c.features
            .intersects(Features::STATUS_REG_2 | Features::QE_SR2)
    }) {
        profile.features |= Features::STATUS_REG_2;
    }
    if chips.iter().any(|c| {
        c.features.contains(Features::WRSR_EXT) != first.features.contains(Features::WRSR_EXT)
    }) {
        profile.features.remove(
            Features::WRSR_WREN
                | Features::WRSR_EWSR
                | Features::WRSR_VOLATILE_WREN
                | Features::WRSR_PERSISTENT_EWSR,
        );
        profile.features |= Features::WP_UNRESOLVED;
    }
    // Only flags that change the WP register bit map count (see
    // `SpiFlashDevice::wp_bit_map`); SST26_BPR is already required to match.
    // Descriptive flags such as STATUS_REG_2 or WP_TB are not used by WP
    // access, so differences there must not disable it.
    if chips.iter().any(|c| {
        let layout = Features::WP_BP3 | Features::WP_WPS;
        c.features & layout != first.features & layout
    }) {
        profile.features |= Features::WP_UNRESOLVED;
    }
    profile
        .erase_blocks
        .to_mut()
        .retain(|block| chips.iter().all(|c| c.erase_blocks().contains(block)));
    // Do not substitute an addressless chip erase for missing addressed geometry.
    if !profile.erase_blocks.iter().any(|b| !b.is_chip_erase()) {
        return None;
    }
    profile.voltage_min_mv = chips.iter().map(|c| c.voltage_min_mv).max()?;
    profile.voltage_max_mv = chips.iter().map(|c| c.voltage_max_mv).min()?;
    if profile.voltage_min_mv > profile.voltage_max_mv {
        return None;
    }
    if chips.iter().any(|c| {
        c.tested.write == crate::chip::TestStatus::Bad
            || c.tested.erase == crate::chip::TestStatus::Bad
    }) {
        return None;
    }
    Some(profile)
}

/// Resolve a raw three-byte RDID, preserving the legacy representation for callers.
#[cfg(feature = "std")]
pub(crate) async fn select_probed_chip<M: SpiMaster + ?Sized, P: ChipProvider + ?Sized>(
    master: &mut M,
    provider: &P,
    manufacturer: u8,
    device: u16,
    sfdp: Option<&crate::sfdp::SfdpInfo>,
    wanted: Option<&str>,
) -> Result<Option<crate::chip::FlashChip>> {
    if manufacturer != 0x7f {
        return select_database_chip(provider, manufacturer, device, sfdp, wanted);
    }
    let banked = protocol::read_banked_jedec_id(master, device).await?;
    let candidates = if banked.device_bytes().is_empty() {
        Vec::new()
    } else {
        (0..)
            .map_while(|i| {
                provider.find_nth_by_banked_jedec_id(
                    1,
                    banked.manufacturer,
                    banked.device_bytes(),
                    i,
                )
            })
            .collect()
    };
    select_candidates(
        candidates,
        banked.manufacturer,
        banked.device_id(),
        sfdp,
        wanted,
    )
}

/// Probe for a flash chip with detailed results
///
/// Equivalent to [`probe_with_options`] with default options.
#[cfg(feature = "std")]
pub async fn probe_detailed<M, P>(master: &mut M, provider: &P) -> Result<ProbeResult>
where
    M: SpiMaster + ?Sized,
    P: ChipProvider + ?Sized,
{
    probe_with_options(master, provider, &ProbeOptions::default()).await
}

/// Probe for a flash chip with detailed results
///
/// This function performs comprehensive probing:
/// 1. Reads JEDEC ID
/// 2. Probes SFDP (if supported)
/// 3. Looks up in database, resolving JEDEC ID collisions (see
///    [`ProbeOptions::chip`])
/// 4. Compares SFDP with database (if both available) and refuses a database
///    entry that contradicts SFDP on size or page size unless
///    [`ProbeOptions::force`] is set
///
/// Returns detailed information about what was found, allowing the caller
/// to decide how to handle mismatches or unknown chips.
#[cfg(feature = "std")]
pub async fn probe_with_options<M, P>(
    master: &mut M,
    provider: &P,
    options: &ProbeOptions<'_>,
) -> Result<ProbeResult>
where
    M: SpiMaster + ?Sized,
    P: ChipProvider + ?Sized,
{
    let (jedec_manufacturer, jedec_device) = protocol::read_jedec_id(master).await?;

    log::info!(
        "JEDEC ID: manufacturer=0x{:02X}, device=0x{:04X}",
        jedec_manufacturer,
        jedec_device
    );

    // Try SFDP probing
    log::debug!("Attempting SFDP probe...");

    let mut sfdp_absent = false;
    let sfdp = match crate::sfdp::probe(master).await {
        Ok(info) => {
            log::info!(
                "SFDP probe successful: {} bytes, page size {} bytes",
                info.total_size(),
                info.page_size()
            );
            Some(info)
        }
        Err(e) => {
            log::debug!("SFDP probe failed: {:?}", e);
            // A transport failure does not prove that SFDP is absent.
            sfdp_absent = e == Error::ChipNotSupported;
            None
        }
    };

    let selection = select_probed_chip(
        master,
        provider,
        jedec_manufacturer,
        jedec_device,
        sfdp.as_ref(),
        options.chip,
    )
    .await;
    let mut db_chip = match selection {
        Err(Error::ChipNotFound) if options.chip.is_some() && sfdp.is_none() && sfdp_absent => {
            // A requested name may refer to a legacy identity, but never let
            // it override an identifiable RDID chip.
            match select_probed_chip(
                master,
                provider,
                jedec_manufacturer,
                jedec_device,
                None,
                None,
            )
            .await
            {
                Ok(None) => None,
                _ => return Err(Error::ChipNotFound),
            }
        }
        other => other?,
    };
    // A controller limited to three-byte reads cannot resolve AMIC's four-byte
    // continuation ID. Do not turn that uncertainty into a legacy match.
    if db_chip.is_none()
        && sfdp.is_none()
        && sfdp_absent
        && !(jedec_manufacturer == 0x7f && jedec_device >> 8 == 0x37)
    {
        // RES2 is 0xAB followed by three zero bytes and two response bytes.
        // Never interpret a failed transfer as an identity.
        let mut id = [0; 2];
        let mut cmd = crate::spi::SpiCommand::read_3b(crate::spi::opcodes::RES, 0, &mut id);
        match master.execute(&mut cmd).await {
            Ok(()) if id != [0, 0] && id != [0xff, 0xff] => {
                let candidates: Vec<_> = (0..)
                    .map_while(|i| provider.find_nth_by_res2_id(id[0], id[1], i))
                    .collect();
                if candidates.len() > 1 && options.chip.is_none() {
                    return Err(Error::ChipAmbiguous);
                }
                db_chip =
                    select_candidates(candidates, id[0], u16::from(id[1]), None, options.chip)?;
            }
            Ok(()) | Err(Error::OpcodeNotSupported) => {}
            Err(e) => return Err(e),
        }
    }
    if db_chip.is_some() {
        log::debug!("Chip found in database");
    } else {
        log::info!(
            "Chip not in database (JEDEC {:02X}:{:04X})",
            jedec_manufacturer,
            jedec_device
        );
    }

    // Determine the chip to use and collect mismatches
    let (chip, from_database, mismatches) = match (&db_chip, &sfdp) {
        (Some(db), Some(sfdp_info)) => {
            let mismatches = crate::sfdp::compare_with_chip(sfdp_info, db);
            ((*db).clone(), true, mismatches)
        }
        (Some(db), None) => ((*db).clone(), true, Vec::new()),
        (None, Some(sfdp_info)) => {
            let chip = crate::sfdp::to_flash_chip(sfdp_info, jedec_manufacturer, jedec_device);
            (chip, false, Vec::new())
        }
        (None, None) => return Err(Error::ChipNotFound),
    };

    let mut result = ProbeResult {
        jedec_manufacturer,
        jedec_device,
        chip,
        from_database,
        sfdp,
        mismatches,
    };

    if result.has_critical_mismatches() && !options.force {
        for mismatch in result.mismatches.iter().filter(|m| m.is_critical()) {
            log::error!("{}: {}", result.chip.name(), mismatch);
        }
        log::error!(
            "The database entry contradicts the chip's SFDP data, so operating on it could \
             corrupt data. Select the right entry with --chip, or override with --force."
        );
        return Err(Error::ChipMismatch);
    }

    if let Some(info) = &result.sfdp {
        // BFPT can disprove an addressed erase opcode but does not advertise
        // addressless chip erase. Never execute a disputed database eraser.
        result.chip.erase_blocks.to_mut().retain(|block| {
            block.is_chip_erase()
                || (block.is_uniform()
                    && info.basic_params.erase_types.iter().any(|e| {
                        e.is_valid() && e.size == block.min_block_size() && e.opcode == block.opcode
                    }))
        });
        if info.four_byte_addr_table.is_some() {
            let validated = crate::sfdp::to_flash_chip(info, jedec_manufacturer, jedec_device);
            for block in result.chip.erase_blocks.iter_mut() {
                if !block.is_chip_erase() {
                    let known = validated.erase_blocks.iter().find(|b| {
                        b.opcode == block.opcode && b.min_block_size() == block.min_block_size()
                    });
                    if block.opcode_4b != known.and_then(|b| b.opcode_4b) {
                        // Keep the validated three-byte opcode, but do not
                        // execute an unadvertised native four-byte command.
                        block.opcode_4b = None;
                    }
                }
            }
        }
    }

    master.chip_identified(&result.chip);
    // Like flashprog, leave out erasers this programmer cannot issue (e.g. a
    // locked Intel opcode menu without chip erase), so that erases are
    // planned with smaller ones it can send instead of being refused.
    let (features, four_byte) = (result.chip.features, result.chip.requires_4byte_addr());
    result.chip.erase_blocks.to_mut().retain(|block| {
        let usable = check_spi_eraser(&*master, features, four_byte, block).is_ok();
        if !usable {
            log::info!(
                "Programmer cannot send erase opcode {:#04x}; not using it",
                block.opcode
            );
        }
        usable
    });
    Ok(result)
}

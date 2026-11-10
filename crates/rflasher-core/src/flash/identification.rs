//! Collect method-specific responses, retaining unknown catalog alternatives.
use crate::chip::{ChipIdentity, ChipProvider, FlashChip, ProbeMethod};
use crate::error::{Error, Result};
use crate::programmer::SpiMaster;
use crate::spi::SpiCommand;
use alloc::vec::Vec;

/// Decode RDID without truncating a continuation-prefixed identity. The PMC
/// bank-one family has one device byte; other banked identities need four bytes.
pub(super) async fn rdid_identity<M: SpiMaster + ?Sized>(
    master: &mut M,
    manufacturer: u8,
    device: u16,
) -> Result<Option<ChipIdentity>> {
    let raw = (u32::from(manufacturer) << 16) | u32::from(device);
    if manufacturer != 0x7f || device >> 8 == 0x9d {
        return Ok(Some(ChipIdentity::Rdid { id: raw }));
    }
    if master.max_read_len() < 4 {
        return Ok(None);
    }
    let mut response = [0; 4];
    master
        .execute(&mut SpiCommand::read_reg(0x9f, &mut response))
        .await?;
    if response[..3] != [manufacturer, (device >> 8) as u8, device as u8] {
        return Err(Error::SpiTransferFailed);
    }
    Ok(Some(ChipIdentity::Rdid {
        id: u32::from_be_bytes(response),
    }))
}

const LEGACY_PROBES: &[(ProbeMethod, u8, usize, usize)] = &[
    (ProbeMethod::Rems, 0x90, 3, 2),
    (ProbeMethod::At25f, 0x15, 0, 2),
    (ProbeMethod::Res3, 0xab, 3, 3),
    (ProbeMethod::Res2, 0xab, 3, 2),
    (ProbeMethod::Res1, 0xab, 3, 1),
];

#[derive(Default)]
pub(super) struct LegacyMatches<'a> {
    // Keep unmatched responses too: a known method with a different value is
    // contradictory even if no catalog entry matches that value.
    pub(super) observed: Vec<ChipIdentity>,
    matches: Vec<(ChipIdentity, &'a FlashChip)>,
}

pub(super) async fn legacy_candidates<'a, M: SpiMaster + ?Sized, P: ChipProvider + ?Sized>(
    master: &mut M,
    provider: &'a P,
    rdid_absent: bool,
) -> Result<LegacyMatches<'a>> {
    let mut matches = LegacyMatches::default();
    let mut rems_replied = false;
    for &(method, opcode, address_len, read_len) in LEGACY_PROBES {
        if method == ProbeMethod::Res1 && (!rdid_absent || rems_replied) {
            continue;
        }
        let needed = provider.supports_probe_method(method)
            || (method == ProbeMethod::Rems && provider.supports_probe_method(ProbeMethod::Res1));
        if !needed
            || !master.probe_opcode(opcode)
            || master.max_read_len() < read_len
            || master.max_write_len() < 1 + address_len
        {
            continue;
        }
        let mut response = [0; 3];
        let mut command = if address_len == 0 {
            SpiCommand::read_reg(opcode, &mut response[..read_len])
        } else {
            SpiCommand::read_3b(opcode, 0, &mut response[..read_len])
        };
        match master.execute(&mut command).await {
            Ok(()) => {}
            Err(Error::OpcodeNotSupported) => continue,
            Err(error) => return Err(error),
        }
        let data = &response[..read_len];
        if data.iter().all(|&b| b == 0) || data.iter().all(|&b| b == 0xff) {
            continue;
        }
        rems_replied |= method == ProbeMethod::Rems;
        let short = u16::from_be_bytes([response[0], response[1]]);
        let identity = match method {
            ProbeMethod::Rems => ChipIdentity::Rems { id: short },
            ProbeMethod::At25f => ChipIdentity::At25f { id: short },
            ProbeMethod::Res1 => ChipIdentity::Res1 { id: response[0] },
            ProbeMethod::Res2 => ChipIdentity::Res2 { id: short },
            ProbeMethod::Res3 if response[0] == 0x7f => ChipIdentity::Res3 {
                id: u32::from_be_bytes([0, response[0], response[1], response[2]]),
            },
            _ => continue,
        };
        log::debug!("SPI probe identity: {:?}", identity);
        matches.observed.push(identity);
        for index in 0.. {
            let Some(chip) = provider.find_nth_by_identity(&identity, index) else {
                break;
            };
            matches.matches.push((identity, chip));
        }
    }
    Ok(matches)
}

/// Start from every positively matched chip. A missing method is unknown,
/// not negative evidence; only a documented different value excludes a chip.
/// Leave name selection and common-operation policy to select_candidates.
/// The returned identity is for diagnostics, not necessarily the chosen chip.
pub(super) fn consistent_matches<'a>(
    matches: &LegacyMatches<'a>,
) -> Result<(Vec<&'a FlashChip>, Option<ChipIdentity>)> {
    let Some(&(first_id, _)) = matches.matches.first() else {
        return Ok((Vec::new(), None));
    };
    let mut candidates: Vec<&FlashChip> = Vec::new();
    for &(_, chip) in &matches.matches {
        if !candidates.iter().any(|&other| core::ptr::eq(chip, other)) {
            candidates.push(chip);
        }
    }
    candidates.retain(|chip| {
        matches.observed.iter().all(|identity| {
            chip.matches_identity(identity)
                || !chip.identities().any(|id| id.method() == identity.method())
        })
    });
    if candidates.is_empty() {
        return Err(Error::ChipAmbiguous);
    }
    Ok((candidates, Some(first_id)))
}

#[cfg(test)]
#[path = "identification_tests.rs"]
mod tests;

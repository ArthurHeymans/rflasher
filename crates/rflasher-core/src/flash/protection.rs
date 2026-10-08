//! Temporary chip-specific unprotection, independent of WP range decoding.
//!
//! Each primitive mutation restores its protection on both success and error.
//! This is not cancellation-safe: callers must drive the future to completion.

use super::FlashContext;
use crate::chip::{Features, Unlock};
use crate::error::{EraseFailure, Error, Result};
use crate::programmer::SpiMaster;
use crate::protocol;
use crate::spi::{AddressWidth, IoMode, SpiCommand};

// Keep the bounded sector snapshot inline: core also supports no-alloc targets.
#[allow(clippy::large_enum_variant)]
pub(crate) enum SavedProtection {
    Unchanged,
    Status {
        sr1: u8,
        sr2: Option<u8>,
    },
    // count == 0 denotes global protection; mixed protection uses the bitset.
    At2x {
        sr1: u8,
        sectors: [u8; 256],
        count: u32,
    },
    Sst26 {
        bytes: [u8; 18],
        len: usize,
    },
}

async fn write_status<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    sr1: u8,
    sr2: Option<u8>,
) -> Result<()> {
    let features = ctx.chip.features;
    if features.intersects(Features::WRSR_WREN | Features::WRSR_VOLATILE_WREN) {
        protocol::write_enable(master).await?;
    } else if features.intersects(Features::WRSR_EWSR | Features::WRSR_PERSISTENT_EWSR) {
        protocol::write_enable_ewsr(master).await?;
    } else {
        return Err(Error::ChipNotSupported);
    }
    let data = [sr1 & !3, sr2.unwrap_or(0)];
    master
        .execute(&mut SpiCommand::write_reg(
            0x01,
            &data[..if sr2.is_some() { 2 } else { 1 }],
        ))
        .await?;
    // Some parts do not assert WIP immediately after WRSR (flashprog uses 100ms).
    master.delay_us(100_000).await;
    protocol::wait_ready(master, 10_000, 5_000_000).await
}

async fn sector_protection<M: SpiMaster + ?Sized>(
    master: &mut M,
    opcode: u8,
    addr: u32,
    read: &mut [u8],
) -> Result<()> {
    master
        .execute(&mut SpiCommand {
            opcode,
            address: Some(addr),
            address_width: AddressWidth::ThreeByte,
            io_mode: IoMode::Single,
            dummy_cycles: 0,
            write_data: &[],
            read_buf: read,
        })
        .await
}

pub(crate) async fn prepare<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
) -> Result<SavedProtection> {
    let unlock = ctx.chip.unlock;
    let bpr_len = match unlock {
        Unlock::Unknown | Unlock::None => return Ok(SavedProtection::Unchanged),
        Unlock::Unsupported => return Err(Error::ChipNotSupported),
        Unlock::Sst26_6 => 6,
        Unlock::Sst26_10 => 10,
        Unlock::Sst26_18 => 18,
        _ => 0,
    };
    let saved = if bpr_len != 0 {
        if master.max_read_len() < bpr_len || master.max_write_len() < bpr_len {
            return Err(Error::ChipNotSupported);
        }
        if [0x06, 0x72, 0x98, 0x42]
            .iter()
            .any(|op| !master.probe_opcode(*op))
        {
            return Err(Error::OpcodeNotSupported);
        }
        let mut bytes = [0; 18];
        master
            .execute(&mut SpiCommand::read_reg(0x72, &mut bytes[..bpr_len]))
            .await?;
        // The first two bytes contain alternating read/write locks for the
        // eight parameter blocks. ULBPR clears write locks only.
        if bytes[..2].iter().any(|b| b & 0xaa != 0) {
            return Err(Error::WriteProtected);
        }
        if bytes[..bpr_len].iter().all(|b| *b == 0) {
            return Ok(SavedProtection::Unchanged);
        }
        SavedProtection::Sst26 {
            bytes,
            len: bpr_len,
        }
    } else {
        let (bp, lock, wp, _) = unlock.status_masks().ok_or(Error::ChipNotSupported)?;
        let sr1 = protocol::read_status1(master).await?;
        let features = ctx.chip.features;
        if features.contains(Features::WP_CMP)
            && !features.intersects(Features::WP_CMP_SR2 | Features::WP_WINBOND)
        {
            // CMP availability alone does not identify its register/bit.
            return Err(Error::ChipNotSupported);
        }
        let sr2 = if features
            .intersects(Features::WP_CMP_SR2 | Features::WP_WINBOND | Features::WRSR_EXT)
        {
            if !master.probe_opcode(0x35) {
                return Err(Error::OpcodeNotSupported);
            }
            let value = protocol::read_status2(master).await?;
            // Check even with BP=0: CMP can then protect the entire array.
            // Never clear CMP/SRL, or reinterpret an unverified CMP layout.
            if value & 0x41 != 0 {
                return Err(Error::WriteProtected);
            }
            Some(value)
        } else {
            None
        };
        if sr1 & bp == 0 {
            return Ok(SavedProtection::Unchanged);
        }
        if !ctx.chip.features.intersects(
            Features::WRSR_WREN
                | Features::WRSR_VOLATILE_WREN
                | Features::WRSR_EWSR
                | Features::WRSR_PERSISTENT_EWSR,
        ) {
            return Err(Error::ChipNotSupported);
        }
        let enable = if ctx
            .chip
            .features
            .intersects(Features::WRSR_WREN | Features::WRSR_VOLATILE_WREN)
        {
            0x06
        } else {
            0x50
        };
        if master.max_write_len() == 0 || !master.probe_opcode(enable) || !master.probe_opcode(0x01)
        {
            return Err(Error::OpcodeNotSupported);
        }
        if sr1 & lock != 0 && wp != 0 && sr1 & wp == 0 {
            return Err(Error::WriteProtected);
        }
        if unlock == Unlock::At2x {
            // SWP is only a summary, not a restorable BP field. Snapshot the
            // sector registers, sampling every 4KiB (also safe for 64KiB sectors).
            let count = if sr1 & bp == bp {
                0
            } else {
                ctx.chip.total_size.div_ceil(4096)
            };
            if count > 2048 {
                return Err(Error::ChipNotSupported);
            }
            if count != 0
                && [0x3c, 0x36, 0x39, 0x06]
                    .iter()
                    .any(|op| !master.probe_opcode(*op))
            {
                return Err(Error::OpcodeNotSupported);
            }
            let mut sectors = [0; 256];
            for i in 0..count {
                let mut value = [0];
                sector_protection(master, 0x3c, i * 4096, &mut value).await?;
                match value[0] {
                    0 => {}
                    0xff => sectors[i as usize / 8] |= 1 << (i % 8),
                    _ => return Err(Error::WriteProtected),
                }
            }
            SavedProtection::At2x {
                sr1,
                sectors,
                count,
            }
        } else {
            let sr2 = if ctx.chip.features.contains(Features::WRSR_EXT) {
                if master.max_write_len() < 2 {
                    return Err(Error::ChipNotSupported);
                }
                sr2
            } else {
                None
            };
            SavedProtection::Status { sr1, sr2 }
        }
    };
    let result = async {
        match &saved {
            SavedProtection::Sst26 { len, .. } => {
                protocol::sst26_global_unprotect(master).await?;
                let mut bytes = [0; 18];
                master
                    .execute(&mut SpiCommand::read_reg(0x72, &mut bytes[..*len]))
                    .await?;
                if bytes[..*len].iter().any(|b| *b != 0) {
                    return Err(Error::WriteProtected);
                }
            }
            SavedProtection::Status { sr1, .. } | SavedProtection::At2x { sr1, .. } => {
                let sr2 = match &saved {
                    SavedProtection::Status { sr2, .. } => *sr2,
                    _ => None,
                };
                let (bp, lock, _, preserve) = unlock.status_masks().unwrap();
                if sr1 & lock != 0 {
                    // Atmel: clearing SPRL must not also change sector protection.
                    let value = if unlock == Unlock::At2x {
                        0x04
                    } else {
                        sr1 & !lock
                    };
                    write_status(master, ctx, value, sr2).await?;
                    if protocol::read_status1(master).await? & lock != 0 {
                        return Err(Error::WriteProtected);
                    }
                }
                write_status(master, ctx, (sr1 & !(bp | lock)) & preserve, sr2).await?;
                if protocol::read_status1(master).await? & bp != 0 {
                    return Err(Error::WriteProtected);
                }
            }
            SavedProtection::Unchanged => {}
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        log::error!(
            "Protection preparation failed before array mutation: {error}; attempting restoration"
        );
        return finish(
            master,
            ctx,
            saved,
            Err(error),
            (protocol::WRSR_POLL_US, protocol::WRSR_TIMEOUT_US),
        )
        .await
        .map(|()| SavedProtection::Unchanged);
    }
    Ok(saved)
}

async fn restore<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    saved: SavedProtection,
    ready_timing: (u32, u32),
) -> Result<()> {
    if matches!(saved, SavedProtection::Unchanged) {
        return Ok(());
    }
    // A failed transfer/poll can leave an accepted mutation running. Wait with
    // that operation's budget, not the much shorter status-write timeout.
    protocol::wait_ready(master, ready_timing.0, ready_timing.1).await?;
    match saved {
        SavedProtection::Unchanged => Ok(()),
        SavedProtection::Status { sr1, sr2 } => {
            let (_, _, wp, _) = ctx.chip.unlock.status_masks().unwrap();
            let mask = !(3 | wp | ctx.chip.unlock.error_mask());
            if protocol::read_status1(master).await? & mask == sr1 & mask
                && (sr2.is_none() || Some(protocol::read_status2(master).await?) == sr2)
            {
                return Ok(());
            }
            write_status(master, ctx, sr1, sr2).await?;
            if protocol::read_status1(master).await? & mask != sr1 & mask {
                return Err(Error::WriteProtected);
            }
            if let Some(expected) = sr2
                && protocol::read_status2(master).await? != expected
            {
                return Err(Error::WriteProtected);
            }
            Ok(())
        }
        SavedProtection::At2x {
            sr1,
            sectors,
            count,
        } => {
            if count == 0 {
                if protocol::read_status1(master).await? & 0x8c == sr1 & 0x8c {
                    return Ok(());
                }
                write_status(master, ctx, 0x04, None).await?;
                if protocol::read_status1(master).await? & 0x80 != 0 {
                    return Err(Error::WriteProtected);
                }
                write_status(master, ctx, (sr1 & 0x80) | 0x3c, None).await?;
                if protocol::read_status1(master).await? & 0x8c != sr1 & 0x8c {
                    return Err(Error::WriteProtected);
                }
                return Ok(());
            }
            // Clear SPRL without modifying sectors, including after a failed unlock.
            write_status(master, ctx, 0x04, None).await?;
            if protocol::read_status1(master).await? & 0x80 != 0 {
                return Err(Error::WriteProtected);
            }
            for i in 0..count {
                let protected = sectors[i as usize / 8] & (1 << (i % 8)) != 0;
                let mut value = [0];
                sector_protection(master, 0x3c, i * 4096, &mut value).await?;
                if (value[0] == 0xff) != protected {
                    protocol::write_enable(master).await?;
                    sector_protection(
                        master,
                        if protected { 0x36 } else { 0x39 },
                        i * 4096,
                        &mut [],
                    )
                    .await?;
                    protocol::wait_ready(master, 10_000, 5_000_000).await?;
                    sector_protection(master, 0x3c, i * 4096, &mut value).await?;
                    if value[0] != if protected { 0xff } else { 0 } {
                        return Err(Error::WriteProtected);
                    }
                }
            }
            // Mixed bits[5:2] mean "leave sectors alone"; restore only SPRL.
            write_status(master, ctx, (sr1 & 0x80) | 0x04, None).await?;
            if protocol::read_status1(master).await? & 0x8c != sr1 & 0x8c {
                return Err(Error::WriteProtected);
            }
            Ok(())
        }
        SavedProtection::Sst26 { bytes, len } => {
            protocol::write_enable(master).await?;
            master
                .execute(&mut SpiCommand::write_reg(0x42, &bytes[..len]))
                .await?;
            protocol::wait_ready(master, 10_000, 5_000_000).await?;
            let mut actual = [0; 18];
            master
                .execute(&mut SpiCommand::read_reg(0x72, &mut actual[..len]))
                .await?;
            if actual[..len] != bytes[..len] {
                return Err(Error::WriteProtected);
            }
            Ok(())
        }
    }
}

pub(crate) async fn finish<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    saved: SavedProtection,
    result: Result<()>,
    ready_timing: (u32, u32),
) -> Result<()> {
    if let Err(error) = restore(master, ctx, saved, ready_timing).await {
        log::error!("Protection restoration failed: {error}; operation result: {result:?}");
        return Err(Error::ProtectionRestoreFailed);
    }
    result
}

pub(crate) async fn check_result<M: SpiMaster + ?Sized>(
    master: &mut M,
    ctx: &FlashContext,
    addr: u32,
    erase: bool,
) -> Result<()> {
    let mask = ctx.chip.unlock.error_mask();
    if mask != 0 && protocol::read_status1(master).await? & mask != 0 {
        return Err(if erase {
            Error::EraseError(EraseFailure::CommandFailed { addr })
        } else {
            Error::WriteError { addr }
        });
    }
    Ok(())
}

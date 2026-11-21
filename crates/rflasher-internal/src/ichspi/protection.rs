//! Flash region permissions (FRAP/FREG), protected ranges (PR0..PR4), and BBAR.

use super::{Footprint, IchSpiController, Restriction};
use crate::error::InternalError;
use crate::host::{HostAccess, MmioAccess};
use crate::ich_regs::*;
use rflasher_core::programmer::FlashAccess;

use super::{MutationIntent, SpiMode};
use crate::chipset::IchChipset;

impl<H: HostAccess> IchSpiController<H> {
    /// Log access permissions from FRAP/FREG
    pub(super) fn handle_access_permissions(&mut self) -> Result<(), InternalError> {
        let frap = self.spibar.read32(ICH9_REG_FRAP);
        log::debug!("FRAP: {:#010x}", frap);
        log::debug!(
            "BRWA: {:#04x}, BRRA: {:#04x}",
            (frap >> 8) & 0xff,
            frap & 0xff
        );
        if self.generation.has_new_access_perm() {
            log::debug!("BIOS_BM_WAP: {:#010x}", self.spibar.read32(BIOS_BM_WAP));
            log::debug!("BIOS_BM_RAP: {:#010x}", self.spibar.read32(BIOS_BM_RAP));
        }

        let mut restricted = false;
        for (i, base, limit, can_read, can_write) in self.region_permissions() {
            let prot = AccessProtection::from_permissions(can_read, can_write);

            if prot != AccessProtection::None {
                restricted = true;
                log::info!(
                    "FREG{}: Region {:#010x}-{:#010x} is {:?}",
                    i,
                    base,
                    limit,
                    prot
                );
            } else {
                log::debug!(
                    "FREG{}: Region {:#010x}-{:#010x} is read-write",
                    i,
                    base,
                    limit
                );
            }
        }

        if restricted {
            log::warn!(
                "Not all flash regions are freely accessible. \
                       This is most likely due to an active ME."
            );
        }

        Ok(())
    }

    /// Log the protected ranges, and which of them are cleared before the
    /// first mutation.
    pub(super) fn log_protected_ranges(&self) {
        let name = if self.generation == IchChipset::Ich7 {
            "PBR"
        } else {
            "PR"
        };
        for (r, clearable) in self.protected_ranges() {
            let what = match (r.read, r.write) {
                (true, true) => "read/write",
                (true, false) => "read",
                _ => "write",
            };
            if clearable {
                log::info!(
                    "{name}{}: {:#010x}-{:#010x} is {what} protected; it is cleared \
                     before the first write or erase (not locked down)",
                    r.index,
                    r.base,
                    r.limit
                );
            } else {
                log::warn!(
                    "{name}{}: {:#010x}-{:#010x} is {what} protected",
                    r.index,
                    r.base,
                    r.limit
                );
            }
        }
    }

    /// Enabled descriptor regions as `(index, base, limit, can_read,
    /// can_write)` for the BIOS master, from FREG and FRAP (BIOS_BM_RAP/WAP
    /// where present). These bits grant access. Regions without permission
    /// bits (FRAP covers regions 0-7) are reported accessible, as flashprog
    /// does; the controller still enforces whatever applies to them.
    pub(super) fn region_permissions(
        &self,
    ) -> impl Iterator<Item = (usize, u32, u32, bool, bool)> + '_ {
        let (wap, rap, max_regions) = if self.generation.has_new_access_perm() {
            (
                self.spibar.read32(BIOS_BM_WAP),
                self.spibar.read32(BIOS_BM_RAP),
                32,
            )
        } else {
            let frap = self.spibar.read32(ICH9_REG_FRAP);
            ((frap >> 8) & 0xff, frap & 0xff, 8)
        };
        let num_freg = match self.generation {
            IchChipset::Series100SunrisePoint => 10,
            IchChipset::C620Lewisburg => 12,
            _ if self.generation.is_pch100_compatible() => 16,
            _ => 5,
        };
        (0..num_freg).filter_map(move |i| {
            let offset = if i < 12 {
                ICH9_REG_FREG0 + i * 4
            } else {
                APL_REG_FREG12 + (i - 12) * 4
            };
            let freg = self.spibar.read32(offset);
            let (base, limit) = (freg_base(freg), freg_limit(freg));
            // Skip disabled regions
            if base > limit || (freg == 0 && i > 0) {
                return None;
            }
            let granted = |bits: u32| i >= max_regions || (bits >> i) & 1 != 0;
            Some((i, base, limit, granted(rap), granted(wap)))
        })
    }

    /// First protected range register and how many there are: PRx
    /// (ICH8-ICH10, 5 series and later until PCH100), FPRx and GPR0
    /// (PCH100+), PBRx (ICH7).
    pub(super) fn pr_registers(&self) -> (usize, usize) {
        if self.generation == IchChipset::Ich7 {
            (ICH7_REG_PBR0, 3)
        } else if self.generation.is_pch100_compatible() {
            (PCH100_REG_FPR0, 6)
        } else {
            (ICH9_REG_PR0, 5)
        }
    }

    /// Whether protected range `index` is cleared before the first mutation
    /// (flashprog clears it at open): at open, the configuration was not
    /// locked down and, on PCH100+, neither was the range itself
    /// (DLOCK.PRx_LOCKDN). ICH7 PBRs are never cleared. Decided at open, so
    /// the access checks and [`Self::clear_protected_ranges`] agree; a lock
    /// set since makes the latter refuse the mutation.
    pub(super) fn pr_clearable(&self, index: usize) -> bool {
        self.generation != IchChipset::Ich7
            && !self.locked
            && self.dlock & (DLOCK_PR0_LOCKDN << index) == 0
    }

    /// Whether protected range `index` is locked now: HSFS.FLOCKDN or, on
    /// PCH100+, DLOCK.PRx_LOCKDN.
    pub(super) fn pr_locked_now(&self, index: usize) -> bool {
        self.spibar.read16(ICH9_REG_HSFS) & HSFS_FLOCKDN != 0
            || (self.generation.is_pch100_compatible()
                && self.spibar.read32(PCH100_REG_DLOCK) & (DLOCK_PR0_LOCKDN << index) != 0)
    }

    /// Protected range registers that currently restrict access, and
    /// whether each is cleared before the first mutation
    /// ([`Self::pr_clearable`]).
    pub(super) fn protected_ranges(&self) -> impl Iterator<Item = (Restriction, bool)> + '_ {
        (0..self.pr_registers().1).filter_map(|index| self.protected_range(index))
    }

    /// Protected range `index`, if it restricts access. Its bits enable
    /// protection; PBRs only write protect.
    pub(super) fn protected_range(&self, index: usize) -> Option<(Restriction, bool)> {
        let pr = self.spibar.read32(self.pr_registers().0 + index * 4);
        let (base, limit, read) = if self.generation == IchChipset::Ich7 {
            (
                (pr & 0xfff) << 12,
                ((pr >> 12) & 0xfff) << 12 | 0xfff,
                false,
            )
        } else {
            (freg_base(pr), freg_limit(pr), pr & (1 << PR_RP_OFF) != 0)
        };
        let write = pr & (1 << PR_WP_OFF) != 0;
        let restriction = Restriction {
            source: "protected range",
            index,
            base,
            limit,
            read,
            write,
        };
        ((read || write) && base <= limit).then(|| (restriction, self.pr_clearable(index)))
    }

    /// Clear every protected range the access checks assume cleared before a
    /// mutation ([`Self::pr_clearable`]), whether the mutation overlaps it or
    /// not, so none of them can refuse a later part of the operation. Each
    /// change is journaled and restored on drop. All or nothing: if one was
    /// locked since open or does not read back cleared, those cleared here
    /// are restored and the mutation is refused, before the flash is changed.
    pub(super) fn clear_protected_ranges(&mut self) -> Result<(), InternalError> {
        let (reg0, count) = self.pr_registers();
        let clearable = |c: &Self, index| {
            c.protected_range(index)
                .and_then(|(r, clearable)| clearable.then_some(r))
        };
        if !(0..count).any(|index| clearable(self, index).is_some()) {
            return Ok(());
        }
        // Do not alter controller configuration while a cycle is active.
        self.wait_configuration_idle()?;
        let journal = self.change_count;
        for index in 0..count {
            let Some(r) = clearable(self, index) else {
                continue;
            };
            // Checked per range: a lock may be set while we go.
            let locked = self.pr_locked_now(index);
            if !locked {
                self.set_pr(reg0, index, false, false);
            }
            let pr = self.spibar.read32(reg0 + index * 4);
            if locked || pr & (1 << PR_RP_OFF | 1 << PR_WP_OFF) != 0 {
                log::error!(
                    "PR{}: {:#010x}-{:#010x} {} ({pr:#010x}); refusing to modify the flash",
                    r.index,
                    r.base,
                    r.limit,
                    if locked {
                        "was locked since open and cannot be cleared"
                    } else {
                        "could not be cleared"
                    }
                );
                self.roll_back(journal);
                return Err(InternalError::AccessDenied { region: r.source });
            }
            log::info!(
                "PR{}: {:#010x}-{:#010x} protection cleared until close",
                r.index,
                r.base,
                r.limit
            );
        }
        Ok(())
    }

    /// Every flash range the controller restricts for us now: protected
    /// ranges, descriptor regions without BIOS master access (unless the
    /// descriptor override strap is set) and, in software sequencing,
    /// addresses below BBAR.
    pub(super) fn restrictions(&self) -> impl Iterator<Item = Restriction> + '_ {
        // FDOPSS reads 0 while the override strap is set.
        let regions_enforced = self.generation != IchChipset::Ich7
            && self.desc_valid
            && self.spibar.read16(ICH9_REG_HSFS) & HSFS_FDOPSS != 0;
        let regions = regions_enforced
            .then(|| self.region_permissions())
            .into_iter()
            .flatten()
            .filter(|&(_, _, _, read, write)| !read || !write)
            .map(|(index, base, limit, read, write)| Restriction {
                source: "descriptor region",
                index,
                base,
                limit,
                read: !read,
                write: !write,
            });
        let bbar = self.bbar & BBAR_MASK;
        let below_bbar =
            (self.mode != SpiMode::HardwareSequencing && bbar != 0).then(|| Restriction {
                source: "below BBAR",
                index: 0,
                base: 0,
                limit: bbar - 1,
                read: true,
                write: true,
            });
        // Write protection of a clearable range is lifted before the first
        // mutation. Its read protection is not: reads come first.
        let ranges = self.protected_ranges().map(|(mut r, clearable)| {
            r.write &= !clearable;
            r
        });
        ranges.chain(regions).chain(below_bbar)
    }

    /// Refuse `access` to `len` bytes from `addr` if the controller restricts
    /// any of them. This mirrors the controller's own per-cycle checks so a
    /// refusal can come before the first command of an operation.
    pub(super) fn check_range(
        &self,
        access: FlashAccess,
        addr: u32,
        len: u64,
    ) -> Result<(), InternalError> {
        if len == 0 {
            return Ok(());
        }
        let last = u64::from(addr) + len - 1;
        let Some(r) = self.restrictions().find(|r| {
            r.blocks(access) && u64::from(r.base) <= last && u64::from(addr) <= u64::from(r.limit)
        }) else {
            return Ok(());
        };
        log::error!(
            "Refusing to {} flash {:#x}-{:#x}: it overlaps {} {} at {:#010x}-{:#010x}; \
             select only accessible regions (e.g. a layout with just the BIOS region)",
            match access {
                FlashAccess::Read => "read",
                FlashAccess::Modify => "modify",
            },
            addr,
            last,
            r.source,
            r.index,
            r.base,
            r.limit
        );
        Err(InternalError::AccessDenied { region: r.source })
    }

    pub(super) fn check_footprint(&self, footprint: Footprint) -> Result<(), InternalError> {
        match footprint {
            Footprint::Nothing => Ok(()),
            Footprint::Range { addr, len } => {
                self.check_range(FlashAccess::Modify, addr, len.into())
            }
            // The chip size is unknown here; any write restriction may be hit.
            Footprint::Everything => self.check_range(FlashAccess::Modify, 0, 1 << 32),
        }
    }

    /// What a software-sequencing command may change in the array.
    pub(super) fn command_footprint(&self, opcode: u8, addr: u32, data_len: usize) -> Footprint {
        match (opcode, self.opcode_intent(opcode)) {
            (_, Some(MutationIntent::ReadOnly)) => Footprint::Nothing,
            (JEDEC_WRSR | 0x31 | 0x11, _) => Footprint::Nothing,
            (JEDEC_BYTE_PROGRAM, _) => Footprint::Range {
                addr,
                len: data_len.max(1) as u32,
            },
            (JEDEC_SE | JEDEC_BE_52 | JEDEC_BE_D8, _) => Footprint::Range { addr, len: 1 },
            // Chip erase and unclassified commands.
            _ => Footprint::Everything,
        }
    }

    /// Set protection range register
    pub(super) fn set_pr(
        &mut self,
        reg_pr0: usize,
        index: usize,
        read_prot: bool,
        write_prot: bool,
    ) {
        let addr = reg_pr0 + index * 4;
        let mut pr = self.spibar.read32(addr);

        pr &= !((1 << PR_RP_OFF) | (1 << PR_WP_OFF));
        if read_prot {
            pr |= 1 << PR_RP_OFF;
        }
        if write_prot {
            pr |= 1 << PR_WP_OFF;
        }

        // Treat the range and enable bits as one unit: a changed range must
        // never receive protection bits belonging to the old range.
        self.change_register(addr, false, pr, u32::MAX);
    }

    /// Set BBAR (BIOS Base Address Register)
    pub(super) fn set_bbar(&mut self, min_addr: u32) {
        let bbar_off = if self.generation >= IchChipset::Ich8 {
            ICH9_REG_BBAR
        } else {
            0x50
        };

        let mut bbar = self.spibar.read32(bbar_off) & !BBAR_MASK;
        bbar |= min_addr & BBAR_MASK;
        self.change_register(bbar_off, false, bbar, BBAR_MASK);

        self.bbar = self.spibar.read32(bbar_off) & BBAR_MASK;
        if self.bbar != (min_addr & BBAR_MASK) {
            log::warn!(
                "Setting BBAR to {:#010x} failed! New value: {:#010x}",
                min_addr,
                self.bbar
            );
        }
    }
}

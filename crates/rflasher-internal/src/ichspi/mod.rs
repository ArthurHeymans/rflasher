//! Intel ICH/PCH SPI Controller Driver
//!
//! This module implements the SPI controller driver for Intel ICH/PCH chipsets.
//! It supports both hardware sequencing (hwseq) and software sequencing (swseq)
//! modes.
//!
//! # Supported Chipsets
//!
//! - ICH7: Original SPI controller (swseq only)
//! - ICH8: ICH9-like software sequencing; hardware sequencing is unsupported
//! - ICH9-ICH10: Hardware sequencing introduced
//! - 5-9 Series (Ibex Peak through Wildcat Point)
//! - 100+ Series (Sunrise Point and later): New register layout
//!
//! # Operating Modes
//!
//! - **Hardware Sequencing**: The SPI controller handles read/write/erase
//!   operations internally. This is the default for PCH100+.
//! - **Software Sequencing**: We control the SPI protocol directly.
//!   More flexible but may not be available on locked-down systems.

use crate::DetectedChipset;
use crate::chipset::IchChipset;
use crate::controller::Controller;
use crate::error::InternalError;
#[cfg(all(feature = "std", target_os = "linux"))]
use crate::host::LinuxHost;
use crate::host::{HostAccess, MmioAccess, PciAddress};
use crate::ich_regs::*;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::programmer::{FlashAccess, SpiFeatures};
use rflasher_core::spi::SpiCommand;

mod hwseq;
mod opcodes;
mod protection;
mod spibar;
mod swseq;
#[cfg(test)]
mod tests;

pub use spibar::get_spibar_address_with_host;
#[cfg(any(test, all(feature = "std", target_os = "linux")))]
use spibar::intel_config_function;
use spibar::uses_silvermont_sbase;
pub(crate) use spibar::{intel_config_bdf, is_visible_spi_id};

#[cfg(all(feature = "std", not(target_os = "uefi")))]
struct Deadline {
    end: std::time::Instant,
}

#[cfg(all(feature = "std", not(target_os = "uefi")))]
impl Deadline {
    fn new(timeout_us: u32) -> Self {
        Self {
            end: std::time::Instant::now()
                + std::time::Duration::from_micros(u64::from(timeout_us)),
        }
    }

    fn expired(&self) -> bool {
        std::time::Instant::now() >= self.end
    }

    fn wait_step<H: HostAccess>(&mut self, _host: &H) {
        core::hint::spin_loop();
    }
}

#[cfg(any(not(feature = "std"), target_os = "uefi"))]
struct Deadline {
    remaining_us: u32,
}

#[cfg(any(not(feature = "std"), target_os = "uefi"))]
impl Deadline {
    fn new(timeout_us: u32) -> Self {
        Self {
            remaining_us: timeout_us,
        }
    }

    fn expired(&self) -> bool {
        self.remaining_us == 0
    }

    fn wait_step<H: HostAccess>(&mut self, host: &H) {
        if self.remaining_us > 0 {
            host.delay_us(1);
            self.remaining_us -= 1;
        }
    }
}

/// Maximum SPI data transfer size for hardware sequencing
pub const HWSEQ_MAX_DATA: usize = 64;

/// SPI controller operating mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpiMode {
    /// Automatic mode selection
    #[default]
    Auto,
    /// Force hardware sequencing
    HardwareSequencing,
    /// Force software sequencing
    SoftwareSequencing,
}

impl SpiMode {
    /// Parse from string (for CLI parameter)
    pub fn parse(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("auto") {
            Some(Self::Auto)
        } else if s.eq_ignore_ascii_case("hwseq")
            || s.eq_ignore_ascii_case("hardware")
            || s.eq_ignore_ascii_case("hw")
        {
            Some(Self::HardwareSequencing)
        } else if s.eq_ignore_ascii_case("swseq")
            || s.eq_ignore_ascii_case("software")
            || s.eq_ignore_ascii_case("sw")
        {
            Some(Self::SoftwareSequencing)
        } else {
            None
        }
    }
}

impl core::fmt::Display for SpiMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::HardwareSequencing => write!(f, "hwseq"),
            Self::SoftwareSequencing => write!(f, "swseq"),
        }
    }
}

const SILVERMONT_REG_BCR: usize = 0xfc;

/// Software sequencing register offsets (varies by chipset generation)
#[derive(Debug, Clone, Copy)]
struct SwseqRegs {
    ssfsc: usize,
    preop: usize,
    optype: usize,
    opmenu: usize,
}

/// ICH7 software sequencing register offsets
#[derive(Debug, Clone, Copy)]
struct Ich7SwseqRegs {
    spis: usize,
    spic: usize,
    spia: usize,
    spid0: usize,
    preop: usize,
    optype: usize,
    opmenu: usize,
}

/// Hardware sequencing data
#[derive(Debug, Clone, Copy)]
struct HwseqData {
    /// Address mask for FADDR register
    addr_mask: u32,
    /// Whether only 4KB erase is supported
    only_4k: bool,
    /// HSFC FCYCLE field mask (differs between ICH9 and PCH100+)
    hsfc_fcycle: u16,
    /// Size of flash component 0
    #[allow(dead_code)]
    size_comp0: u32,
    /// Size of flash component 1
    #[allow(dead_code)]
    size_comp1: u32,
}

/// Erase size of every PCH100+ hardware-sequencing cycle
const HWSEQ_ONLY_4K_ERASE_SIZE: u32 = 4 * 1024;

/// Erase block sizes selectable through HSFS.BERASE (bits 4:3)
const HWSEQ_BERASE_SIZES: [u32; 4] = [256, 4 * 1024, 8 * 1024, 64 * 1024];

/// Decode the erase block size from an HSFS value
fn berase_block_size(hsfs: u16) -> u32 {
    HWSEQ_BERASE_SIZES[usize::from((hsfs & HSFS_BERASE) >> HSFS_BERASE_OFF)]
}

/// The single erase block size to plan erases with, given the sizes the
/// controller uses below (`low`) and from (`high`) the flash partition
/// `boundary` (0 = one partition).
///
/// With two different sizes the larger one is used, since erasing a multiple
/// of the smaller size is always possible, but only if the partitions meet on
/// a boundary of it. Otherwise no uniform size exists and `None` is returned.
fn uniform_erase_size(low: u32, high: u32, boundary: u32) -> Option<u32> {
    if low == high {
        return Some(low);
    }
    let larger = low.max(high);
    boundary.is_multiple_of(larger).then_some(larger)
}

/// Opcode entry for software sequencing
#[derive(Debug, Clone, Copy, Default)]
struct Opcode {
    /// SPI opcode byte
    opcode: u8,
    /// Opcode type (read/write, with/without address)
    spi_type: u8,
    /// Atomic operation: 0 = none, 1 = preop0, 2 = preop1
    #[allow(dead_code)]
    atomic: u8,
}

/// Opcode table for software sequencing
#[derive(Debug, Clone)]
struct Opcodes {
    preop: [u8; 2],
    opcode: [Opcode; 8],
}

impl Default for Opcodes {
    fn default() -> Self {
        // Default opcode configuration (like O_ST_M25P in flashprog)
        Self {
            preop: [JEDEC_WREN, JEDEC_EWSR],
            opcode: [
                Opcode {
                    opcode: JEDEC_BYTE_PROGRAM,
                    spi_type: SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS,
                    atomic: 0,
                },
                Opcode {
                    opcode: JEDEC_READ,
                    spi_type: SPI_OPCODE_TYPE_READ_WITH_ADDRESS,
                    atomic: 0,
                },
                Opcode {
                    opcode: JEDEC_SE,
                    spi_type: SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS,
                    atomic: 0,
                },
                Opcode {
                    opcode: JEDEC_RDSR,
                    spi_type: SPI_OPCODE_TYPE_READ_NO_ADDRESS,
                    atomic: 0,
                },
                Opcode {
                    opcode: JEDEC_REMS,
                    spi_type: SPI_OPCODE_TYPE_READ_WITH_ADDRESS,
                    atomic: 0,
                },
                Opcode {
                    opcode: JEDEC_WRSR,
                    spi_type: SPI_OPCODE_TYPE_WRITE_NO_ADDRESS,
                    atomic: 0,
                },
                Opcode {
                    opcode: JEDEC_RDID,
                    spi_type: SPI_OPCODE_TYPE_READ_NO_ADDRESS,
                    atomic: 0,
                },
                Opcode {
                    opcode: JEDEC_CE_C7,
                    spi_type: SPI_OPCODE_TYPE_WRITE_NO_ADDRESS,
                    atomic: 0,
                },
            ],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MutationIntent {
    ReadOnly,
    Mutating,
}

/// Flash array bytes a software-sequencing command may change, as far as
/// the controller can tell without knowing the chip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Footprint {
    /// None (reads, status register writes).
    Nothing,
    /// `len` bytes from `addr`. For block erases this is just the address,
    /// which is all the hardware checks too: the erased block size is chip
    /// metadata, checked by the caller's preflight.
    Range { addr: u32, len: u32 },
    /// Possibly any of it (chip erase, unclassified commands).
    Everything,
}

/// A flash range the controller restricts for the BIOS master.
#[derive(Clone, Copy, Debug)]
struct Restriction {
    /// What restricts it, for diagnostics and [`InternalError::AccessDenied`].
    source: &'static str,
    /// Register or region index, for diagnostics.
    index: usize,
    base: u32,
    /// Inclusive.
    limit: u32,
    read: bool,
    write: bool,
}

impl Restriction {
    fn blocks(&self, access: FlashAccess) -> bool {
        match access {
            FlashAccess::Read => self.read,
            FlashAccess::Modify => self.write,
        }
    }
}

#[derive(Clone, Copy)]
struct RegisterChange {
    offset: usize,
    width16: bool,
    before: u32,
    after: u32,
    mask: u32,
}

/// Intel ICH/PCH SPI Controller
pub struct IchSpiController<H: HostAccess> {
    /// Platform access backend
    host: H,
    /// Memory-mapped SPI registers
    spibar: H::MmioRegion,
    /// Chipset generation
    generation: IchChipset,
    /// PCI location of BIOS_CNTL (SPI function on PCH100+).
    config_bdf: PciAddress,
    /// Whether configuration is locked (HSFS.FLOCKDN)
    locked: bool,
    /// BIOS_CNTL.BWE was observed or successfully verified during enable.
    bios_writes_enabled: bool,
    /// Bay Trail / Avoton / Braswell BCR is in the SBASE MMIO window.
    silvermont_bcr: bool,
    /// Restore Silvermont BCR.WPD if this controller enabled it.
    restore_bcr_wpd: bool,
    restore_pci_bwe: bool,
    changes: [Option<RegisterChange>; 12],
    change_count: usize,
    /// Whether software sequencing is locked (DLOCK.SSEQ_LOCKDN on PCH100+)
    swseq_locked: bool,
    /// DLOCK (PCH100+) at open.
    dlock: u32,
    /// Flash descriptor valid
    desc_valid: bool,
    /// Requested operating mode (from user)
    requested_mode: SpiMode,
    /// Actual operating mode (after validation)
    mode: SpiMode,
    /// Software sequencing registers (ICH9+)
    swseq: SwseqRegs,
    /// ICH7 software sequencing registers
    ich7_swseq: Ich7SwseqRegs,
    /// Hardware sequencing data
    hwseq: HwseqData,
    /// Current opcodes (for software sequencing)
    opcodes: Option<Opcodes>,
    /// WREN/EWSR sent on its own; the next command uses it as its atomic
    /// prefix, like flashprog's `ich_spi_send_multicommand`.
    pending_preop: Option<u8>,
    /// The identified chip reads status register 2 with 0x35, which is
    /// otherwise too ambiguous to send (see `chip_identified`).
    rdsr2: bool,
    /// BBAR value
    bbar: u32,
}

impl<H: HostAccess> Drop for IchSpiController<H> {
    fn drop(&mut self) {
        if self.restore_pci_bwe {
            let bdf = self.config_bdf;
            match self.host.read8(bdf, PCI_REG_BIOS_CNTL as u16) {
                Ok(value) => {
                    if let Err(e) =
                        self.host
                            .write8(bdf, PCI_REG_BIOS_CNTL as u16, value & !BIOS_CNTL_BWE)
                    {
                        log::warn!("Failed to restore BIOS_CNTL.BWE: {}", e);
                    }
                }
                Err(e) => log::warn!("Failed to read BIOS_CNTL for restore: {}", e),
            }
        }
        if self.restore_bcr_wpd {
            let bcr = self.spibar.read32(SILVERMONT_REG_BCR);
            self.spibar
                .write32(SILVERMONT_REG_BCR, bcr & !u32::from(BIOS_CNTL_BWE));
        }
        self.restore_registers();
    }
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl IchSpiController<LinuxHost> {
    /// Initialize a new SPI controller for the detected chipset using Linux host access.
    pub fn new(chipset: &DetectedChipset, mode: SpiMode) -> Result<Self, InternalError> {
        let config = intel_config_function(chipset)?;
        let host = if config.hidden {
            LinuxHost::new()
                .with_hidden_function(config.address)
                .map_err(|e| {
                    log::warn!(
                        "SPI PCI function {} is not enumerated by the OS: {e}",
                        config.address
                    );
                    InternalError::ChipsetEnable(
                        "hidden SPI PCI function needs raw x86 PCI config access (run as root)",
                    )
                })?
        } else {
            LinuxHost::new()
        };
        Self::new_with_host(host, chipset, mode)
    }
}

impl<H: HostAccess> IchSpiController<H> {
    /// Initialize a new SPI controller for the detected chipset.
    pub fn new_with_host(
        host: H,
        chipset: &DetectedChipset,
        mode: SpiMode,
    ) -> Result<Self, InternalError> {
        let generation = chipset.chipset_type();

        // Get SPI BAR address
        let config_bdf = intel_config_bdf(chipset)?;
        let spibar_addr = get_spibar_address_with_host(&host, chipset)?;
        log::debug!("SPI BAR at physical address: {:#x}", spibar_addr);

        // Map the SPI registers
        // SAFETY: SPIBAR was discovered from chipset PCI configuration and is
        // the controller register window for the selected chipset.
        let spibar = unsafe { host.map_mmio(spibar_addr, 0x200)? };

        // Initialize register offsets based on generation
        let (swseq, hwseq) = if generation.is_pch100_compatible() {
            (
                SwseqRegs {
                    ssfsc: PCH100_REG_SSFSC,
                    preop: PCH100_REG_PREOP,
                    optype: PCH100_REG_OPTYPE,
                    opmenu: PCH100_REG_OPMENU,
                },
                HwseqData {
                    addr_mask: PCH100_FADDR_FLA,
                    only_4k: true,
                    hsfc_fcycle: PCH100_HSFC_FCYCLE,
                    size_comp0: 0,
                    size_comp1: 0,
                },
            )
        } else {
            (
                SwseqRegs {
                    ssfsc: ICH9_REG_SSFS,
                    preop: ICH9_REG_PREOP,
                    optype: ICH9_REG_OPTYPE,
                    opmenu: ICH9_REG_OPMENU,
                },
                HwseqData {
                    addr_mask: ICH9_FADDR_FLA,
                    only_4k: false,
                    hsfc_fcycle: HSFC_FCYCLE,
                    size_comp0: 0,
                    size_comp1: 0,
                },
            )
        };

        // ICH7 software sequencing registers
        let ich7_swseq = Ich7SwseqRegs {
            spis: ICH7_REG_SPIS,
            spic: ICH7_REG_SPIC,
            spia: ICH7_REG_SPIA,
            spid0: ICH7_REG_SPID0,
            preop: ICH7_REG_PREOP,
            optype: ICH7_REG_OPTYPE,
            opmenu: ICH7_REG_OPMENU,
        };

        let mut controller = Self {
            host,
            spibar,
            generation,
            config_bdf,
            locked: false,
            bios_writes_enabled: false,
            silvermont_bcr: uses_silvermont_sbase(chipset),
            restore_bcr_wpd: false,
            restore_pci_bwe: false,
            changes: [None; 12],
            change_count: 0,
            swseq_locked: false,
            dlock: 0,
            desc_valid: false,
            requested_mode: mode,
            mode: SpiMode::Auto, // Will be determined during init
            swseq,
            ich7_swseq,
            hwseq,
            opcodes: None,
            pending_preop: None,
            rdsr2: false,
            bbar: 0,
        };

        // Do not alter controller configuration while a cycle is active.
        controller.wait_configuration_idle()?;
        // Initialize the controller. Drop also unwinds a partially failed init.
        controller.init()?;

        Ok(controller)
    }

    fn wait_configuration_idle(&self) -> Result<(), InternalError> {
        if self.generation == IchChipset::Ich7 {
            self.ich7_swseq_wait_idle(60_000)
        } else {
            self.swseq_wait_idle(60_000)?;
            let mut deadline = Deadline::new(60_000);
            loop {
                if self.spibar.read16(ICH9_REG_HSFS) & HSFS_SCIP == 0 {
                    return Ok(());
                }
                if deadline.expired() {
                    return Err(InternalError::Io("hardware sequencing busy"));
                }
                deadline.wait_step(&self.host);
            }
        }
    }

    fn change_register(&mut self, offset: usize, width16: bool, value: u32, mask: u32) {
        let before = if width16 {
            u32::from(self.spibar.read16(offset))
        } else {
            self.spibar.read32(offset)
        };
        let after = (before & !mask) | (value & mask);
        if before == after {
            return;
        }
        if self.change_count == self.changes.len() {
            log::warn!("SPI configuration journal full; not changing register {offset:#x}");
            return;
        }
        self.changes[self.change_count] = Some(RegisterChange {
            offset,
            width16,
            before,
            after,
            mask,
        });
        self.change_count += 1;
        if width16 {
            self.spibar.write16(offset, after as u16);
        } else {
            self.spibar.write32(offset, after);
        }
    }

    fn restore_registers(&mut self) {
        self.restore_registers_from(0);
    }

    /// Undo the journaled changes from `start` on at once, saying loudly if
    /// some cannot be (they stay journaled and are retried on drop).
    fn roll_back(&mut self, start: usize) {
        if !self.restore_registers_from(start) {
            log::error!(
                "Could not undo the SPI configuration changed for this operation; \
                 protected ranges may stay cleared until the controller is closed"
            );
        }
    }

    /// Undo the journaled changes from `start` on, newest first. Changes
    /// that cannot be undone now (busy or locked controller, locked
    /// register, restore not taken) stay journaled for a later attempt.
    /// Returns whether none are left.
    fn restore_registers_from(&mut self, start: usize) -> bool {
        if self.change_count <= start {
            return true;
        }
        if self.wait_configuration_idle().is_err() {
            log::warn!("SPI controller busy; cannot restore configuration");
            return false;
        }
        let locked = if self.generation == IchChipset::Ich7 {
            self.spibar.read16(ICH7_REG_SPIS) & (1 << 15) != 0
        } else {
            self.spibar.read16(ICH9_REG_HSFS) & HSFS_FLOCKDN != 0
        };
        if locked {
            log::warn!("SPI configuration locked; cannot restore configuration");
            return false;
        }
        let dlock = if self.generation.is_pch100_compatible() {
            self.spibar.read32(PCH100_REG_DLOCK)
        } else {
            0
        };
        let end = self.change_count;
        for i in (start..end).rev() {
            if let Some(change) = self.changes[i]
                && self.restore_change(&change, dlock)
            {
                self.changes[i] = None;
            }
        }
        // Keep what is left, in order.
        self.change_count = (start..end).fold(start, |kept, i| match self.changes[i].take() {
            Some(change) => {
                self.changes[kept] = Some(change);
                kept + 1
            }
            None => kept,
        });
        self.change_count == start
    }

    /// Undo one journaled change; false if it has to stay journaled. Only
    /// the bits it changed are restored, and only those still reading back
    /// as it set them: bits the hardware did not take already hold their old
    /// value. If any other bit of the register changed since, it is someone
    /// else's now and is left alone.
    fn restore_change(&self, change: &RegisterChange, dlock: u32) -> bool {
        let pr_index = change
            .offset
            .checked_sub(PCH100_REG_FPR0)
            .filter(|offset| *offset < 6 * 4 && *offset % 4 == 0)
            .map(|offset| offset / 4);
        let pr_locked = self.generation.is_pch100_compatible()
            && pr_index.is_some_and(|index| dlock & (DLOCK_PR0_LOCKDN << index) != 0);
        let menu_locked = self.generation.is_pch100_compatible()
            && change.offset >= self.swseq.preop
            && change.offset <= self.swseq.opmenu + 4
            && dlock & DLOCK_SSEQ_LOCKDN != 0;
        if pr_locked || menu_locked {
            log::warn!(
                "SPI register {:#x} locked; cannot restore it",
                change.offset
            );
            return false;
        }
        let read = || {
            if change.width16 {
                u32::from(self.spibar.read16(change.offset))
            } else {
                self.spibar.read32(change.offset)
            }
        };
        let current = read();
        let changed = (change.before ^ change.after) & change.mask;
        if (current ^ change.after) & change.mask & !changed != 0 {
            log::warn!(
                "SPI register {:#x} changed externally; not restoring it",
                change.offset
            );
            return true;
        }
        let restored = (current & !changed) | (change.before & changed);
        if restored == current {
            return true;
        }
        if change.width16 {
            self.spibar.write16(change.offset, restored as u16);
        } else {
            self.spibar.write32(change.offset, restored);
        }
        if (read() ^ restored) & change.mask != 0 {
            log::error!("SPI register {:#x} restore failed", change.offset);
            return false;
        }
        true
    }

    /// Initialize the SPI controller
    fn init(&mut self) -> Result<(), InternalError> {
        if self.generation.is_ich9_compatible() {
            self.init_ich9()
        } else if self.generation == IchChipset::Ich7 {
            self.init_ich7()
        } else {
            Err(InternalError::NotSupported(
                "Unsupported chipset generation",
            ))
        }
    }

    /// Initialize ICH7 SPI controller
    ///
    /// ICH7 only supports software sequencing (no hwseq).
    fn init_ich7(&mut self) -> Result<(), InternalError> {
        // Log register values like flashprog does
        let spis = self.spibar.read16(ICH7_REG_SPIS);
        let spic = self.spibar.read16(ICH7_REG_SPIC);
        let spia = self.spibar.read32(ICH7_REG_SPIA);

        log::debug!("0x00: {:#06x} (SPIS)", spis);
        log::debug!("0x02: {:#06x} (SPIC)", spic);
        log::debug!("0x04: {:#010x} (SPIA)", spia);

        // Read BBAR
        self.bbar = self.spibar.read32(0x50);
        log::debug!("0x50: {:#010x} (BBAR)", self.bbar);

        // Log opcode registers
        let preop = self.spibar.read16(ICH7_REG_PREOP);
        let optype = self.spibar.read16(ICH7_REG_OPTYPE);
        let opmenu_lo = self.spibar.read32(ICH7_REG_OPMENU);
        let opmenu_hi = self.spibar.read32(ICH7_REG_OPMENU + 4);

        log::debug!("0x54: {:#06x} (PREOP)", preop);
        log::debug!("0x56: {:#06x} (OPTYPE)", optype);
        log::debug!("0x58: {:#010x} (OPMENU)", opmenu_lo);
        log::debug!("0x5c: {:#010x} (OPMENU+4)", opmenu_hi);

        // Log PBR registers (Protected BIOS Range)
        for i in 0..3 {
            let offs = ICH7_REG_PBR0 + (i * 4);
            let pbr = self.spibar.read32(offs);
            log::debug!("{:#04x}: {:#010x} (PBR{})", offs, pbr, i);
        }
        self.log_protected_ranges();

        // Check for lockdown (bit 15 of SPIS)
        if spis & (1 << 15) != 0 {
            log::warn!("SPI Configuration Lockdown activated");
            self.locked = true;
        }

        // Initialize opcodes using ICH7 register offsets
        self.init_ich7_opcodes()?;

        // Try to set BBAR to 0 (allow access to all flash addresses)
        if !self.locked {
            self.set_bbar(0);
        }

        // ICH7 only supports swseq
        self.mode = SpiMode::SoftwareSequencing;
        log::info!("Using swseq mode on ICH7 (hwseq not supported)");

        Ok(())
    }

    /// Initialize ICH9+ SPI controller (including PCH100+)
    fn init_ich9(&mut self) -> Result<(), InternalError> {
        // Read HSFS
        let hsfs = self.spibar.read16(ICH9_REG_HSFS);
        log::debug!("HSFS: {:#06x}", hsfs);
        self.print_hsfs(hsfs);

        // Check for lockdown
        if hsfs & HSFS_FLOCKDN != 0 {
            log::info!("SPI Configuration is locked down");
            self.locked = true;
        }

        // Check descriptor valid
        if hsfs & HSFS_FDV != 0 {
            self.desc_valid = true;
            log::debug!("Flash Descriptor is valid");
        }

        // Check for descriptor override
        if hsfs & HSFS_FDOPSS == 0 && self.desc_valid {
            log::info!(
                "Flash Descriptor Override Strap-Pin is set. \
                       Master Section restrictions NOT in effect."
            );
        }

        // Read software-sequencing lock before touching its opcode menu.
        if self.generation.is_pch100_compatible() {
            self.swseq_locked = self.spibar.read32(PCH100_REG_DLOCK) & DLOCK_SSEQ_LOCKDN != 0;
        }
        // Initialize opcodes
        self.init_opcodes()?;

        // Read and log HSFC if descriptor valid
        if self.desc_valid {
            let hsfc = self.spibar.read16(ICH9_REG_HSFC);
            log::debug!("HSFC: {:#06x}", hsfc);
        }

        // PCH100+ specific: read DLOCK and check SSEQ_LOCKDN
        if self.generation.is_pch100_compatible() {
            let dlock = self.spibar.read32(PCH100_REG_DLOCK);
            log::debug!("DLOCK: {:#010x}", dlock);
            self.dlock = dlock;
            self.print_dlock(dlock);

            // Check if software sequencing is locked down
            if dlock & DLOCK_SSEQ_LOCKDN != 0 {
                log::info!("Software sequencing is locked (DLOCK.SSEQ_LOCKDN=1)");
                self.swseq_locked = true;
            }
        }

        // Read FRAP and handle access permissions if descriptor valid
        if self.desc_valid {
            self.handle_access_permissions()?;
        }

        self.log_protected_ranges();

        // Log SSFS/SSFC
        let ssfsc = self.spibar.read32(self.swseq.ssfsc);
        log::debug!("SSFS: {:#04x}", ssfsc & 0xFF);
        log::debug!("SSFC: {:#08x}", ssfsc >> 8);

        // Clear any pending errors
        if ssfsc & SSFS_FCERR != 0 {
            log::debug!("Clearing SSFS.FCERR");
            self.spibar.write8(self.swseq.ssfsc, SSFS_FCERR as u8);
        }

        // Handle BBAR for older chipsets
        if self.desc_valid
            && !self.generation.is_pch100_compatible()
            && self.generation != IchChipset::Ich8
            && self.generation != IchChipset::BayTrail
        {
            self.bbar = self.spibar.read32(ICH9_REG_BBAR);
            log::debug!("BBAR: {:#010x}", self.bbar);
            if !self.locked {
                self.set_bbar(0);
            }
        }

        // Determine operating mode
        self.determine_mode()?;

        Ok(())
    }

    /// Determine the operating mode
    ///
    /// This validates the requested mode against hardware capabilities:
    /// - ICH7 only supports swseq (no hwseq)
    /// - PCH100+ may have swseq locked (DLOCK.SSEQ_LOCKDN)
    /// - hwseq requires a valid flash descriptor
    fn determine_mode(&mut self) -> Result<(), InternalError> {
        let requested = self.requested_mode;

        // First, validate user's explicit request
        if requested == SpiMode::HardwareSequencing {
            // Check if hwseq is supported
            if !self.generation.supports_hwseq() {
                log::error!(
                    "Hardware sequencing requested but not supported on {} (ICH7/ICH8 only support swseq)",
                    self.generation
                );
                return Err(InternalError::NotSupported(
                    "Hardware sequencing not available on ICH7/ICH8",
                ));
            }
            if !self.desc_valid {
                log::error!("Hardware sequencing requested but flash descriptor is not valid");
                return Err(InternalError::InvalidDescriptor);
            }
        } else if requested == SpiMode::SoftwareSequencing {
            // Check if swseq is available
            if self.swseq_locked {
                log::error!(
                    "Software sequencing requested but locked on {} (DLOCK.SSEQ_LOCKDN=1)",
                    self.generation
                );
                return Err(InternalError::NotSupported(
                    "Software sequencing is locked (DLOCK.SSEQ_LOCKDN=1)",
                ));
            }
        }

        // Now determine effective mode for Auto
        let effective_mode = if requested != SpiMode::Auto {
            requested
        } else {
            // Auto mode selection logic
            if !self.generation.supports_hwseq() {
                // ICH7/ICH8: swseq only
                log::debug!("Using swseq ({} has no hwseq support)", self.generation);
                SpiMode::SoftwareSequencing
            } else if self.swseq_locked {
                // swseq locked, must use hwseq
                log::info!("Using hwseq because swseq is locked (DLOCK.SSEQ_LOCKDN=1)");
                if !self.desc_valid {
                    return Err(InternalError::InvalidDescriptor);
                }
                SpiMode::HardwareSequencing
            } else if self.locked && self.missing_opcodes() {
                // Important opcodes missing, use hwseq
                log::info!("Enabling hwseq because some important opcode is locked");
                if !self.desc_valid {
                    return Err(InternalError::InvalidDescriptor);
                }
                SpiMode::HardwareSequencing
            } else if self.generation.defaults_to_hwseq() {
                // PCH100+ defaults to hwseq
                log::debug!("Enabling hwseq by default for {} series", self.generation);
                if self.desc_valid {
                    SpiMode::HardwareSequencing
                } else {
                    // No valid descriptor, fall back to swseq if not locked
                    log::warn!(
                        "Flash descriptor not valid, falling back to swseq on {}",
                        self.generation
                    );
                    SpiMode::SoftwareSequencing
                }
            } else {
                // ICH9-style chipsets: prefer swseq
                SpiMode::SoftwareSequencing
            }
        };

        self.mode = effective_mode;
        log::info!(
            "Using {} mode on {} (requested: {})",
            self.mode,
            self.generation,
            requested
        );

        Ok(())
    }

    /// Admit the status reads the identified chip's protection needs.
    /// Enable BIOS write access via BIOS_CNTL register
    pub fn enable_bios_write(&mut self) -> Result<(), InternalError> {
        self.bios_writes_enabled = false;
        if self.silvermont_bcr {
            let bcr = self.spibar.read32(SILVERMONT_REG_BCR);
            if bcr & u32::from(BIOS_CNTL_BLE) != 0 {
                log::warn!("BIOS Lock Enable is set - writes may trigger SMI");
            }
            // Match the PCI path: try to clear SMM_BWP and verify only BWE.
            let wanted = (bcr | u32::from(BIOS_CNTL_BWE)) & !u32::from(BIOS_CNTL_SMM_BWP);
            let current = if wanted != bcr {
                if bcr & u32::from(BIOS_CNTL_BWE) == 0 {
                    // Failed verification must also clean up an attempted enable.
                    self.restore_bcr_wpd = true;
                }
                self.spibar.write32(SILVERMONT_REG_BCR, wanted);
                self.spibar.read32(SILVERMONT_REG_BCR)
            } else {
                bcr
            };
            if current & u32::from(BIOS_CNTL_SMM_BWP) != 0 {
                log::warn!("SMM BIOS Write Protect is set - BIOS region writes may be rejected");
            }
            if current & u32::from(BIOS_CNTL_BWE) == 0 {
                return Err(InternalError::ChipsetEnable("Cannot enable BIOS writes"));
            }
            self.bios_writes_enabled = true;
            return Ok(());
        }
        let lpc_bdf = self.config_bdf;
        let bios_cntl = self.host.read8(lpc_bdf, PCI_REG_BIOS_CNTL as u16)?;

        log::debug!("BIOS_CNTL: {:#04x}", bios_cntl);

        // Check if BIOS Lock Enable is set
        if bios_cntl & BIOS_CNTL_BLE != 0 {
            log::warn!("BIOS Lock Enable (BLE) is set - writes may trigger SMI");
        }

        // Like flashprog, try to clear SMM_BWP (EISS on PCH100+) while
        // setting BWE. Only BWE decides success: firmware may leave SMM_BWP
        // writable, and if it stays set the controller rejects the cycles.
        let wanted = (bios_cntl | BIOS_CNTL_BWE) & !BIOS_CNTL_SMM_BWP;
        let current = if wanted != bios_cntl {
            if bios_cntl & BIOS_CNTL_BWE == 0 {
                self.restore_pci_bwe = true;
            }
            self.host
                .write8(lpc_bdf, PCI_REG_BIOS_CNTL as u16, wanted)?;
            self.host.read8(lpc_bdf, PCI_REG_BIOS_CNTL as u16)?
        } else {
            bios_cntl
        };

        if current & BIOS_CNTL_SMM_BWP != 0 {
            log::warn!("SMM BIOS Write Protect is set - BIOS region writes may be rejected");
        }

        if current & BIOS_CNTL_BWE == 0 {
            log::error!("Failed to enable BIOS Write Enable");
            return Err(InternalError::ChipsetEnable("Cannot enable BIOS writes"));
        }

        if bios_cntl & BIOS_CNTL_BWE == 0 {
            log::info!("BIOS Write Enable activated");
        } else {
            log::debug!("BIOS Write Enable already active");
        }

        self.bios_writes_enabled = true;
        Ok(())
    }

    /// Get the current operating mode
    pub fn mode(&self) -> SpiMode {
        self.mode
    }

    /// Check if the controller is locked (HSFS.FLOCKDN)
    #[allow(dead_code)]
    fn is_locked(&self) -> bool {
        self.locked
    }

    /// Return the BIOS region from the Intel Flash Descriptor, if valid.
    ///
    /// The tuple is `(base, limit)` in flash offsets, inclusive. This is used
    /// by firmware callers to translate top-of-4GiB memory-mapped BIOS
    /// addresses back to SPI flash offsets.
    pub fn get_bios_region(&self) -> Option<(u32, u32)> {
        if !self.desc_valid || self.generation == IchChipset::Ich7 {
            return None;
        }

        let freg1 = self.spibar.read32(ICH9_REG_FREG0 + 4);
        let base = (freg1 & 0x7fff) << 12;
        let limit = (((freg1 >> 16) & 0x7fff) << 12) | 0x0fff;
        if base <= limit {
            Some((base, limit))
        } else {
            None
        }
    }

    /// Get the chipset generation
    pub fn generation(&self) -> IchChipset {
        self.generation
    }
}

impl<H: HostAccess> Controller for IchSpiController<H> {
    fn is_locked(&self) -> bool {
        self.locked
    }

    fn writes_enabled(&self) -> bool {
        self.bios_writes_enabled
    }

    fn enable_bios_write(&mut self) -> Result<(), InternalError> {
        self.enable_bios_write()
    }

    fn controller_read(&mut self, addr: u32, buf: &mut [u8], _chip_size: usize) -> CoreResult<()> {
        let result = if self.mode == SpiMode::HardwareSequencing {
            self.hwseq_read(addr, buf)
        } else if self.generation == IchChipset::Ich7 {
            self.ich7_swseq_read(addr, buf)
        } else {
            self.swseq_read(addr, buf)
        };
        result.map_err(InternalError::to_core_error)
    }

    fn controller_write(&mut self, addr: u32, data: &[u8]) -> CoreResult<()> {
        let result = if self.mode == SpiMode::HardwareSequencing {
            self.hwseq_write(addr, data)
        } else if self.generation == IchChipset::Ich7 {
            self.ich7_swseq_write(addr, data)
        } else {
            self.swseq_write(addr, data)
        };
        result.map_err(InternalError::to_core_error)
    }

    fn controller_erase(&mut self, addr: u32, len: u32) -> CoreResult<()> {
        let result = if self.mode == SpiMode::HardwareSequencing {
            self.hwseq_erase(addr, len)
        } else if self.generation == IchChipset::Ich7 {
            self.ich7_swseq_erase(addr, len)
        } else {
            self.swseq_erase(addr, len)
        };
        result.map_err(InternalError::to_core_error)
    }

    fn controller_name(&self) -> &'static str {
        "Intel ICH/PCH"
    }

    fn physical_flash_size(&self) -> Result<Option<u32>, InternalError> {
        if self.mode == SpiMode::HardwareSequencing {
            self.descriptor_flash_size()
        } else {
            Ok(None)
        }
    }

    fn erase_block_size(&self, flash_size: u32) -> Result<Option<u32>, InternalError> {
        if self.mode == SpiMode::HardwareSequencing {
            self.hwseq_erase_geometry(flash_size).map(Some)
        } else {
            Ok(None)
        }
    }

    fn spi_mode(&self) -> SpiMode {
        self.mode
    }

    fn features(&self) -> SpiFeatures {
        // Intel ICH/PCH SPI controller only supports single I/O mode
        // No 4-byte addressing, dual, or quad support
        SpiFeatures::empty()
    }

    fn max_read_len(&self) -> usize {
        Self::SWSEQ_MAX_DATA
    }

    fn max_write_len(&self) -> usize {
        Self::SWSEQ_MAX_DATA
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        use rflasher_core::spi::{AddressWidth, IoMode};

        // SpiMaster requires software sequencing mode
        if self.mode == SpiMode::HardwareSequencing {
            log::warn!(
                "SpiMaster::execute() called in hwseq mode - this mode doesn't support raw SPI commands"
            );
            return Err(CoreError::OpcodeNotSupported);
        }

        // WREN (0x06) and EWSR (0x50) are preops on Intel, not regular opcodes.
        // The higher-level protocol sends them separately; remember which one
        // and send it atomically with the next command.
        if cmd.opcode == JEDEC_WREN || cmd.opcode == JEDEC_EWSR {
            self.pending_preop = None;
            if self.find_preop(cmd.opcode).is_none() {
                return Err(CoreError::OpcodeNotSupported);
            }
            log::trace!("Deferring preop {:#04x} to the next command", cmd.opcode);
            self.pending_preop = Some(cmd.opcode);
            return Ok(());
        }
        let preop = self.pending_preop.take();

        // Intel controller doesn't support dummy cycles in swseq
        if cmd.dummy_cycles > 0 {
            log::debug!(
                "Dummy cycles ({}) not supported by Intel swseq",
                cmd.dummy_cycles
            );
            return Err(CoreError::OpcodeNotSupported);
        }

        // Only single I/O mode supported
        if cmd.io_mode != IoMode::Single {
            log::debug!("Only single I/O mode supported by Intel swseq");
            return Err(CoreError::OpcodeNotSupported);
        }

        // Build the write array for swseq_send_command
        // Format: opcode + [address bytes] + [write data]
        let mut writearr = [0u8; 68]; // 1 opcode + 3 addr + 64 data max
        let mut write_len = 0;

        // Opcode is always first
        writearr[0] = cmd.opcode;
        write_len += 1;

        // Add address if present
        if let Some(addr) = cmd.address {
            match cmd.address_width {
                AddressWidth::ThreeByte => {
                    Self::check_swseq_range(addr, cmd.write_data.len().max(cmd.read_buf.len()))
                        .map_err(InternalError::to_core_error)?;
                    writearr[1] = ((addr >> 16) & 0xff) as u8;
                    writearr[2] = ((addr >> 8) & 0xff) as u8;
                    writearr[3] = (addr & 0xff) as u8;
                    write_len += 3;
                }
                AddressWidth::FourByte => {
                    // Intel swseq doesn't support 4-byte addresses
                    log::debug!("4-byte addressing not supported by Intel swseq");
                    return Err(CoreError::OpcodeNotSupported);
                }
                AddressWidth::None => {
                    // Address provided but width is None - shouldn't happen but handle it
                }
            }
        }

        // Add write data if present
        if !cmd.write_data.is_empty() {
            let data_len = cmd.write_data.len();
            if write_len + data_len > 68 {
                log::debug!("Write data too long for Intel swseq");
                return Err(CoreError::IoError);
            }
            writearr[write_len..write_len + data_len].copy_from_slice(cmd.write_data);
            write_len += data_len;
        }

        // Execute the command - route to appropriate swseq implementation
        let result = if self.generation == IchChipset::Ich7 {
            self.ich7_swseq_send_command_after(&writearr[..write_len], cmd.read_buf, preop, false)
        } else {
            self.swseq_send_command_after(&writearr[..write_len], cmd.read_buf, preop, false)
        };
        result.map_err(InternalError::to_core_error)
    }

    fn check_access(&self, access: FlashAccess, addr: u32, len: u32) -> CoreResult<()> {
        self.check_range(access, addr, len.into())
            .map_err(InternalError::to_core_error)
    }

    fn chip_identified(&mut self, chip: &rflasher_core::chip::FlashChip) {
        self.adopt_chip(chip);
    }

    fn probe_opcode(&self, opcode: u8) -> bool {
        // In hwseq mode, no raw opcodes are available
        if self.mode == SpiMode::HardwareSequencing {
            return false;
        }

        // OPMENU opcodes, or WREN/EWSR sent atomically from the PREOP table.
        (self.opcode_intent(opcode).is_some() && self.menu_type(opcode).is_ok())
            || ((opcode == JEDEC_WREN || opcode == JEDEC_EWSR) && self.find_preop(opcode).is_some())
    }
}

// Firmware uses the same SPI protocol/protection/geometry code as native
// programmers. No Box or second firmware-specific flash implementation is needed.
impl<H: HostAccess> rflasher_core::programmer::SpiMaster for IchSpiController<H> {
    fn features(&self) -> SpiFeatures {
        Controller::features(self)
    }
    fn max_read_len(&self) -> usize {
        Controller::max_read_len(self)
    }
    fn max_write_len(&self) -> usize {
        Controller::max_write_len(self)
    }
    fn probe_opcode(&self, opcode: u8) -> bool {
        Controller::probe_opcode(self, opcode)
    }
    fn chip_identified(&mut self, chip: &rflasher_core::chip::FlashChip) {
        Controller::chip_identified(self, chip)
    }
    fn check_access(&self, access: FlashAccess, addr: u32, len: u32) -> CoreResult<()> {
        Controller::check_access(self, access, addr, len)
    }
    async fn execute(&mut self, command: &mut SpiCommand<'_>) -> CoreResult<()> {
        Controller::execute(self, command)
    }
    async fn delay_us(&mut self, us: u32) {
        self.host.delay_us(us);
    }
}

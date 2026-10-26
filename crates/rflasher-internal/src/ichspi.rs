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

#![cfg_attr(
    all(feature = "std", not(target_os = "linux")),
    allow(dead_code, unused_imports)
)]

use crate::DetectedChipset;
use crate::chipset::IchChipset;
use crate::controller::Controller;
use crate::error::InternalError;
#[cfg(all(feature = "std", target_os = "linux"))]
use crate::host::LinuxHost;
use crate::host::{HostAccess, MmioAccess, PciAddress, PciConfigAccess};
use crate::ich_regs::*;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};

#[inline]
fn spin_loop() {
    core::hint::spin_loop();
}

#[cfg(all(feature = "std", target_os = "linux"))]
struct Deadline {
    end_sec: i64,
    end_nsec: i64,
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl Deadline {
    fn new(timeout_us: u32) -> Self {
        let mut start = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock_gettime writes to a valid stack timespec.
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut start);
        }

        let timeout_ns = (timeout_us as i64) * 1000;
        let end_nsec = start.tv_nsec + timeout_ns;
        let end_sec = start.tv_sec + (end_nsec / 1_000_000_000);
        let end_nsec = end_nsec % 1_000_000_000;

        Self { end_sec, end_nsec }
    }

    fn expired(&mut self) -> bool {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: clock_gettime writes to a valid stack timespec.
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now);
        }

        now.tv_sec > self.end_sec || (now.tv_sec == self.end_sec && now.tv_nsec >= self.end_nsec)
    }
}

#[cfg(not(feature = "std"))]
struct Deadline {
    polls_left: u32,
}

#[cfg(not(feature = "std"))]
impl Deadline {
    fn new(timeout_us: u32) -> Self {
        Self {
            polls_left: timeout_us.saturating_mul(64).max(1),
        }
    }

    fn expired(&mut self) -> bool {
        let expired = self.polls_left == 0;
        self.polls_left = self.polls_left.saturating_sub(1);
        expired
    }
}
use rflasher_core::programmer::SpiFeatures;
use rflasher_core::spi::SpiCommand;

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

fn uses_silvermont_sbase(chipset: &DetectedChipset) -> bool {
    chipset.chipset_type() == IchChipset::BayTrail
        && matches!(chipset.enable.device_id, 0x0f1c..=0x0f1f | 0x1f38..=0x1f3b | 0x229c)
}

pub(crate) fn is_visible_spi_id(id: u16) -> bool {
    matches!(
        id,
        0x9d24
            | 0xa2a4
            | 0xa1a4
            | 0x9da4
            | 0xa324
            | 0x02a4
            | 0x06a4
            | 0x34a4
            | 0x4da4
            | 0xa3a4
            | 0xa0a4
            | 0x43a4
            | 0x51a4
            | 0x54a4
            | 0x7a24
            | 0x7aa4
            | 0x4b24
            | 0x5a96
            | 0x3196
            | 0x7e23
            | 0xa823
            | 0xae23
    )
}

/// PCI location of the BIOS control register and, on PCH100+, SPI BAR.
/// IDs and bridge mappings follow flashprog's chipset_enable.c. Do not infer
/// a location for other generations or IDs: an unrelated function may exist.
pub(crate) fn intel_config_bdf(chipset: &DetectedChipset) -> Result<PciAddress, InternalError> {
    let actual = PciAddress::new(
        chipset.domain,
        chipset.bus,
        chipset.device,
        chipset.function,
    );
    let id = chipset.enable.device_id;
    if is_visible_spi_id(id) && chipset.chipset_type().is_pch100_compatible() {
        return Ok(actual);
    }
    let location = match (chipset.chipset_type(), id) {
        (IchChipset::Series100SunrisePoint, _)
        | (IchChipset::C620Lewisburg, 0xa1c0..=0xa1c7)
        | (IchChipset::Series300CannonPoint, 0xa3c8 | 0xa3da) => (0x1f, 5),
        (IchChipset::ApolloLake, 0x5ae8) | (IchChipset::GeminiLake, 0x31e8 | 0x3197) => (0x0d, 2),
        (generation, _) if !generation.is_pch100_compatible() => return Ok(actual),
        _ => {
            return Err(InternalError::NotSupported(
                "No validated SPI PCI device mapping for this chipset",
            ));
        }
    };
    Ok(PciAddress::new(
        chipset.domain,
        chipset.bus,
        location.0,
        location.1,
    ))
}

/// Gets the SPI BAR physical address using host-provided PCI config access.
/// Linux uses sysfs only; inaccessible or hidden SPI devices are refused.
pub fn get_spibar_address_with_host<H: PciConfigAccess<Error = InternalError>>(
    host: &H,
    chipset: &DetectedChipset,
) -> Result<u64, InternalError> {
    let generation = chipset.chipset_type();

    if generation.is_pch100_compatible() {
        let spi_bdf = intel_config_bdf(chipset)?;
        let read_config = |offset| {
            host.read32(spi_bdf, offset).map_err(|e| {
            log::warn!("Cannot read SPI PCI configuration at {spi_bdf:?}: {e}");
            InternalError::ChipsetEnable("SPI PCI config unavailable or access denied; hidden SPI devices cannot be accessed safely via sysfs")
        })
        };
        let identity = read_config(0)?;
        let device = (identity >> 16) as u16;
        let valid = if is_visible_spi_id(chipset.enable.device_id) {
            device == chipset.enable.device_id
        } else {
            match generation {
                IchChipset::Series100SunrisePoint => matches!(device, 0x9d24 | 0xa2a4),
                IchChipset::C620Lewisburg => matches!(device, 0xa1a4 | 0xa224),
                IchChipset::Series300CannonPoint => device == 0xa3a4,
                IchChipset::ApolloLake => device == 0x5a96,
                IchChipset::GeminiLake => device == 0x3196,
                _ => false,
            }
        };
        if identity as u16 != 0x8086 || !valid {
            return Err(InternalError::ChipsetEnable(
                "PCI function is not a validated Intel SPI controller",
            ));
        }
        let spibar_raw = read_config(PCI_REG_SPIBAR as u16)?;
        // SPI BAR is a 32-bit memory BAR; never treat I/O or other BAR
        // encodings as a physical MMIO address.
        if spibar_raw & 0x7 != 0 {
            return Err(InternalError::ChipsetEnable(
                "SPI BAR is not a 32-bit memory BAR",
            ));
        }
        let addr = spibar_raw & 0xffff_f000;

        if addr == 0 || addr == 0xffff_f000 {
            return Err(InternalError::ChipsetEnable(
                "SPIBAR is invalid - SPI controller may be hidden or disabled by firmware",
            ));
        }

        Ok(addr as u64)
    } else if uses_silvermont_sbase(chipset) {
        let lpc_bdf = PciAddress::new(
            chipset.domain,
            chipset.bus,
            chipset.device,
            chipset.function,
        );
        let sbase = host.read32(lpc_bdf, 0x54)? & 0xffff_fe00;
        if sbase == 0 || sbase == 0xffff_fe00 {
            return Err(InternalError::ChipsetEnable("SBASE is invalid"));
        }
        Ok(u64::from(sbase))
    } else if generation.is_ich9_compatible() || generation == IchChipset::Ich7 {
        let lpc_bdf = PciAddress::new(
            chipset.domain,
            chipset.bus,
            chipset.device,
            chipset.function,
        );
        let rcba = host.read32(lpc_bdf, PCI_REG_RCBA as u16)?;
        if rcba & 1 == 0 {
            return Err(InternalError::ChipsetEnable("RCBA not enabled"));
        }

        let rcba_base = (rcba & !0x3fff) as u64;
        let spi_offset = match generation {
            // flashprog: SPIBAR is at RCRB+0x3020 for ICH7 and ICH8,
            // and at RCRB+0x3800 for ICH9 and newer legacy-RCBA chipsets.
            IchChipset::Ich7 | IchChipset::Ich8 => RCBA_SPI_OFFSET_ICH7,
            _ => RCBA_SPI_OFFSET_ICH9,
        };

        Ok(rcba_base + spi_offset as u64)
    } else {
        Err(InternalError::NotSupported(
            "Unsupported chipset generation",
        ))
    }
}

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

#[derive(Clone, Copy)]
struct RegisterChange {
    offset: usize,
    width16: bool,
    before: u32,
    after: u32,
    mask: u32,
}

/// Intel ICH/PCH SPI Controller
#[cfg(any(all(feature = "std", target_os = "linux"), not(feature = "std")))]
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
    /// BBAR value
    bbar: u32,
}

#[cfg(any(all(feature = "std", target_os = "linux"), not(feature = "std")))]
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
        Self::new_with_host(LinuxHost::new(), chipset, mode)
    }
}

#[cfg(any(all(feature = "std", target_os = "linux"), not(feature = "std")))]
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
            desc_valid: false,
            requested_mode: mode,
            mode: SpiMode::Auto, // Will be determined during init
            swseq,
            ich7_swseq,
            hwseq,
            opcodes: None,
            pending_preop: None,
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
                spin_loop();
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
        if self.change_count == 0 {
            return;
        }
        if self.wait_configuration_idle().is_err() {
            log::warn!("SPI controller busy; cannot restore configuration");
            return;
        }
        let locked = if self.generation == IchChipset::Ich7 {
            self.spibar.read16(ICH7_REG_SPIS) & (1 << 15) != 0
        } else {
            self.spibar.read16(ICH9_REG_HSFS) & HSFS_FLOCKDN != 0
        };
        if locked {
            log::warn!("SPI configuration locked; cannot restore configuration");
            return;
        }
        let dlock = if self.generation.is_pch100_compatible() {
            self.spibar.read32(PCH100_REG_DLOCK)
        } else {
            0
        };
        for change in self.changes[..self.change_count].iter().rev().flatten() {
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
                continue;
            }
            let current = if change.width16 {
                u32::from(self.spibar.read16(change.offset))
            } else {
                self.spibar.read32(change.offset)
            };
            if current & change.mask != change.after & change.mask {
                log::warn!(
                    "SPI register {:#x} changed externally; not restoring it",
                    change.offset
                );
                continue;
            }
            let restored = (current & !change.mask) | (change.before & change.mask);
            if change.width16 {
                self.spibar.write16(change.offset, restored as u16);
            } else {
                self.spibar.write32(change.offset, restored);
            }
            let verified = if change.width16 {
                u32::from(self.spibar.read16(change.offset))
            } else {
                self.spibar.read32(change.offset)
            };
            if verified & change.mask != restored & change.mask {
                log::warn!("SPI register {:#x} restore failed", change.offset);
            }
        }
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
            let offs = 0x60 + (i * 4);
            let pbr = self.spibar.read32(offs);
            log::debug!("{:#04x}: {:#010x} (PBR{})", offs, pbr, i);
        }

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

    /// Initialize opcodes for ICH7 software sequencing
    fn init_ich7_opcodes(&mut self) -> Result<(), InternalError> {
        if self.locked {
            // Read existing opcodes from hardware
            log::debug!("ICH7: Reading OPCODES from locked controller...");
            self.opcodes = Some(self.generate_ich7_opcodes());
        } else {
            // Program our default opcodes
            log::debug!("ICH7: Programming OPCODES...");
            let opcodes = Opcodes::default();
            self.program_ich7_opcodes(&opcodes)?;
            self.opcodes = Some(opcodes);
        }

        if let Some(ref opcodes) = self.opcodes {
            self.print_opcodes(opcodes);
        }

        Ok(())
    }

    /// Generate opcodes from ICH7 hardware registers
    fn generate_ich7_opcodes(&self) -> Opcodes {
        let preop = self.spibar.read16(self.ich7_swseq.preop);
        let optype = self.spibar.read16(self.ich7_swseq.optype);
        let opmenu_lo = self.spibar.read32(self.ich7_swseq.opmenu);
        let opmenu_hi = self.spibar.read32(self.ich7_swseq.opmenu + 4);

        let mut opcodes = Opcodes {
            preop: [preop as u8, (preop >> 8) as u8],
            opcode: [Opcode::default(); 8],
        };

        let mut optype_val = optype;
        for i in 0..8 {
            opcodes.opcode[i].spi_type = (optype_val & 0x3) as u8;
            optype_val >>= 2;
        }

        let mut opmenu = opmenu_lo;
        for i in 0..4 {
            opcodes.opcode[i].opcode = (opmenu & 0xFF) as u8;
            opmenu >>= 8;
        }

        opmenu = opmenu_hi;
        for i in 4..8 {
            opcodes.opcode[i].opcode = (opmenu & 0xFF) as u8;
            opmenu >>= 8;
        }

        opcodes
    }

    /// Program opcodes to ICH7 hardware registers
    fn program_ich7_opcodes(&mut self, opcodes: &Opcodes) -> Result<(), InternalError> {
        let preop = (opcodes.preop[0] as u16) | ((opcodes.preop[1] as u16) << 8);

        let mut optype: u16 = 0;
        for (i, op) in opcodes.opcode.iter().enumerate() {
            optype |= (op.spi_type as u16) << (i * 2);
        }

        let mut opmenu_lo: u32 = 0;
        for i in 0..4 {
            opmenu_lo |= (opcodes.opcode[i].opcode as u32) << (i * 8);
        }

        let mut opmenu_hi: u32 = 0;
        for i in 4..8 {
            opmenu_hi |= (opcodes.opcode[i].opcode as u32) << ((i - 4) * 8);
        }

        self.change_register(self.ich7_swseq.preop, true, preop.into(), 0xffff);
        self.change_register(self.ich7_swseq.optype, true, optype.into(), 0xffff);
        self.change_register(self.ich7_swseq.opmenu, false, opmenu_lo, u32::MAX);
        self.change_register(self.ich7_swseq.opmenu + 4, false, opmenu_hi, u32::MAX);

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

        // Handle protected ranges
        self.handle_protected_ranges();

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

    /// Initialize opcodes for software sequencing
    fn init_opcodes(&mut self) -> Result<(), InternalError> {
        if self.locked || self.swseq_locked {
            // Read existing opcodes from hardware
            log::debug!("Reading OPCODES from locked controller...");
            self.opcodes = Some(self.generate_opcodes());
        } else {
            // Program our default opcodes
            log::debug!("Programming OPCODES...");
            let opcodes = Opcodes::default();
            self.program_opcodes(&opcodes)?;
            self.opcodes = Some(opcodes);
        }

        if let Some(ref opcodes) = self.opcodes {
            self.print_opcodes(opcodes);
        }

        Ok(())
    }

    /// Generate opcodes from hardware registers
    fn generate_opcodes(&self) -> Opcodes {
        let preop = self.spibar.read16(self.swseq.preop);
        let optype = self.spibar.read16(self.swseq.optype);
        let opmenu_lo = self.spibar.read32(self.swseq.opmenu);
        let opmenu_hi = self.spibar.read32(self.swseq.opmenu + 4);

        let mut opcodes = Opcodes {
            preop: [preop as u8, (preop >> 8) as u8],
            opcode: [Opcode::default(); 8],
        };

        let mut optype_val = optype;
        for i in 0..8 {
            opcodes.opcode[i].spi_type = (optype_val & 0x3) as u8;
            optype_val >>= 2;
        }

        let mut opmenu = opmenu_lo;
        for i in 0..4 {
            opcodes.opcode[i].opcode = (opmenu & 0xFF) as u8;
            opmenu >>= 8;
        }

        opmenu = opmenu_hi;
        for i in 4..8 {
            opcodes.opcode[i].opcode = (opmenu & 0xFF) as u8;
            opmenu >>= 8;
        }

        opcodes
    }

    /// Program opcodes to hardware registers
    fn program_opcodes(&mut self, opcodes: &Opcodes) -> Result<(), InternalError> {
        let preop = (opcodes.preop[0] as u16) | ((opcodes.preop[1] as u16) << 8);

        let mut optype: u16 = 0;
        for (i, op) in opcodes.opcode.iter().enumerate() {
            optype |= (op.spi_type as u16) << (i * 2);
        }

        let mut opmenu_lo: u32 = 0;
        for i in 0..4 {
            opmenu_lo |= (opcodes.opcode[i].opcode as u32) << (i * 8);
        }

        let mut opmenu_hi: u32 = 0;
        for i in 4..8 {
            opmenu_hi |= (opcodes.opcode[i].opcode as u32) << ((i - 4) * 8);
        }

        self.change_register(self.swseq.preop, true, preop.into(), 0xffff);
        self.change_register(self.swseq.optype, true, optype.into(), 0xffff);
        self.change_register(self.swseq.opmenu, false, opmenu_lo, u32::MAX);
        self.change_register(self.swseq.opmenu + 4, false, opmenu_hi, u32::MAX);

        Ok(())
    }

    /// Handle access permissions from FRAP/FREG
    fn handle_access_permissions(&mut self) -> Result<(), InternalError> {
        let frap = self.spibar.read32(ICH9_REG_FRAP);
        log::debug!("FRAP: {:#010x}", frap);

        let brwa = ((frap >> 8) & 0xFF) as u8;
        let brra = (frap & 0xFF) as u8;
        log::debug!("BRWA: {:#04x}, BRRA: {:#04x}", brwa, brra);

        // For PCH100+ with new access permissions
        let (bm_wap, bm_rap, max_regions) = if self.generation.has_new_access_perm() {
            let wap = self.spibar.read32(BIOS_BM_WAP);
            let rap = self.spibar.read32(BIOS_BM_RAP);
            log::debug!("BIOS_BM_WAP: {:#010x}", wap);
            log::debug!("BIOS_BM_RAP: {:#010x}", rap);
            (wap, rap, 32)
        } else {
            (brwa as u32, brra as u32, 8)
        };

        // Determine number of regions based on chipset
        let num_freg = match self.generation {
            IchChipset::Series100SunrisePoint => 10,
            IchChipset::C620Lewisburg => 12,
            _ if self.generation.is_pch100_compatible() => 16,
            _ => 5,
        };

        // Check each region's access permissions
        let mut restricted = false;
        for i in 0..num_freg {
            let offset = if i < 12 {
                ICH9_REG_FREG0 + i * 4
            } else {
                APL_REG_FREG12 + (i - 12) * 4
            };

            let freg = self.spibar.read32(offset);
            let base = freg_base(freg);
            let limit = freg_limit(freg);

            // Skip disabled regions
            if base > limit || (freg == 0 && i > 0) {
                continue;
            }

            // Check permissions
            let can_read = if i < max_regions {
                (bm_rap >> i) & 1 != 0
            } else {
                true
            };
            let can_write = if i < max_regions {
                (bm_wap >> i) & 1 != 0
            } else {
                true
            };
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

    /// Handle protected range registers
    fn handle_protected_ranges(&mut self) {
        let num_pr = if self.generation.is_pch100_compatible() {
            6
        } else {
            5
        };
        let reg_pr0 = if self.generation.is_pch100_compatible() {
            PCH100_REG_FPR0
        } else {
            ICH9_REG_PR0
        };

        for i in 0..num_pr {
            // Try to clear protection if not locked (as flashprog does). This
            // changes chipset state even for read-only operations, so say so.
            let pr_locked = self.generation.is_pch100_compatible()
                && self.spibar.read32(PCH100_REG_DLOCK) & (DLOCK_PR0_LOCKDN << i) != 0;
            if !self.locked && !pr_locked {
                let before = self.spibar.read32(reg_pr0 + i * 4);
                self.set_pr(reg_pr0, i, false, false);
                if (before >> PR_RP_OFF) & 1 != 0 || (before >> PR_WP_OFF) & 1 != 0 {
                    log::info!(
                        "PR{}: {:#010x}-{:#010x} had {}protection set; cleared it \
                         (controller is not locked down)",
                        i,
                        freg_base(before),
                        freg_limit(before),
                        match (
                            (before >> PR_RP_OFF) & 1 != 0,
                            (before >> PR_WP_OFF) & 1 != 0
                        ) {
                            (true, true) => "read and write ",
                            (true, false) => "read ",
                            _ => "write ",
                        }
                    );
                }
            }

            let pr = self.spibar.read32(reg_pr0 + i * 4);
            let base = freg_base(pr);
            let limit = freg_limit(pr);

            let rp = (pr >> PR_RP_OFF) & 1 != 0;
            let wp = (pr >> PR_WP_OFF) & 1 != 0;

            // PR bits are inverted: 1 = protected
            if rp || wp {
                log::warn!(
                    "PR{}: {:#010x}-{:#010x} is {}",
                    i,
                    base,
                    limit,
                    match (rp, wp) {
                        (true, true) => "read/write protected",
                        (true, false) => "read protected",
                        (false, true) => "write protected",
                        _ => "accessible",
                    }
                );
            }
        }
    }

    /// Set protection range register
    fn set_pr(&mut self, reg_pr0: usize, index: usize, read_prot: bool, write_prot: bool) {
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
    fn set_bbar(&mut self, min_addr: u32) {
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

    /// Check if any required opcodes are missing
    fn missing_opcodes(&self) -> bool {
        let required = [JEDEC_READ, JEDEC_RDSR];

        if let Some(ref opcodes) = self.opcodes {
            for req in required {
                if !opcodes.opcode.iter().any(|op| op.opcode == req) {
                    return true;
                }
            }
        }

        false
    }

    /// Find an opcode in the table
    #[allow(dead_code)]
    fn find_opcode(&self, opcode: u8) -> Option<usize> {
        self.opcodes
            .as_ref()?
            .opcode
            .iter()
            .position(|op| op.opcode == opcode)
    }

    /// Check if an opcode is available in the OPMENU table
    ///
    /// This is used internally for the Controller trait's `probe_opcode` method.
    fn has_opcode(&self, opcode: u8) -> bool {
        self.find_opcode_index(opcode).is_some()
    }

    /// Get the opcode type for an opcode in the table
    ///
    /// Returns the SPI opcode type (read/write, with/without address) if found.
    #[allow(dead_code)]
    fn get_opcode_type(&self, opcode: u8) -> Option<u8> {
        let idx = self.find_opcode_index(opcode)?;
        self.opcodes.as_ref().map(|ops| ops.opcode[idx].spi_type)
    }

    /// Find if an opcode is in the preop table
    ///
    /// Returns the preop index (0 or 1) if found, None otherwise.
    /// This is used to detect if a command is a preop like WREN or EWSR.
    fn find_preop(&self, opcode: u8) -> Option<usize> {
        self.opcodes
            .as_ref()?
            .preop
            .iter()
            .position(|&p| p == opcode)
    }

    /// Get the atomic mode for an opcode that needs a preop
    ///
    /// For write operations (page program, erase, status register writes),
    /// we need to send WREN first. The Intel controller supports "atomic"
    /// operations where it automatically sends a preop before the main command.
    ///
    /// Returns:
    /// - 0 = no preop needed
    /// - 1 = use preop[0] (typically WREN)
    /// - 2 = use preop[1] (typically EWSR)
    fn get_atomic_for_opcode(&self, opcode: u8) -> u8 {
        let opcodes = match &self.opcodes {
            Some(ops) => ops,
            None => return 0,
        };

        // List of opcodes that require WREN (preop[0])
        // These are write/erase operations that modify flash content or status
        let needs_wren = matches!(
            opcode,
            JEDEC_BYTE_PROGRAM  // 0x02 - Page Program
            | JEDEC_SE          // 0x20 - Sector Erase 4KB
            | JEDEC_BE_52       // 0x52 - Block Erase 32KB
            | JEDEC_BE_D8       // 0xD8 - Block Erase 64KB
            | JEDEC_CE_C7       // 0xC7 - Chip Erase
            | JEDEC_CE_60       // 0x60 - Chip Erase (alternative)
            | JEDEC_WRSR | 0x31 | 0x11 // Write status registers
        );

        if needs_wren {
            // Check if WREN is in preop[0] position
            if opcodes.preop[0] == JEDEC_WREN {
                return 1; // Use preop[0]
            }
            // Check if WREN is in preop[1] position
            if opcodes.preop[1] == JEDEC_WREN {
                return 2; // Use preop[1]
            }
            // WREN not in preop table - this is a problem but log and continue
            log::warn!(
                "WREN (0x06) not in preop table, atomic mode not available for opcode {:#04x}",
                opcode
            );
        }

        0 // No preop needed
    }

    /// Atomic mode for a write `opcode`. A preop the caller sent first
    /// (`SpiMaster::execute`) selects its PREOP slot, so chips whose WRSR
    /// needs EWSR (older SST25) get it; otherwise WREN as needed.
    fn select_atomic(&self, opcode: u8, preop: Option<u8>) -> Result<u8, InternalError> {
        match preop.map(|preop| (preop, self.find_preop(preop))) {
            Some((_, Some(index))) => Ok(index as u8 + 1),
            Some((JEDEC_EWSR, None)) => Err(InternalError::Io("EWSR not in the PREOP table")),
            _ => Ok(self.get_atomic_for_opcode(opcode)),
        }
    }

    /// Print HSFS register bits
    fn print_hsfs(&self, hsfs: u16) {
        log::debug!(
            "HSFS: FDONE={} FCERR={} AEL={} SCIP={} FDV={} FLOCKDN={}",
            (hsfs & HSFS_FDONE) != 0,
            (hsfs & HSFS_FCERR) != 0,
            (hsfs & HSFS_AEL) != 0,
            (hsfs & HSFS_SCIP) != 0,
            (hsfs & HSFS_FDV) != 0,
            (hsfs & HSFS_FLOCKDN) != 0
        );
    }

    /// Print DLOCK register bits (PCH100+)
    fn print_dlock(&self, dlock: u32) {
        log::debug!(
            "DLOCK: BMWAG_LOCKDN={} BMRAG_LOCKDN={} SBMWAG_LOCKDN={} SBMRAG_LOCKDN={} PR0_LOCKDN={} SSEQ_LOCKDN={}",
            (dlock & DLOCK_BMWAG_LOCKDN) != 0,
            (dlock & DLOCK_BMRAG_LOCKDN) != 0,
            (dlock & DLOCK_SBMWAG_LOCKDN) != 0,
            (dlock & DLOCK_SBMRAG_LOCKDN) != 0,
            (dlock & DLOCK_PR0_LOCKDN) != 0,
            (dlock & DLOCK_SSEQ_LOCKDN) != 0
        );
    }

    /// Print opcodes table
    fn print_opcodes(&self, opcodes: &Opcodes) {
        log::debug!(
            "Preop: [{:#04x}, {:#04x}]",
            opcodes.preop[0],
            opcodes.preop[1]
        );
        for (i, op) in opcodes.opcode.iter().enumerate() {
            let type_str = match op.spi_type {
                SPI_OPCODE_TYPE_READ_NO_ADDRESS => "read w/o addr",
                SPI_OPCODE_TYPE_WRITE_NO_ADDRESS => "write w/o addr",
                SPI_OPCODE_TYPE_READ_WITH_ADDRESS => "read w/ addr",
                SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS => "write w/ addr",
                _ => "unknown",
            };
            log::debug!("op[{}]: {:#04x} ({})", i, op.opcode, type_str);
        }
    }

    /// Enable writes lazily, before a flash mutation is issued.
    fn mutation_gate(&mut self) -> Result<(), InternalError> {
        if !self.bios_writes_enabled {
            self.enable_bios_write()?;
        }
        Ok(())
    }

    /// Classify supported commands by their effect, not their transfer framing.
    fn opcode_intent(opcode: u8) -> Option<MutationIntent> {
        match opcode {
            // RES can also release the device from deep power-down; it does not
            // modify flash contents or protection. 0x4b reads a unique ID.
            JEDEC_READ | JEDEC_FAST_READ | JEDEC_RDSR | 0x15 | JEDEC_REMS | JEDEC_RDID
            | JEDEC_RES | 0x4b => Some(MutationIntent::ReadOnly),
            // 0x35 is RDSR2 on some parts but enters QPI on Macronix parts.
            // OPMENU transfer type does not identify the flash's interpretation.
            JEDEC_BYTE_PROGRAM | JEDEC_SE | JEDEC_BE_52 | JEDEC_BE_D8 | JEDEC_CE_C7
            | JEDEC_CE_60 | JEDEC_WRSR | 0x31 | 0x11 => Some(MutationIntent::Mutating),
            _ => None,
        }
    }

    fn gate_opcode(&mut self, opcode: u8, allow_unknown: bool) -> Result<(), InternalError> {
        match Self::opcode_intent(opcode) {
            Some(MutationIntent::ReadOnly) => Ok(()),
            Some(MutationIntent::Mutating) => self.mutation_gate(),
            None if allow_unknown => self.mutation_gate(),
            None => Err(InternalError::NotSupported("Unclassified SPI opcode")),
        }
    }

    /// Enable BIOS write access via BIOS_CNTL register
    pub fn enable_bios_write(&mut self) -> Result<(), InternalError> {
        self.bios_writes_enabled = false;
        if self.silvermont_bcr {
            let bcr = self.spibar.read32(SILVERMONT_REG_BCR);
            if bcr & u32::from(BIOS_CNTL_BLE) != 0 {
                log::warn!("BIOS Lock Enable is set - writes may trigger SMI");
            }
            if bcr & u32::from(BIOS_CNTL_SMM_BWP) != 0 {
                return Err(InternalError::AccessDenied { region: "BIOS" });
            }
            if bcr & u32::from(BIOS_CNTL_BWE) == 0 {
                // Record ownership before the write so failed verification is cleaned up too.
                self.restore_bcr_wpd = true;
                self.spibar
                    .write32(SILVERMONT_REG_BCR, bcr | u32::from(BIOS_CNTL_BWE));
                if self.spibar.read32(SILVERMONT_REG_BCR) & u32::from(BIOS_CNTL_BWE) == 0 {
                    return Err(InternalError::ChipsetEnable("Cannot enable BIOS writes"));
                }
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

    // ========================================================================
    // Hardware Sequencing Operations
    // ========================================================================

    /// Read the descriptor through the controller's observability registers.
    /// Only generations whose FLCOMP encoding is documented by flashprog are
    /// handled; later engines retain the region-based fallback.
    fn descriptor_flash_size(&self) -> Result<Option<u32>, InternalError> {
        if !self.desc_valid {
            return Err(InternalError::InvalidDescriptor);
        }
        let new_density = match self.generation {
            IchChipset::Ich9
            | IchChipset::Ich10
            | IchChipset::Series5IbexPeak
            | IchChipset::Series6CougarPoint
            | IchChipset::Series7PantherPoint
            | IchChipset::BayTrail => false,
            IchChipset::Series8LynxPoint
            | IchChipset::Series8LynxPointLp
            | IchChipset::Series8Wellsburg
            | IchChipset::Series9WildcatPoint
            | IchChipset::Series9WildcatPointLp
            | IchChipset::Series100SunrisePoint => true,
            _ => return Ok(None),
        };
        let (fdoc, fdod) = if self.generation.is_pch100_compatible() {
            (0xb4, 0xb8)
        } else {
            (0xb0, 0xb4)
        };
        let read = |selector| {
            self.spibar.write32(fdoc, selector);
            self.spibar.read32(fdod)
        };
        let saved_selector = self.spibar.read32(fdoc);
        let signature = read(0);
        let map = read(4);
        let comp = read(1 << 12);
        self.spibar.write32(fdoc, saved_selector);
        if signature != 0x0ff0_a55a {
            return Err(InternalError::InvalidDescriptor);
        }
        let count = (map >> 8) & 3;
        if count > 1 {
            return Err(InternalError::InvalidDescriptor);
        }
        let width = if new_density { 4 } else { 3 };
        let mask = (1 << width) - 1;
        let max = if new_density { 7 } else { 5 };
        let density = |index: u32| -> Option<u32> {
            let code = (comp >> (index * width)) & mask;
            if code > max {
                None
            } else {
                Some(1u32 << (19 + code))
            }
        };
        let first = density(0).ok_or(InternalError::InvalidDescriptor)?;
        let second = if count == 1 {
            density(1).ok_or(InternalError::InvalidDescriptor)?
        } else {
            0
        };
        let size = first
            .checked_add(second)
            .ok_or(InternalError::InvalidDescriptor)?;
        if u64::from(size) > u64::from(self.hwseq.addr_mask) + 1 {
            return Err(InternalError::NotSupported(
                "Flash exceeds hardware sequencing address range",
            ));
        }
        Ok(Some(size))
    }

    /// Reject ranges that FADDR would truncate before touching controller registers.
    fn hwseq_check_range(&self, addr: u32, len: usize) -> Result<(), InternalError> {
        let end = u64::try_from(len)
            .ok()
            .and_then(|len| (addr as u64).checked_add(len));
        if addr & !self.hwseq.addr_mask != 0
            || end.is_none_or(|end| end > self.hwseq.addr_mask as u64 + 1)
        {
            return Err(InternalError::Io(
                "Hardware sequencing address out of range",
            ));
        }
        Ok(())
    }

    /// Set the flash address for hardware sequencing
    #[inline(always)]
    fn hwseq_set_addr(&self, addr: u32) {
        self.spibar
            .write32(ICH9_REG_FADDR, addr & self.hwseq.addr_mask);
    }

    /// Wait for hardware sequencing cycle to complete
    ///
    /// Polls for FDONE or FCERR in HSFS register.
    /// Uses a busy-loop with clock_gettime for precise sub-microsecond timing,
    /// matching flashprog's approach for maximum throughput.
    #[inline(always)]
    #[allow(clippy::unnecessary_cast)] // Casts needed for i686 where tv_sec/tv_nsec are i32
    fn hwseq_wait_for_cycle(&self, timeout_us: u32) -> Result<(), InternalError> {
        let done_or_err = HSFS_FDONE | HSFS_FCERR;

        let mut deadline = Deadline::new(timeout_us);

        loop {
            let hsfs = self.spibar.read16(ICH9_REG_HSFS);

            if hsfs & done_or_err != 0 {
                // Clear status bits by writing 1s to them (W1C)
                self.spibar.write16(ICH9_REG_HSFS, hsfs);

                if hsfs & HSFS_FCERR != 0 {
                    return Err(InternalError::Io("Hardware sequencing cycle error"));
                }

                return Ok(());
            }

            if deadline.expired() {
                return Err(InternalError::Io("Hardware sequencing timeout"));
            }

            // Hint to CPU that we're spinning (reduces power, improves perf on hyperthreaded cores)
            spin_loop();
        }
    }

    /// Read data using hardware sequencing
    ///
    /// This is the main read path - optimized for throughput.
    pub fn hwseq_read(&self, addr: u32, buf: &mut [u8]) -> Result<(), InternalError> {
        let len = buf.len();
        self.hwseq_check_range(addr, len)?;
        if len == 0 {
            return Ok(());
        }

        let mut offset = 0;
        let mut current_addr = addr as u64;

        // Clear FDONE, FCERR, AEL by writing 1s to them (do once at start)
        self.spibar
            .write16(ICH9_REG_HSFS, self.spibar.read16(ICH9_REG_HSFS));

        while offset < len {
            // Calculate block size (max 64 bytes, respect 256-byte page boundaries)
            let remaining = len - offset;
            let page_remaining = 256 - (current_addr as usize & 0xFF);
            let block_len = remaining.min(HWSEQ_MAX_DATA).min(page_remaining);

            self.hwseq_set_addr(current_addr as u32);

            // Set up read cycle using read-modify-write to preserve reserved bits
            let mut hsfc = self.spibar.read16(ICH9_REG_HSFC);
            hsfc &= !self.hwseq.hsfc_fcycle; // Clear FCYCLE (0 = read)
            hsfc &= !HSFC_FDBC; // Clear byte count
            hsfc |= ((block_len - 1) as u16) << HSFC_FDBC_OFF; // Set byte count
            hsfc |= HSFC_FGO; // Start
            self.spibar.write16(ICH9_REG_HSFC, hsfc);

            // Wait for completion (30 second timeout)
            self.hwseq_wait_for_cycle(30_000_000)?;

            // Read data from FDATA registers
            self.read_data(&mut buf[offset..offset + block_len]);

            offset += block_len;
            current_addr += block_len as u64;
        }

        Ok(())
    }

    /// Write data using hardware sequencing
    ///
    /// This is the main write path - optimized for throughput.
    pub fn hwseq_write(&mut self, addr: u32, data: &[u8]) -> Result<(), InternalError> {
        let len = data.len();
        self.hwseq_check_range(addr, len)?;
        if len == 0 {
            return Ok(());
        }

        self.mutation_gate()?;
        let mut offset = 0;
        let mut current_addr = addr as u64;

        // Clear FDONE, FCERR, AEL by writing 1s to them (do once at start)
        self.spibar
            .write16(ICH9_REG_HSFS, self.spibar.read16(ICH9_REG_HSFS));

        while offset < len {
            // Calculate block size (max 64 bytes, respect 256-byte page boundaries)
            let remaining = len - offset;
            let page_remaining = 256 - (current_addr as usize & 0xFF);
            let block_len = remaining.min(HWSEQ_MAX_DATA).min(page_remaining);

            self.hwseq_set_addr(current_addr as u32);

            // Fill data registers first (before starting cycle)
            self.fill_data(&data[offset..offset + block_len]);

            // Set up write cycle using read-modify-write to preserve reserved bits
            let mut hsfc = self.spibar.read16(ICH9_REG_HSFC);
            hsfc &= !self.hwseq.hsfc_fcycle; // Clear FCYCLE
            hsfc |= 0x2 << HSFC_FCYCLE_OFF; // Set write cycle
            hsfc &= !HSFC_FDBC; // Clear byte count
            hsfc |= ((block_len - 1) as u16) << HSFC_FDBC_OFF; // Set byte count
            hsfc |= HSFC_FGO; // Start
            self.spibar.write16(ICH9_REG_HSFC, hsfc);

            // Wait for completion (30 second timeout)
            self.hwseq_wait_for_cycle(30_000_000)?;

            offset += block_len;
            current_addr += block_len as u64;
        }

        Ok(())
    }

    /// Size of the erase block the sequencer will erase at `addr`.
    ///
    /// PCH100 and later always erase 4 KiB. Older controllers erase whatever
    /// HSFS.BERASE says, and BERASE depends on the flash partition FADDR
    /// points into (see `hwseq_erase_geometry`), so FADDR is loaded first;
    /// this mirrors flashprog's `ich_hwseq_get_erase_block_size`.
    fn hwseq_erase_block_size(&self, addr: u32) -> u32 {
        if self.hwseq.only_4k {
            return HWSEQ_ONLY_4K_ERASE_SIZE;
        }
        self.hwseq_set_addr(addr);
        berase_block_size(self.spibar.read16(ICH9_REG_HSFS))
    }

    /// Uniform erase block size to plan erases with, for a flash of `flash_size` bytes.
    ///
    /// Erase cycles always erase the *whole* block containing FADDR, and the
    /// block size is a property of the hardware (BERASE, per flash
    /// partition), not of the request. Planning with any other size would
    /// erase data next to the requested range, so it is read here instead of
    /// assumed.
    pub fn hwseq_erase_geometry(&self, flash_size: u32) -> Result<u32, InternalError> {
        let boundary = if self.hwseq.only_4k {
            0
        } else {
            (self.spibar.read32(ICH9_REG_FPB) & FPB_FPBA) << 12
        };
        let low = self.hwseq_erase_block_size(0);
        let high = if boundary != 0 && boundary < flash_size {
            self.hwseq_erase_block_size(boundary)
        } else {
            low
        };
        uniform_erase_size(low, high, boundary).ok_or(InternalError::NotSupported(
            "Hardware sequencing reports different erase block sizes for the two flash partitions",
        ))
    }

    /// Erase blocks using hardware sequencing
    ///
    /// `addr` and `len` must consist of whole hardware erase blocks. This is
    /// verified for the entire range before the first cycle is started.
    pub fn hwseq_erase(&mut self, addr: u32, len: u32) -> Result<(), InternalError> {
        self.hwseq_check_range(addr, len as usize)?;

        let end_addr = addr as u64 + len as u64;

        // Each cycle erases the block containing FADDR whatever we asked for,
        // so refuse anything that is not exactly a sequence of whole blocks.
        let mut current_addr = addr as u64;
        while current_addr < end_addr {
            let erase_size = self.hwseq_erase_block_size(current_addr as u32) as u64;
            if !current_addr.is_multiple_of(erase_size) || current_addr + erase_size > end_addr {
                return Err(InternalError::Io(
                    "Erase address/length not aligned to the controller's erase block size",
                ));
            }
            current_addr += erase_size;
        }

        if len == 0 {
            return Ok(());
        }
        self.mutation_gate()?;
        // Clear FDONE, FCERR, AEL by writing 1s to them (do once at start)
        self.spibar
            .write16(ICH9_REG_HSFS, self.spibar.read16(ICH9_REG_HSFS));

        let mut current_addr = addr as u64;
        while current_addr < end_addr {
            let erase_size = self.hwseq_erase_block_size(current_addr as u32) as u64;
            self.hwseq_set_addr(current_addr as u32);

            // Set up erase cycle using read-modify-write to preserve reserved bits
            let mut hsfc = self.spibar.read16(ICH9_REG_HSFC);
            hsfc &= !self.hwseq.hsfc_fcycle; // Clear FCYCLE
            hsfc |= 0x3 << HSFC_FCYCLE_OFF; // Set erase cycle
            hsfc |= HSFC_FGO; // Start
            self.spibar.write16(ICH9_REG_HSFC, hsfc);

            // Wait for completion (60 second timeout for erase)
            self.hwseq_wait_for_cycle(60_000_000)?;

            current_addr += erase_size;
        }

        Ok(())
    }

    /// Read data from FDATA registers
    ///
    /// Optimized to read full 32-bit words and extract bytes.
    /// This is performance-critical for flash read operations.
    #[inline(always)]
    fn read_data(&self, buf: &mut [u8]) {
        let len = buf.len();
        let mut offset = 0;

        // Process full 32-bit words
        while offset + 4 <= len {
            let temp = self.spibar.read32(ICH9_REG_FDATA0 + offset);
            // Use native endianness (x86 is little-endian, matching flash byte order)
            buf[offset] = temp as u8;
            buf[offset + 1] = (temp >> 8) as u8;
            buf[offset + 2] = (temp >> 16) as u8;
            buf[offset + 3] = (temp >> 24) as u8;
            offset += 4;
        }

        // Handle remaining bytes (0-3)
        if offset < len {
            let temp = self.spibar.read32(ICH9_REG_FDATA0 + offset);
            let remaining = len - offset;
            if remaining > 0 {
                buf[offset] = temp as u8;
            }
            if remaining > 1 {
                buf[offset + 1] = (temp >> 8) as u8;
            }
            if remaining > 2 {
                buf[offset + 2] = (temp >> 16) as u8;
            }
        }
    }

    /// Fill FDATA registers with data
    ///
    /// Optimized to write full 32-bit words.
    /// This is performance-critical for flash write operations.
    #[inline(always)]
    fn fill_data(&self, data: &[u8]) {
        let len = data.len();
        if len == 0 {
            return;
        }

        let mut offset = 0;

        // Process full 32-bit words
        while offset + 4 <= len {
            let temp = (data[offset] as u32)
                | ((data[offset + 1] as u32) << 8)
                | ((data[offset + 2] as u32) << 16)
                | ((data[offset + 3] as u32) << 24);
            self.spibar.write32(ICH9_REG_FDATA0 + offset, temp);
            offset += 4;
        }

        // Handle remaining bytes (0-3)
        if offset < len {
            let mut temp: u32 = 0;
            let remaining = len - offset;
            if remaining > 0 {
                temp |= data[offset] as u32;
            }
            if remaining > 1 {
                temp |= (data[offset + 1] as u32) << 8;
            }
            if remaining > 2 {
                temp |= (data[offset + 2] as u32) << 16;
            }
            self.spibar.write32(ICH9_REG_FDATA0 + offset, temp);
        }
    }

    // ========================================================================
    // Software Sequencing Operations
    // ========================================================================

    /// Maximum data length for software sequencing (64 bytes)
    pub const SWSEQ_MAX_DATA: usize = 64;

    /// Wait for software sequencing cycle to not be in progress
    ///
    /// This should be called before starting a new cycle to ensure the
    /// previous one has completed.
    #[allow(clippy::unnecessary_cast)] // Casts needed for i686 where tv_sec/tv_nsec are i32
    fn swseq_wait_idle(&self, timeout_us: u32) -> Result<(), InternalError> {
        let mut deadline = Deadline::new(timeout_us);

        loop {
            let ssfs = self.spibar.read8(self.swseq.ssfsc);

            if ssfs & (SSFS_SCIP as u8) == 0 {
                return Ok(());
            }

            if deadline.expired() {
                return Err(InternalError::Io("SCIP never cleared (swseq busy timeout)"));
            }

            spin_loop();
        }
    }

    /// Wait for software sequencing cycle to complete
    #[allow(clippy::unnecessary_cast)] // Casts needed for i686 where tv_sec/tv_nsec are i32
    fn swseq_wait_complete(&self, timeout_us: u32) -> Result<(), InternalError> {
        let done_or_err = SSFS_FDONE | SSFS_FCERR;

        let mut deadline = Deadline::new(timeout_us);

        loop {
            let ssfsc = self.spibar.read32(self.swseq.ssfsc);

            if ssfsc & done_or_err != 0 {
                // Clear status bits
                let clear =
                    (ssfsc & (SSFS_RESERVED_MASK | SSFC_RESERVED_MASK)) | SSFS_FDONE | SSFS_FCERR;
                self.spibar.write32(self.swseq.ssfsc, clear);

                if ssfsc & SSFS_FCERR != 0 {
                    log::debug!("swseq transaction error, SSFS={:#04x}", ssfsc & 0xff);
                    return Err(InternalError::Io("Software sequencing transaction error"));
                }

                return Ok(());
            }

            if deadline.expired() {
                return Err(InternalError::Io("Software sequencing timeout"));
            }

            spin_loop();
        }
    }

    /// Find the index of an opcode in the opcode table
    fn find_opcode_index(&self, opcode: u8) -> Option<usize> {
        self.opcodes
            .as_ref()?
            .opcode
            .iter()
            .position(|op| op.opcode == opcode)
    }

    /// Run an opcode using software sequencing (ICH9+)
    ///
    /// This is the core swseq execution function, equivalent to ich9_run_opcode in flashprog.
    ///
    /// # Arguments
    /// * `opcode` - The SPI opcode byte
    /// * `addr` - Address for address-type opcodes (ignored for no-address opcodes)
    /// * `data` - Data buffer (for writes: data to send, for reads: buffer to fill)
    /// * `is_write` - True if this is a write operation
    /// * `atomic` - 0 = none, 1 = use preop0, 2 = use preop1
    fn ich9_run_opcode(
        &self,
        opcode: u8,
        addr: u32,
        data: &mut [u8],
        is_write: bool,
        atomic: u8,
    ) -> Result<(), InternalError> {
        let datalength = data.len();
        if datalength > Self::SWSEQ_MAX_DATA {
            return Err(InternalError::Io("Data length exceeds swseq maximum"));
        }

        // Find opcode in table
        let opcode_index = self.find_opcode_index(opcode).ok_or_else(|| {
            log::debug!("Opcode {:#04x} not found in opcode table", opcode);
            InternalError::NotSupported("Opcode not in OPMENU table")
        })?;

        // Wait for any previous cycle to complete
        self.swseq_wait_idle(60_000)?; // 60ms timeout

        // Program address in FADDR (preserve reserved bits, clear 25th bit which is hwseq-only)
        let faddr = self.spibar.read32(ICH9_REG_FADDR) & !0x01ff_ffff;
        self.spibar
            .write32(ICH9_REG_FADDR, (addr & 0x00ff_ffff) | faddr);

        // Fill data registers for write commands
        if is_write && datalength > 0 {
            self.fill_data(data);
        }

        // Build SSFS+SSFC value
        let mut ssfsc = self.spibar.read32(self.swseq.ssfsc);
        // Keep only reserved bits
        ssfsc &= SSFS_RESERVED_MASK | SSFC_RESERVED_MASK;
        // Clear cycle done and error status (write-1-to-clear)
        ssfsc |= SSFS_FDONE | SSFS_FCERR;
        self.spibar.write32(self.swseq.ssfsc, ssfsc);

        // Use 20 MHz SPI clock
        ssfsc |= SSFC_SCF_20MHZ;

        // Set data byte count and data cycle bit
        if datalength > 0 {
            ssfsc |= SSFC_DS; // Data cycle
            ssfsc |= ((datalength - 1) as u32) << SSFC_DBC_OFF;
        }

        // Select opcode index
        ssfsc |= (opcode_index as u32 & 0x7) << SSFC_COP_OFF;

        // Handle atomic operations (preop + main op)
        let timeout_us = match atomic {
            2 => {
                ssfsc |= SSFC_SPOP; // Select second preop
                ssfsc |= SSFC_ACS; // Atomic cycle sequence
                60_000_000 // 60 seconds for chip erase
            }
            1 => {
                ssfsc |= SSFC_ACS; // Atomic cycle sequence (uses first preop)
                60_000_000 // 60 seconds for chip erase
            }
            _ => 60_000, // 60ms for normal operations
        };

        // Start the cycle
        ssfsc |= SSFC_SCGO;

        // Write it
        self.spibar.write32(self.swseq.ssfsc, ssfsc);

        // Wait for completion
        self.swseq_wait_complete(timeout_us)?;

        // Read data for read commands
        if !is_write && datalength > 0 {
            self.read_data(data);
        }

        Ok(())
    }

    // ========================================================================
    // ICH7 Software Sequencing Operations
    // ========================================================================

    /// Wait for ICH7 software sequencing cycle to not be in progress
    ///
    /// This should be called before starting a new cycle to ensure the
    /// previous one has completed.
    #[allow(clippy::unnecessary_cast)] // Casts needed for i686 where tv_sec/tv_nsec are i32
    fn ich7_swseq_wait_idle(&self, timeout_us: u32) -> Result<(), InternalError> {
        let mut deadline = Deadline::new(timeout_us);

        loop {
            let spis = self.spibar.read16(self.ich7_swseq.spis);

            if spis & SPIS_SCIP == 0 {
                return Ok(());
            }

            if deadline.expired() {
                return Err(InternalError::Io(
                    "ICH7: SCIP never cleared (swseq busy timeout)",
                ));
            }

            spin_loop();
        }
    }

    /// Wait for ICH7 software sequencing cycle to complete
    #[allow(clippy::unnecessary_cast)] // Casts needed for i686 where tv_sec/tv_nsec are i32
    fn ich7_swseq_wait_complete(&self, timeout_us: u32) -> Result<(), InternalError> {
        let done_or_err = SPIS_CDS | SPIS_FCERR;

        let mut deadline = Deadline::new(timeout_us);

        loop {
            let spis = self.spibar.read16(self.ich7_swseq.spis);

            if spis & done_or_err != 0 {
                // Check for error
                if spis & SPIS_FCERR != 0 {
                    log::debug!("ICH7 transaction error, SPIS={:#06x}", spis);
                    // Clear error by keeping reserved bits and writing 1 to error bit
                    let clear = (spis & SPIS_RESERVED_MASK) | SPIS_FCERR;
                    self.spibar.write16(self.ich7_swseq.spis, clear);
                    return Err(InternalError::Io(
                        "ICH7: Software sequencing transaction error",
                    ));
                }

                return Ok(());
            }

            if deadline.expired() {
                return Err(InternalError::Io("ICH7: Software sequencing timeout"));
            }

            spin_loop();
        }
    }

    /// Read data from ICH7 SPID registers
    ///
    /// Optimized to read full 32-bit words and extract bytes.
    #[inline(always)]
    fn ich7_read_data(&self, buf: &mut [u8]) {
        let len = buf.len();
        let mut offset = 0;

        // Process full 32-bit words
        while offset + 4 <= len {
            let temp = self.spibar.read32(self.ich7_swseq.spid0 + offset);
            buf[offset] = temp as u8;
            buf[offset + 1] = (temp >> 8) as u8;
            buf[offset + 2] = (temp >> 16) as u8;
            buf[offset + 3] = (temp >> 24) as u8;
            offset += 4;
        }

        // Handle remaining bytes (0-3)
        if offset < len {
            let temp = self.spibar.read32(self.ich7_swseq.spid0 + offset);
            let remaining = len - offset;
            if remaining > 0 {
                buf[offset] = temp as u8;
            }
            if remaining > 1 {
                buf[offset + 1] = (temp >> 8) as u8;
            }
            if remaining > 2 {
                buf[offset + 2] = (temp >> 16) as u8;
            }
        }
    }

    /// Fill ICH7 SPID registers with data
    #[inline(always)]
    fn ich7_fill_data(&self, data: &[u8]) {
        let len = data.len();
        if len == 0 {
            return;
        }

        let mut offset = 0;

        // Process full 32-bit words
        while offset + 4 <= len {
            let temp = (data[offset] as u32)
                | ((data[offset + 1] as u32) << 8)
                | ((data[offset + 2] as u32) << 16)
                | ((data[offset + 3] as u32) << 24);
            self.spibar.write32(self.ich7_swseq.spid0 + offset, temp);
            offset += 4;
        }

        // Handle remaining bytes (0-3)
        if offset < len {
            let mut temp: u32 = 0;
            let remaining = len - offset;
            if remaining > 0 {
                temp |= data[offset] as u32;
            }
            if remaining > 1 {
                temp |= (data[offset + 1] as u32) << 8;
            }
            if remaining > 2 {
                temp |= (data[offset + 2] as u32) << 16;
            }
            self.spibar.write32(self.ich7_swseq.spid0 + offset, temp);
        }
    }

    /// Run an opcode using ICH7 software sequencing
    ///
    /// This is the core swseq execution function for ICH7, equivalent to ich7_run_opcode
    /// in flashprog.
    ///
    /// # Arguments
    /// * `opcode` - The SPI opcode byte
    /// * `addr` - Address for address-type opcodes (ignored for no-address opcodes)
    /// * `data` - Data buffer (for writes: data to send, for reads: buffer to fill)
    /// * `is_write` - True if this is a write operation
    /// * `atomic` - 0 = none, 1 = use preop0, 2 = use preop1
    fn ich7_run_opcode(
        &self,
        opcode: u8,
        addr: u32,
        data: &mut [u8],
        is_write: bool,
        atomic: u8,
    ) -> Result<(), InternalError> {
        let datalength = data.len();
        if datalength > Self::SWSEQ_MAX_DATA {
            return Err(InternalError::Io("Data length exceeds swseq maximum"));
        }

        // Wait for any previous cycle to complete
        self.ich7_swseq_wait_idle(60_000)?; // 60ms timeout

        // Program offset in flash into SPIA while preserving reserved bits
        let spia = self.spibar.read32(self.ich7_swseq.spia) & !0x00ff_ffff;
        self.spibar
            .write32(self.ich7_swseq.spia, (addr & 0x00ff_ffff) | spia);

        // Fill data registers for write commands
        if is_write && datalength > 0 {
            self.ich7_fill_data(data);
        }

        // Assemble SPIS - clear status bits by writing 1 to them
        let mut spis = self.spibar.read16(self.ich7_swseq.spis);
        // Keep reserved bits
        spis &= SPIS_RESERVED_MASK;
        // Clear done and error status
        spis |= SPIS_CDS | SPIS_FCERR;
        self.spibar.write16(self.ich7_swseq.spis, spis);

        // Assemble SPIC
        let mut spic: u16 = 0;

        // Set data byte count (bits 8-13) and data cycle bit
        if datalength > 0 {
            spic |= SPIC_DS;
            // Data byte count is bits 8-13, max 63 bytes
            spic |= (((datalength - 1) & 0x3f) as u16) << 8;
        }

        // Find opcode in opmenu
        let opmenu_lo = self.spibar.read32(self.ich7_swseq.opmenu);
        let opmenu_hi = self.spibar.read32(self.ich7_swseq.opmenu + 4);
        let opmenu = (opmenu_lo as u64) | ((opmenu_hi as u64) << 32);

        let mut opcode_index = None;
        let mut opmenu_tmp = opmenu;
        for i in 0..8 {
            if (opmenu_tmp & 0xff) as u8 == opcode {
                opcode_index = Some(i);
                break;
            }
            opmenu_tmp >>= 8;
        }

        let opcode_index = opcode_index.ok_or_else(|| {
            log::debug!("ICH7: Opcode {:#04x} not found in opmenu", opcode);
            InternalError::NotSupported("Opcode not in OPMENU table")
        })?;

        // Select opcode (bits 4-6)
        spic |= ((opcode_index & 0x7) as u16) << 4;

        // Handle atomic operations (preop + main op)
        let timeout_us = match atomic {
            2 => {
                // Select second preop
                spic |= SPIC_SPOP;
                // Atomic command
                spic |= SPIC_ACS;
                60_000_000 // 60 seconds for chip erase
            }
            1 => {
                // Atomic command (uses first preop)
                spic |= SPIC_ACS;
                60_000_000 // 60 seconds for chip erase
            }
            _ => 60_000, // 60ms for normal operations
        };

        // Start the cycle
        spic |= SPIC_SCGO;

        // Write control register
        self.spibar.write16(self.ich7_swseq.spic, spic);

        // Wait for completion
        self.ich7_swseq_wait_complete(timeout_us)?;

        // Read data for read commands
        if !is_write && datalength > 0 {
            self.ich7_read_data(data);
        }

        Ok(())
    }

    /// Send a raw SPI command using ICH7 software sequencing.
    ///
    /// Only classified opcodes in OPMENU are accepted. Mutating commands
    /// enable BIOS writes first; unknown commands (including ambiguous 0x35)
    /// are rejected. Opcode meanings must match the connected flash.
    pub fn ich7_swseq_send_command(
        &mut self,
        writearr: &[u8],
        readarr: &mut [u8],
    ) -> Result<(), InternalError> {
        self.ich7_swseq_send_command_after(writearr, readarr, None, false)
    }

    /// Send a raw command, allowing unknown opcodes through the mutation gate.
    ///
    /// Known opcodes retain their normal classification; OPMENU and framing
    /// checks still apply. This does not bypass BIOS write protection.
    ///
    /// # Safety
    /// The caller must verify the command's meaning and effects on the connected
    /// flash and platform, including for opcodes classified as read-only.
    pub unsafe fn ich7_swseq_send_command_unchecked(
        &mut self,
        writearr: &[u8],
        readarr: &mut [u8],
    ) -> Result<(), InternalError> {
        self.ich7_swseq_send_command_after(writearr, readarr, None, true)
    }

    /// [`Self::ich7_swseq_send_command`] after an explicit `preop`.
    fn ich7_swseq_send_command_after(
        &mut self,
        writearr: &[u8],
        readarr: &mut [u8],
        preop: Option<u8>,
        allow_unknown: bool,
    ) -> Result<(), InternalError> {
        if writearr.is_empty() {
            return Err(InternalError::Io("Empty write array"));
        }

        let opcode = writearr[0];
        let writecnt = writearr.len();
        let readcnt = readarr.len();

        // Find opcode in table
        let opcode_index = self.find_opcode_index(opcode).ok_or_else(|| {
            log::debug!("ICH7: Opcode {:#04x} not found in opcode table", opcode);
            InternalError::NotSupported("Opcode not available in OPMENU")
        })?;

        // Invariant: init() always populates `opcodes` before the controller
        // is returned; find_opcode_index() above already returns an error if
        // it were somehow absent.
        let opcodes = self
            .opcodes
            .as_ref()
            .expect("opcodes initialized by IchSpiController::init");
        let spi_type = opcodes.opcode[opcode_index].spi_type;

        // Validate command format based on opcode type
        match spi_type {
            SPI_OPCODE_TYPE_READ_WITH_ADDRESS if writecnt != 4 => {
                return Err(InternalError::Io(
                    "Read with address requires exactly 4 write bytes",
                ));
            }
            SPI_OPCODE_TYPE_READ_NO_ADDRESS if writecnt != 1 => {
                return Err(InternalError::Io(
                    "Read without address requires exactly 1 write byte",
                ));
            }
            SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS if writecnt < 4 => {
                return Err(InternalError::Io(
                    "Write with address requires at least 4 write bytes",
                ));
            }
            SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS if readcnt > 0 => {
                return Err(InternalError::Io("Write commands cannot have read data"));
            }
            SPI_OPCODE_TYPE_WRITE_NO_ADDRESS if readcnt > 0 => {
                return Err(InternalError::Io("Write commands cannot have read data"));
            }
            _ => {}
        }

        // Extract address and data based on opcode type
        let (addr, data_slice, is_write): (u32, &[u8], bool) = match spi_type {
            SPI_OPCODE_TYPE_WRITE_NO_ADDRESS => {
                // Data starts after opcode
                (0, &writearr[1..], true)
            }
            SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS => {
                // Address is bytes 1-3, data is bytes 4+
                let addr = ((writearr[1] as u32) << 16)
                    | ((writearr[2] as u32) << 8)
                    | (writearr[3] as u32);
                (addr, &writearr[4..], true)
            }
            SPI_OPCODE_TYPE_READ_WITH_ADDRESS => {
                // Address is bytes 1-3
                let addr = ((writearr[1] as u32) << 16)
                    | ((writearr[2] as u32) << 8)
                    | (writearr[3] as u32);
                (addr, &[], false)
            }
            SPI_OPCODE_TYPE_READ_NO_ADDRESS => {
                // No address, just read data
                (0, &[], false)
            }
            _ => (0, &[], false),
        };

        // Determine if we need atomic mode (preop + main op)
        let atomic = if is_write {
            self.select_atomic(opcode, preop)?
        } else {
            0
        };

        // For read operations, we need to use the readarr
        // For write operations, we use the data from writearr
        self.gate_opcode(opcode, allow_unknown)?;
        if is_write {
            let mut data_buf: [u8; 64] = [0; 64];
            let len = data_slice.len().min(64);
            data_buf[..len].copy_from_slice(&data_slice[..len]);
            self.ich7_run_opcode(opcode, addr, &mut data_buf[..len], true, atomic)
        } else {
            let len = readcnt.min(64);
            self.ich7_run_opcode(opcode, addr, &mut readarr[..len], false, 0)
        }
    }

    /// Read data using ICH7 software sequencing
    pub fn ich7_swseq_read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), InternalError> {
        // Check we have a read opcode available
        let read_opcode = if self.find_opcode_index(JEDEC_READ).is_some() {
            JEDEC_READ
        } else if self.find_opcode_index(JEDEC_FAST_READ).is_some() {
            JEDEC_FAST_READ
        } else {
            return Err(InternalError::NotSupported("No read opcode available"));
        };

        let len = buf.len();
        let mut offset = 0;
        let mut current_addr = addr;

        while offset < len {
            // Avoid transfers that cross 4 KiB boundaries. The ICH SPI
            // controller is known to fail on some platforms when a single
            // transaction spans such a boundary; coreboot applies the same
            // restriction in its Intel common SPI driver.
            let remaining = len - offset;
            let boundary_remaining = 0x1000 - (current_addr as usize & 0xfff);
            let block_len = remaining.min(Self::SWSEQ_MAX_DATA).min(boundary_remaining);

            // Build write array: opcode + 3-byte address
            let writearr = [
                read_opcode,
                ((current_addr >> 16) & 0xff) as u8,
                ((current_addr >> 8) & 0xff) as u8,
                (current_addr & 0xff) as u8,
            ];

            self.ich7_swseq_send_command(&writearr, &mut buf[offset..offset + block_len])?;

            offset += block_len;
            current_addr += block_len as u32;
        }

        Ok(())
    }

    /// Write data using ICH7 software sequencing
    pub fn ich7_swseq_write(&mut self, addr: u32, data: &[u8]) -> Result<(), InternalError> {
        // Check we have a program opcode available
        if self.find_opcode_index(JEDEC_BYTE_PROGRAM).is_none() {
            return Err(InternalError::NotSupported("No program opcode available"));
        }

        let len = data.len();
        let mut offset = 0;
        let mut current_addr = addr;

        while offset < len {
            // Max 64 bytes per transfer, but also respect page boundaries (256 bytes)
            let remaining = len - offset;
            let page_remaining = 256 - (current_addr as usize & 0xff);
            let block_len = remaining.min(Self::SWSEQ_MAX_DATA).min(page_remaining);

            // Build write array: opcode + 3-byte address + data
            let mut writearr = [0u8; 68]; // 4 header + 64 data max
            writearr[0] = JEDEC_BYTE_PROGRAM;
            writearr[1] = ((current_addr >> 16) & 0xff) as u8;
            writearr[2] = ((current_addr >> 8) & 0xff) as u8;
            writearr[3] = (current_addr & 0xff) as u8;
            writearr[4..4 + block_len].copy_from_slice(&data[offset..offset + block_len]);

            // Send program command (WREN is handled automatically via atomic mode)
            self.ich7_swseq_send_command(&writearr[..4 + block_len], &mut [])?;

            // Wait for programming to complete by polling status register
            self.ich7_swseq_wait_wip()?;

            offset += block_len;
            current_addr += block_len as u32;
        }

        Ok(())
    }

    /// Erase a block using ICH7 software sequencing
    pub fn ich7_swseq_erase(&mut self, addr: u32, len: u32) -> Result<(), InternalError> {
        // Find an erase opcode - prefer 4KB sector erase for granularity
        let (erase_opcode, erase_size) = if self.find_opcode_index(JEDEC_SE).is_some() {
            (JEDEC_SE, 4096u32)
        } else if self.find_opcode_index(JEDEC_BE_52).is_some() {
            (JEDEC_BE_52, 32768u32)
        } else if self.find_opcode_index(JEDEC_BE_D8).is_some() {
            (JEDEC_BE_D8, 65536u32)
        } else {
            return Err(InternalError::NotSupported("No erase opcode available"));
        };

        // Verify alignment
        if addr & (erase_size - 1) != 0 || len & (erase_size - 1) != 0 {
            return Err(InternalError::Io("Erase address/length not aligned"));
        }

        let mut current_addr = addr;
        let end_addr = addr + len;

        while current_addr < end_addr {
            // Build erase command: opcode + 3-byte address
            let erase_arr = [
                erase_opcode,
                ((current_addr >> 16) & 0xff) as u8,
                ((current_addr >> 8) & 0xff) as u8,
                (current_addr & 0xff) as u8,
            ];
            self.ich7_swseq_send_command(&erase_arr, &mut [])?;

            // Wait for erase to complete
            self.ich7_swseq_wait_wip()?;

            current_addr += erase_size;
        }

        Ok(())
    }

    /// Wait for Write-In-Progress (WIP) bit to clear using ICH7 swseq
    fn ich7_swseq_wait_wip(&mut self) -> Result<(), InternalError> {
        // Check we have RDSR opcode
        if self.find_opcode_index(JEDEC_RDSR).is_none() {
            // No RDSR available, just wait a fixed time
            self.host.delay_us(100_000);
            return Ok(());
        }

        let rdsr_arr = [JEDEC_RDSR];
        let mut status = [0u8; 1];

        // Poll for up to 60 seconds (for chip erase)
        for _ in 0..600_000 {
            self.ich7_swseq_send_command(&rdsr_arr, &mut status)?;

            // WIP is bit 0
            if status[0] & 0x01 == 0 {
                return Ok(());
            }

            // Small delay between polls
            self.host.delay_us(100);
        }

        Err(InternalError::Io("Timeout waiting for WIP to clear"))
    }

    /// Send a raw SPI command using software sequencing
    ///
    /// This is the main public interface for swseq commands, implementing the
    /// same logic as ich_spi_send_command() in flashprog.
    ///
    /// # Arguments
    /// * `writearr` - Data to write (opcode + address + write data)
    /// * `readarr` - Buffer for read data
    ///
    /// The first byte of writearr is the opcode. For address-type commands,
    /// bytes 1-3 are the 24-bit address. Any remaining bytes are data to write.
    /// Only classified opcodes in OPMENU are accepted. Mutating commands enable
    /// BIOS writes first; unknown commands (including ambiguous 0x35) are
    /// rejected. Opcode meanings must match the connected flash.
    pub fn swseq_send_command(
        &mut self,
        writearr: &[u8],
        readarr: &mut [u8],
    ) -> Result<(), InternalError> {
        self.swseq_send_command_after(writearr, readarr, None, false)
    }

    /// Send a raw command, allowing unknown opcodes through the mutation gate.
    ///
    /// Known opcodes retain their normal classification; OPMENU and framing
    /// checks still apply. This does not bypass BIOS write protection.
    ///
    /// # Safety
    /// The caller must verify the command's meaning and effects on the connected
    /// flash and platform, including for opcodes classified as read-only.
    pub unsafe fn swseq_send_command_unchecked(
        &mut self,
        writearr: &[u8],
        readarr: &mut [u8],
    ) -> Result<(), InternalError> {
        self.swseq_send_command_after(writearr, readarr, None, true)
    }

    /// [`Self::swseq_send_command`] after an explicit `preop`.
    fn swseq_send_command_after(
        &mut self,
        writearr: &[u8],
        readarr: &mut [u8],
        preop: Option<u8>,
        allow_unknown: bool,
    ) -> Result<(), InternalError> {
        if writearr.is_empty() {
            return Err(InternalError::Io("Empty write array"));
        }

        let opcode = writearr[0];
        let writecnt = writearr.len();
        let readcnt = readarr.len();

        // Find opcode in table
        let opcode_index = self.find_opcode_index(opcode).ok_or_else(|| {
            log::debug!("Opcode {:#04x} not found in opcode table", opcode);
            InternalError::NotSupported("Opcode not available in OPMENU")
        })?;

        // Invariant: init() always populates `opcodes` before the controller
        // is returned; find_opcode_index() above already returns an error if
        // it were somehow absent.
        let opcodes = self
            .opcodes
            .as_ref()
            .expect("opcodes initialized by IchSpiController::init");
        let spi_type = opcodes.opcode[opcode_index].spi_type;

        // Validate command format based on opcode type
        match spi_type {
            SPI_OPCODE_TYPE_READ_WITH_ADDRESS if writecnt != 4 => {
                return Err(InternalError::Io(
                    "Read with address requires exactly 4 write bytes",
                ));
            }
            SPI_OPCODE_TYPE_READ_NO_ADDRESS if writecnt != 1 => {
                return Err(InternalError::Io(
                    "Read without address requires exactly 1 write byte",
                ));
            }
            SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS if writecnt < 4 => {
                return Err(InternalError::Io(
                    "Write with address requires at least 4 write bytes",
                ));
            }
            SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS if readcnt > 0 => {
                return Err(InternalError::Io("Write commands cannot have read data"));
            }
            SPI_OPCODE_TYPE_WRITE_NO_ADDRESS if readcnt > 0 => {
                return Err(InternalError::Io("Write commands cannot have read data"));
            }
            _ => {}
        }

        // Extract address and data based on opcode type
        let (addr, data_slice, is_write): (u32, &[u8], bool) = match spi_type {
            SPI_OPCODE_TYPE_WRITE_NO_ADDRESS => {
                // Data starts after opcode
                (0, &writearr[1..], true)
            }
            SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS => {
                // Address is bytes 1-3, data is bytes 4+
                let addr = ((writearr[1] as u32) << 16)
                    | ((writearr[2] as u32) << 8)
                    | (writearr[3] as u32);
                (addr, &writearr[4..], true)
            }
            SPI_OPCODE_TYPE_READ_WITH_ADDRESS => {
                // Address is bytes 1-3
                let addr = ((writearr[1] as u32) << 16)
                    | ((writearr[2] as u32) << 8)
                    | (writearr[3] as u32);
                (addr, &[], false)
            }
            SPI_OPCODE_TYPE_READ_NO_ADDRESS => {
                // No address, just read data
                (0, &[], false)
            }
            _ => (0, &[], false),
        };

        // Determine if we need atomic mode (preop + main op)
        // Write operations typically need WREN first
        let atomic = if is_write {
            self.select_atomic(opcode, preop)?
        } else {
            0
        };

        self.gate_opcode(opcode, allow_unknown)?;

        // For read operations, we need to use the readarr
        // For write operations, we use the data from writearr
        if is_write {
            // Create a mutable copy for the write path (though it won't be modified)
            let mut data_buf: [u8; 64] = [0; 64];
            let len = data_slice.len().min(64);
            data_buf[..len].copy_from_slice(&data_slice[..len]);
            self.ich9_run_opcode(opcode, addr, &mut data_buf[..len], true, atomic)
        } else {
            // Read operation - fill readarr
            let len = readcnt.min(64);
            self.ich9_run_opcode(opcode, addr, &mut readarr[..len], false, 0)
        }
    }

    /// Read data using software sequencing
    pub fn swseq_read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), InternalError> {
        // Check we have a read opcode available
        let read_opcode = if self.find_opcode_index(JEDEC_READ).is_some() {
            JEDEC_READ
        } else if self.find_opcode_index(JEDEC_FAST_READ).is_some() {
            JEDEC_FAST_READ
        } else {
            return Err(InternalError::NotSupported("No read opcode available"));
        };

        let len = buf.len();
        let mut offset = 0;
        let mut current_addr = addr;

        while offset < len {
            // Avoid transfers that cross 4 KiB boundaries. The ICH SPI
            // controller is known to fail on some platforms when a single
            // transaction spans such a boundary; coreboot applies the same
            // restriction in its Intel common SPI driver.
            let remaining = len - offset;
            let boundary_remaining = 0x1000 - (current_addr as usize & 0xfff);
            let block_len = remaining.min(Self::SWSEQ_MAX_DATA).min(boundary_remaining);

            // Build write array: opcode + 3-byte address
            let writearr = [
                read_opcode,
                ((current_addr >> 16) & 0xff) as u8,
                ((current_addr >> 8) & 0xff) as u8,
                (current_addr & 0xff) as u8,
            ];

            self.swseq_send_command(&writearr, &mut buf[offset..offset + block_len])?;

            offset += block_len;
            current_addr += block_len as u32;
        }

        Ok(())
    }

    /// Write data using software sequencing
    pub fn swseq_write(&mut self, addr: u32, data: &[u8]) -> Result<(), InternalError> {
        // Check we have a program opcode available
        if self.find_opcode_index(JEDEC_BYTE_PROGRAM).is_none() {
            return Err(InternalError::NotSupported("No program opcode available"));
        }

        let len = data.len();
        let mut offset = 0;
        let mut current_addr = addr;

        while offset < len {
            // Max 64 bytes per transfer, but also respect page boundaries (256 bytes)
            let remaining = len - offset;
            let page_remaining = 256 - (current_addr as usize & 0xff);
            let block_len = remaining.min(Self::SWSEQ_MAX_DATA).min(page_remaining);

            // Build write array: opcode + 3-byte address + data
            let mut writearr = [0u8; 68]; // 4 header + 64 data max
            writearr[0] = JEDEC_BYTE_PROGRAM;
            writearr[1] = ((current_addr >> 16) & 0xff) as u8;
            writearr[2] = ((current_addr >> 8) & 0xff) as u8;
            writearr[3] = (current_addr & 0xff) as u8;
            writearr[4..4 + block_len].copy_from_slice(&data[offset..offset + block_len]);

            // Send program command (WREN is handled automatically via atomic mode)
            self.swseq_send_command(&writearr[..4 + block_len], &mut [])?;

            // Wait for programming to complete by polling status register
            self.swseq_wait_wip()?;

            offset += block_len;
            current_addr += block_len as u32;
        }

        Ok(())
    }

    /// Erase a block using software sequencing
    pub fn swseq_erase(&mut self, addr: u32, len: u32) -> Result<(), InternalError> {
        // Find an erase opcode - prefer 4KB sector erase for granularity
        let (erase_opcode, erase_size) = if self.find_opcode_index(JEDEC_SE).is_some() {
            (JEDEC_SE, 4096u32)
        } else if self.find_opcode_index(JEDEC_BE_52).is_some() {
            (JEDEC_BE_52, 32768u32)
        } else if self.find_opcode_index(JEDEC_BE_D8).is_some() {
            (JEDEC_BE_D8, 65536u32)
        } else {
            return Err(InternalError::NotSupported("No erase opcode available"));
        };

        // Verify alignment
        if addr & (erase_size - 1) != 0 || len & (erase_size - 1) != 0 {
            return Err(InternalError::Io("Erase address/length not aligned"));
        }

        let mut current_addr = addr;
        let end_addr = addr + len;

        while current_addr < end_addr {
            // Build erase command: opcode + 3-byte address
            // WREN is handled automatically via atomic mode in swseq_send_command
            let erase_arr = [
                erase_opcode,
                ((current_addr >> 16) & 0xff) as u8,
                ((current_addr >> 8) & 0xff) as u8,
                (current_addr & 0xff) as u8,
            ];
            self.swseq_send_command(&erase_arr, &mut [])?;

            // Wait for erase to complete
            self.swseq_wait_wip()?;

            current_addr += erase_size;
        }

        Ok(())
    }

    /// Wait for Write-In-Progress (WIP) bit to clear
    fn swseq_wait_wip(&mut self) -> Result<(), InternalError> {
        // Check we have RDSR opcode
        if self.find_opcode_index(JEDEC_RDSR).is_none() {
            // No RDSR available, just wait a fixed time
            self.host.delay_us(100_000);
            return Ok(());
        }

        let rdsr_arr = [JEDEC_RDSR];
        let mut status = [0u8; 1];

        // Poll for up to 60 seconds (for chip erase)
        for _ in 0..600_000 {
            self.swseq_send_command(&rdsr_arr, &mut status)?;

            // WIP is bit 0
            if status[0] & 0x01 == 0 {
                return Ok(());
            }

            // Small delay between polls
            self.host.delay_us(100);
        }

        Err(InternalError::Io("Timeout waiting for WIP to clear"))
    }
}

#[cfg(any(all(feature = "std", target_os = "linux"), not(feature = "std")))]
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

    fn probe_opcode(&self, opcode: u8) -> bool {
        // In hwseq mode, no raw opcodes are available
        if self.mode == SpiMode::HardwareSequencing {
            return false;
        }

        // OPMENU opcodes, or WREN/EWSR sent atomically from the PREOP table.
        (Self::opcode_intent(opcode).is_some() && self.has_opcode(opcode))
            || ((opcode == JEDEC_WREN || opcode == JEDEC_EWSR) && self.find_preop(opcode).is_some())
    }
}

// Unsupported std non-Linux stub.
#[cfg(all(feature = "std", not(target_os = "linux")))]
pub struct IchSpiController {
    _private: (),
}

#[cfg(all(feature = "std", not(target_os = "linux")))]
impl IchSpiController {
    pub fn new(_chipset: &DetectedChipset, _mode: SpiMode) -> Result<Self, InternalError> {
        Err(InternalError::NotSupported(
            "Intel internal programmer not available on this target",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::tests::{FakeHost, FakeMmio};
    use alloc::vec;

    fn test_controller() -> IchSpiController<FakeHost> {
        IchSpiController {
            host: FakeHost::default(),
            spibar: FakeMmio::default(),
            generation: IchChipset::Series100SunrisePoint,
            config_bdf: PciAddress::new(0, 0, 0x1f, 0),
            locked: false,
            bios_writes_enabled: false,
            silvermont_bcr: false,
            restore_bcr_wpd: false,
            restore_pci_bwe: false,
            changes: [None; 12],
            change_count: 0,
            swseq_locked: false,
            desc_valid: false,
            requested_mode: SpiMode::HardwareSequencing,
            mode: SpiMode::HardwareSequencing,
            swseq: SwseqRegs {
                ssfsc: PCH100_REG_SSFSC,
                preop: PCH100_REG_PREOP,
                optype: PCH100_REG_OPTYPE,
                opmenu: PCH100_REG_OPMENU,
            },
            ich7_swseq: Ich7SwseqRegs {
                spis: ICH7_REG_SPIS,
                spic: ICH7_REG_SPIC,
                spia: ICH7_REG_SPIA,
                spid0: ICH7_REG_SPID0,
                preop: ICH7_REG_PREOP,
                optype: ICH7_REG_OPTYPE,
                opmenu: ICH7_REG_OPMENU,
            },
            hwseq: HwseqData {
                addr_mask: PCH100_FADDR_FLA,
                only_4k: true,
                hsfc_fcycle: PCH100_HSFC_FCYCLE,
                size_comp0: 0,
                size_comp1: 0,
            },
            opcodes: None,
            pending_preop: None,
            bbar: 0,
        }
    }

    #[test]
    fn init_read_and_drop_restores_menu_and_protection() {
        let mut c = test_controller();
        let mmio = c.spibar.clone();
        mmio.set32(ICH9_REG_HSFS, u32::from(HSFS_FDV));
        mmio.set32(PCH100_REG_PREOP, 0x1234);
        mmio.set32(PCH100_REG_FPR0, 0x8000_0001);
        c.wait_configuration_idle().unwrap();
        c.init().unwrap();
        c.hwseq_read(0, &mut [0; 4]).unwrap();
        drop(c);
        assert_eq!(mmio.read16(PCH100_REG_PREOP), 0x1234);
        assert_eq!(mmio.read32(PCH100_REG_FPR0), 0x8000_0001);
    }

    #[test]
    fn init_failure_restores_configuration_in_reverse() {
        let mut c = test_controller();
        c.requested_mode = SpiMode::HardwareSequencing; // descriptor invalid
        let mmio = c.spibar.clone();
        mmio.set32(PCH100_REG_PREOP, 0x1234);
        mmio.set32(PCH100_REG_FPR0, 0x8000_0001);
        assert!(c.init().is_err());
        drop(c);
        assert_eq!(mmio.read16(PCH100_REG_PREOP), 0x1234);
        assert_eq!(mmio.read32(PCH100_REG_FPR0), 0x8000_0001);
        let writes = mmio.writes();
        let pr = writes.iter().rposition(|w| w.0 == PCH100_REG_FPR0).unwrap();
        let preop = writes
            .iter()
            .rposition(|w| w.0 == PCH100_REG_PREOP)
            .unwrap();
        assert!(pr < preop);
    }

    #[test]
    fn restore_preserves_external_changes_and_bbar_unrelated_bits() {
        let mut c = test_controller();
        c.generation = IchChipset::Ich9;
        let mmio = c.spibar.clone();
        mmio.set32(ICH9_REG_BBAR, 0x123400 | 1);
        c.set_bbar(0);
        mmio.set32(ICH9_REG_BBAR, 2);
        mmio.set32(ICH9_REG_PR0, 0x8000_0001);
        c.set_pr(ICH9_REG_PR0, 0, false, false);
        mmio.set32(ICH9_REG_PR0, 0x0000_0002);
        drop(c);
        assert_eq!(mmio.read32(ICH9_REG_BBAR), 0x123402);
        assert_eq!(mmio.read32(ICH9_REG_PR0), 2);
    }

    #[test]
    fn individual_locks_only_block_their_own_restoration() {
        let mut c = test_controller();
        let mmio = c.spibar.clone();
        mmio.set32(PCH100_REG_FPR0, 0x8000_0001);
        mmio.set32(PCH100_REG_FPR0 + 4, 0x8000_0002);
        c.handle_protected_ranges();
        mmio.set32(PCH100_REG_DLOCK, DLOCK_PR0_LOCKDN | DLOCK_SSEQ_LOCKDN);
        drop(c);
        assert_eq!(mmio.read32(PCH100_REG_FPR0), 1);
        assert_eq!(mmio.read32(PCH100_REG_FPR0 + 4), 0x8000_0002);
    }

    #[test]
    fn restore_skips_locked_or_busy_controller() {
        for busy in [false, true] {
            let mut c = test_controller();
            let mmio = c.spibar.clone();
            mmio.set32(PCH100_REG_PREOP, 0x1234);
            c.change_register(PCH100_REG_PREOP, true, 0x5678, 0xffff);
            if busy {
                mmio.set32(PCH100_REG_SSFSC, SSFS_SCIP);
            } else {
                mmio.set32(ICH9_REG_HSFS, u32::from(HSFS_FLOCKDN));
            }
            drop(c);
            assert_eq!(mmio.read16(PCH100_REG_PREOP), 0x5678);
        }
    }

    #[test]
    fn drop_clears_owned_write_enable_even_when_busy() {
        for silvermont in [false, true] {
            for busy in [0, 1, 2] {
                let mut c = test_controller();
                c.silvermont_bcr = silvermont;
                c.host.enable_write8_readback();
                let host = c.host.clone();
                let mmio = c.spibar.clone();
                let bdf = PciAddress::new(0, 0, 0x1f, 0);
                host.set_config32(bdf, PCI_REG_BIOS_CNTL as u16 & !3, 0);
                mmio.set32(SILVERMONT_REG_BCR, 0);
                mmio.set32(PCH100_REG_PREOP, 0x1234);
                c.change_register(PCH100_REG_PREOP, true, 0x5678, 0xffff);
                c.enable_bios_write().unwrap();
                if busy == 1 {
                    mmio.set32(PCH100_REG_SSFSC, SSFS_SCIP | SSFS_FDONE | SSFS_FCERR);
                } else if busy == 2 {
                    mmio.set32(
                        ICH9_REG_HSFS,
                        u32::from(HSFS_SCIP | HSFS_FDONE | HSFS_FCERR),
                    );
                }
                let writes_before = mmio.writes().len();
                drop(c);
                assert_eq!(
                    mmio.read16(PCH100_REG_PREOP),
                    if busy == 0 { 0x1234 } else { 0x5678 }
                );
                if silvermont {
                    assert_eq!(
                        mmio.read32(SILVERMONT_REG_BCR) & u32::from(BIOS_CNTL_BWE),
                        0
                    );
                } else {
                    assert_eq!(
                        host.read8(bdf, PCI_REG_BIOS_CNTL as u16).unwrap() & BIOS_CNTL_BWE,
                        0
                    );
                    assert_eq!(
                        host.writes().last().unwrap().2 & u32::from(BIOS_CNTL_BWE),
                        0
                    );
                }
                assert!(
                    mmio.writes()[writes_before..]
                        .iter()
                        .all(
                            |&(offset, _, _)| offset != ICH9_REG_HSFS && offset != PCH100_REG_SSFSC
                        )
                );
            }
        }
    }

    #[test]
    fn direct_paths_gate_mutations_and_restore() {
        for ich7 in [false, true] {
            let mut c = test_controller();
            c.host.enable_write8_readback();
            c.host
                .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
            c.opcodes = Some(Opcodes::default());
            c.generation = if ich7 {
                IchChipset::Ich7
            } else {
                IchChipset::Ich9
            };
            let host = c.host.clone();
            // A refused command never enables writes, regardless of framing.
            c.opcodes.as_mut().unwrap().opcode[0].opcode = 0xaa;
            let send = |c: &mut IchSpiController<FakeHost>, cmd: &[u8], out: &mut [u8]| {
                if ich7 {
                    c.ich7_swseq_send_command(cmd, out)
                } else {
                    c.swseq_send_command(cmd, out)
                }
            };
            assert!(send(&mut c, &[0xaa, 0, 0, 0], &mut []).is_err());
            assert!(host.writes().is_empty());
            let _ = send(&mut c, &[JEDEC_RDSR], &mut [0]);
            assert!(host.writes().is_empty());
            // SMM protection is checked before issuing even a read-framed mutation.
            c.opcodes.as_mut().unwrap().opcode[0] = Opcode {
                opcode: JEDEC_CE_C7,
                spi_type: SPI_OPCODE_TYPE_READ_NO_ADDRESS,
                atomic: 0,
            };
            host.set_config32(
                c.config_bdf,
                PCI_REG_BIOS_CNTL as u16,
                u32::from(BIOS_CNTL_SMM_BWP),
            );
            assert!(matches!(
                send(&mut c, &[JEDEC_CE_C7], &mut [0]),
                Err(InternalError::AccessDenied { .. })
            ));
            assert!(host.writes().is_empty());
            host.set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
            // Cycle may fail in the fake, but enable must precede it and be restored.
            let _ = send(&mut c, &[JEDEC_CE_C7], &mut [0]);
            assert_eq!(host.writes().len(), 1);
            drop(c);
            assert_eq!(host.writes().len(), 2);
            assert_eq!(host.writes()[1].2 & 1, 0);
        }
    }

    #[test]
    fn locked_menu_identification_reads_and_ambiguous_opcode() {
        for ich7 in [false, true] {
            let mut c = test_controller();
            c.locked = true;
            c.mode = SpiMode::SoftwareSequencing;
            c.generation = if ich7 {
                IchChipset::Ich7
            } else {
                IchChipset::Ich9
            };
            c.opcodes = Some(Opcodes::default());
            let host = c.host.clone();
            let send = |c: &mut IchSpiController<FakeHost>, cmd: &[u8], out: &mut [u8]| {
                if ich7 {
                    c.ich7_swseq_send_command(cmd, out)
                } else {
                    c.swseq_send_command(cmd, out)
                }
            };
            for opcode in [JEDEC_RES, 0x4b] {
                c.opcodes.as_mut().unwrap().opcode[0] = Opcode {
                    opcode,
                    spi_type: SPI_OPCODE_TYPE_READ_WITH_ADDRESS,
                    atomic: 0,
                };
                c.spibar.set32(ICH7_REG_OPMENU, u32::from(opcode));
                assert!(c.probe_opcode(opcode));
                let writes_before = c.spibar.writes().len();
                // The fake may report a cycle error; verify the cycle was issued.
                let _ = send(&mut c, &[opcode, 0, 0, 0], &mut [0; 2]);
                let control = if ich7 {
                    ICH7_REG_SPIC
                } else {
                    PCH100_REG_SSFSC
                };
                assert!(
                    c.spibar.writes()[writes_before..]
                        .iter()
                        .any(|w| w.0 == control)
                );
                assert!(host.writes().is_empty());
            }
            c.opcodes.as_mut().unwrap().opcode[0].opcode = 0x35;
            assert!(!c.probe_opcode(0x35));
            assert!(matches!(
                send(&mut c, &[0x35, 0, 0, 0], &mut [0]),
                Err(InternalError::NotSupported("Unclassified SPI opcode"))
            ));
            assert!(host.writes().is_empty());
        }
    }

    #[test]
    fn unsafe_unknown_command_is_always_treated_as_mutating() {
        let mut c = test_controller();
        c.host.enable_write8_readback();
        c.host
            .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
        c.opcodes = Some(Opcodes::default());
        c.opcodes.as_mut().unwrap().opcode[0] = Opcode {
            opcode: 0xaa,
            spi_type: SPI_OPCODE_TYPE_READ_NO_ADDRESS,
            atomic: 0,
        };
        let host = c.host.clone();
        assert!(!c.probe_opcode(0xaa));
        // SAFETY: the fake controller has no physical flash.
        let _ = unsafe { c.swseq_send_command_unchecked(&[0xaa], &mut [0]) };
        assert_eq!(host.writes().len(), 1);
        drop(c);
        assert_eq!(host.writes().len(), 2);
    }

    #[test]
    fn ambiguous_opcode_unchecked_still_obeys_write_protection() {
        for ich7 in [false, true] {
            let mut c = test_controller();
            c.opcodes = Some(Opcodes::default());
            c.opcodes.as_mut().unwrap().opcode[0] = Opcode {
                opcode: 0x35,
                spi_type: SPI_OPCODE_TYPE_READ_NO_ADDRESS,
                atomic: 0,
            };
            c.host.set_config32(
                c.config_bdf,
                PCI_REG_BIOS_CNTL as u16,
                u32::from(BIOS_CNTL_SMM_BWP),
            );
            // SAFETY: the fake controller has no physical flash.
            let result = unsafe {
                if ich7 {
                    c.ich7_swseq_send_command_unchecked(&[0x35], &mut [0])
                } else {
                    c.swseq_send_command_unchecked(&[0x35], &mut [0])
                }
            };
            assert!(matches!(result, Err(InternalError::AccessDenied { .. })));
            assert!(c.host.writes().is_empty());
            assert!(c.spibar.writes().is_empty());
        }
    }

    #[test]
    fn failed_lazy_enable_is_restored_and_status_write_is_gated() {
        let mut c = test_controller();
        c.opcodes = Some(Opcodes::default());
        c.host
            .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
        let host = c.host.clone();
        assert!(c.swseq_send_command(&[JEDEC_WRSR, 0], &mut []).is_err());
        assert!(!c.writes_enabled());
        assert_eq!(host.writes().len(), 1);
        drop(c);
        assert_eq!(host.writes().len(), 2);
        assert_eq!(host.writes()[1].2 & 1, 0);
    }

    #[test]
    fn read_probe_and_hwseq_mutations_are_lazy() {
        let mut c = test_controller();
        c.host.enable_write8_readback();
        c.host
            .set_config32(c.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
        let host = c.host.clone();
        let shared: &IchSpiController<FakeHost> = &c;
        shared.hwseq_read(0, &mut [0; 1]).unwrap();
        assert!(!c.probe_opcode(JEDEC_READ));
        assert!(host.writes().is_empty());
        c.hwseq_write(0, &[0]).unwrap();
        c.hwseq_erase(0, 4096).unwrap();
        assert_eq!(host.writes().len(), 1);
        drop(c);
        assert_eq!(host.writes().len(), 2);
    }

    #[test]
    fn configuration_wait_polls_hardware_sequencer_without_status_writes() {
        let c = test_controller();
        c.spibar.set_hsfs_busy_reads(3);
        c.wait_configuration_idle().unwrap();
        assert!(c.spibar.writes().is_empty());
    }

    #[test]
    fn full_journal_never_makes_untracked_changes() {
        let mut c = test_controller();
        let mmio = c.spibar.clone();
        for i in 0..c.changes.len() {
            c.change_register(0x200 + i * 4, false, 1, u32::MAX);
        }
        c.change_register(0x300, false, 1, u32::MAX);
        assert_eq!(mmio.read32(0x300), 0);
        drop(c);
        for i in 0..12 {
            assert_eq!(mmio.read32(0x200 + i * 4), 0);
        }
    }

    #[test]
    fn descriptor_densities_and_controller_limits() {
        let mut c = test_controller();
        c.desc_valid = true;
        c.spibar.set32(0xb4, 0x20);
        // 8 MiB component, ignoring the unused second density field.
        c.spibar.set_descriptor(0xb4, 0x0ff0_a55a, 0, 0xf4);
        assert_eq!(c.descriptor_flash_size().unwrap(), Some(8 << 20));
        assert_eq!(c.spibar.read32(0xb4), 0x20);
        // 8 + 16 MiB, even if descriptor regions leave a trailing gap.
        c.spibar.set_descriptor(0xb4, 0x0ff0_a55a, 1 << 8, 0x54);
        assert_eq!(c.descriptor_flash_size().unwrap(), Some(24 << 20));
        c.spibar.set_descriptor(0xb4, 0x0ff0_a55a, 2 << 8, 0x54);
        assert!(c.descriptor_flash_size().is_err());
        c.spibar.set_descriptor(0xb4, 0x0ff0_a55a, 1 << 8, 0x84);
        assert!(c.descriptor_flash_size().is_err());
        c.spibar.set_descriptor(0xb4, 0, 0, 4);
        assert!(c.descriptor_flash_size().is_err());
        c.desc_valid = false;
        assert!(c.descriptor_flash_size().is_err());
        c.desc_valid = true;
        c.spibar.set_descriptor(0xb4, 0x0ff0_a55a, 1 << 8, 0x77);
        assert_eq!(c.descriptor_flash_size().unwrap(), Some(128 << 20));
        c.hwseq.addr_mask = ICH9_FADDR_FLA;
        assert!(c.descriptor_flash_size().is_err()); // 128 MiB > SPI limit

        c.generation = IchChipset::BayTrail;
        c.hwseq.addr_mask = ICH9_FADDR_FLA;
        c.spibar
            .set_descriptor(0xb0, 0x0ff0_a55a, 1 << 8, 4 | (5 << 3));
        assert_eq!(c.descriptor_flash_size().unwrap(), Some(24 << 20));
        c.spibar.set_descriptor(0xb0, 0x0ff0_a55a, 0, 6);
        assert!(c.descriptor_flash_size().is_err());
        c.generation = IchChipset::ApolloLake;
        assert_eq!(c.descriptor_flash_size().unwrap(), None);
    }

    #[test]
    fn swseq_writes_use_the_preop_sent_before_them() {
        let mut controller = test_controller();
        controller.mode = SpiMode::SoftwareSequencing;
        controller.opcodes = Some(Opcodes::default());
        assert!(controller.probe_opcode(JEDEC_WREN));
        assert!(controller.probe_opcode(JEDEC_EWSR));
        // SST25VF064C: EWSR + WRSR selects the EWSR slot, not WREN.
        assert_eq!(
            controller
                .select_atomic(JEDEC_WRSR, Some(JEDEC_EWSR))
                .unwrap(),
            2
        );
        assert_eq!(
            controller
                .select_atomic(JEDEC_WRSR, Some(JEDEC_WREN))
                .unwrap(),
            1
        );
        assert_eq!(controller.select_atomic(JEDEC_WRSR, None).unwrap(), 1);
        // A locked table without EWSR must not silently fall back to WREN.
        controller.opcodes.as_mut().unwrap().preop = [JEDEC_WREN, 0];
        assert!(!controller.probe_opcode(JEDEC_EWSR));
        assert!(
            controller
                .select_atomic(JEDEC_WRSR, Some(JEDEC_EWSR))
                .is_err()
        );
    }

    #[test]
    fn hwseq_rejects_unrepresentable_ranges_before_mmio() {
        for mask in [ICH9_FADDR_FLA, PCH100_FADDR_FLA] {
            let mut controller = test_controller();
            controller.hwseq.addr_mask = mask;
            assert!(controller.hwseq_check_range(mask, 1).is_ok());
            assert!(controller.hwseq_check_range(mask, 2).is_err());
            assert!(controller.hwseq_check_range(mask + 1, 0).is_err());
            assert!(controller.hwseq_read(mask, &mut [0; 2]).is_err());
            assert!(controller.hwseq_write(mask, &[0; 2]).is_err());
            assert!(controller.hwseq_erase(mask + 1, 4096).is_err());
            assert!(controller.hwseq_erase(mask - 4095, 8192).is_err());
        }

        let mut controller = test_controller();
        controller.hwseq.addr_mask = u32::MAX;
        assert!(controller.hwseq_check_range(u32::MAX, 1).is_ok());
        assert!(controller.hwseq_check_range(u32::MAX, 2).is_err());
        assert!(controller.hwseq_check_range(2, usize::MAX).is_err());
        assert!(controller.hwseq_check_range(0xffff_f000, 4096).is_ok());
    }

    #[test]
    fn berase_decodes_every_hardware_erase_size() {
        for (encoding, size) in [(0u16, 256u32), (1, 4096), (2, 8192), (3, 65536)] {
            // Other HSFS bits (status flags, FDOPSS, ...) must not disturb the decode.
            let hsfs = (encoding << HSFS_BERASE_OFF) | HSFS_FDONE | HSFS_SCIP | (1 << 13);
            assert_eq!(berase_block_size(hsfs), size);
        }
    }

    #[test]
    fn uniform_erase_size_handles_partitions() {
        const K: u32 = 1024;
        // One size everywhere, with or without a partition boundary.
        assert_eq!(uniform_erase_size(4 * K, 4 * K, 0), Some(4 * K));
        assert_eq!(uniform_erase_size(64 * K, 64 * K, 0x10_0000), Some(64 * K));
        // Different sizes: plan with the larger one if the partitions meet on
        // one of its blocks ...
        assert_eq!(uniform_erase_size(4 * K, 64 * K, 0x10_0000), Some(64 * K));
        assert_eq!(uniform_erase_size(64 * K, 4 * K, 0x10_0000), Some(64 * K));
        // ... and refuse when a block would straddle the boundary.
        assert_eq!(uniform_erase_size(4 * K, 64 * K, 0x10_1000), None);
    }

    /// An ICH9-style controller (BERASE-selected erase size) over a simulated
    /// sequencer whose partitions report `low`/`high` BERASE encodings.
    fn ich9_controller(low: u16, high: u16, fpb: u32) -> IchSpiController<FakeHost> {
        let mut controller = test_controller();
        controller.host.enable_write8_readback();
        controller
            .host
            .set_config32(controller.config_bdf, PCI_REG_BIOS_CNTL as u16, 0);
        controller.generation = IchChipset::Ich9;
        controller.hwseq = HwseqData {
            addr_mask: ICH9_FADDR_FLA,
            only_4k: false,
            hsfc_fcycle: HSFC_FCYCLE,
            size_comp0: 0,
            size_comp1: 0,
        };
        controller.spibar.set_berase(low, high);
        controller.spibar.set32(ICH9_REG_FPB, fpb);
        controller
    }

    #[cfg(all(feature = "std", target_os = "linux"))]
    #[test]
    fn hwseq_flash_plans_with_the_controller_block_size() {
        use crate::programmer::HwseqFlash;
        use futures_lite::future::block_on;
        use rflasher_core::flash::FlashDevice;

        let controller = ich9_controller(3, 3, 0);
        controller.host.set_config32(
            controller.config_bdf,
            PCI_REG_BIOS_CNTL as u16,
            u32::from(BIOS_CNTL_SMM_BWP),
        );
        let mut flash = HwseqFlash::with_size(Box::new(controller), 1 << 20).unwrap();
        assert_eq!(flash.erase_granularity(), 64 * 1024);
        assert_eq!(flash.erase_blocks()[0].total_size(), 1 << 20);
        // SMM write protection prevents the lazy enable.
        assert_eq!(
            block_on(flash.erase(0, 64 * 1024)),
            Err(CoreError::RegionProtected)
        );
        assert_eq!(
            block_on(flash.read(1 << 20, &mut [0])),
            Err(CoreError::AddressOutOfBounds)
        );
    }

    #[test]
    fn ich9_erase_follows_berase_instead_of_assuming_4k() {
        // BERASE = 3: every cycle erases 64 KiB.
        let mut controller = ich9_controller(3, 3, 0);
        assert_eq!(
            controller.hwseq_erase_geometry(16 << 20).unwrap(),
            64 * 1024
        );

        // A 4 KiB request would wipe 60 KiB of neighbouring data: refuse it,
        // and start no cycle.
        for (addr, len) in [
            (0x1000, 0x1000),
            (0, 0x1000),
            (0x10000, 0x8000),
            (0x8000, 0x10000),
        ] {
            assert!(
                controller.hwseq_erase(addr, len).is_err(),
                "erase({addr:#x}, {len:#x}) must be refused"
            );
        }
        assert!(controller.spibar.erase_cycles().is_empty());

        // Whole blocks are erased one cycle per block.
        controller.hwseq_erase(0x10000, 0x20000).unwrap();
        assert_eq!(controller.spibar.erase_cycles(), vec![0x10000, 0x20000]);
    }

    #[test]
    fn ich9_4k_erase_walks_every_block() {
        let mut controller = ich9_controller(1, 1, 0);
        assert_eq!(controller.hwseq_erase_geometry(8 << 20).unwrap(), 4096);
        controller.hwseq_erase(0x2000, 0x3000).unwrap();
        assert_eq!(
            controller.spibar.erase_cycles(),
            vec![0x2000, 0x3000, 0x4000]
        );
    }

    #[test]
    fn ich9_partitions_with_different_erase_sizes() {
        // FPB = 0x100 puts the boundary at 1 MiB: 4 KiB blocks below, 64 KiB above.
        let mut controller = ich9_controller(1, 3, 0x100);
        assert_eq!(controller.hwseq_erase_geometry(8 << 20).unwrap(), 64 * 1024);

        // Each partition erases in its own block size ...
        controller.hwseq_erase(0xF_F000, 0x1000).unwrap();
        controller.hwseq_erase(0x10_0000, 0x1_0000).unwrap();
        assert_eq!(controller.spibar.erase_cycles(), vec![0xF_F000, 0x10_0000]);
        // ... and a 4 KiB request in the 64 KiB partition is refused.
        assert!(controller.hwseq_erase(0x11_0000, 0x1000).is_err());

        // A boundary that does not fall on a 64 KiB block has no uniform plan.
        let controller = ich9_controller(1, 3, 0x101);
        assert!(controller.hwseq_erase_geometry(8 << 20).is_err());
    }

    #[test]
    fn pch100_erase_geometry_is_4k_without_touching_registers() {
        let controller = test_controller();
        assert_eq!(controller.hwseq_erase_geometry(16 << 20).unwrap(), 4096);
        assert_eq!(controller.hwseq_erase_block_size(0x12345), 4096);
    }

    #[test]
    fn hwseq_erase_rejects_partial_blocks_before_any_cycle() {
        let mut controller = test_controller();
        // PCH100+: 4 KiB blocks. None of these is a sequence of whole blocks.
        for (addr, len) in [(1, 4096), (0, 4095), (4096, 100), (2048, 4096)] {
            assert!(
                matches!(controller.hwseq_erase(addr, len), Err(InternalError::Io(_))),
                "erase({addr:#x}, {len:#x}) must be refused"
            );
        }
    }

    #[test]
    fn enable_bios_write_only_reports_verified_enable() {
        let mut controller = test_controller();
        let bdf = PciAddress::new(0, 0, 0x1f, 0);
        assert!(!controller.writes_enabled());

        // The fake host accepts writes without changing the PCI register.
        controller
            .host
            .set_config32(bdf, PCI_REG_BIOS_CNTL as u16, 0);
        assert!(matches!(
            controller.enable_bios_write(),
            Err(InternalError::ChipsetEnable(_))
        ));
        assert!(!controller.writes_enabled());

        controller
            .host
            .set_config32(bdf, PCI_REG_BIOS_CNTL as u16, BIOS_CNTL_BWE as u32);
        controller.enable_bios_write().unwrap();
        assert!(controller.writes_enabled());

        // SMM_BWP/EISS is cleared alongside BWE; only BWE decides success.
        let bios_cntl = BIOS_CNTL_SMM_BWP | BIOS_CNTL_BLE;
        controller
            .host
            .set_config32(bdf, PCI_REG_BIOS_CNTL as u16, bios_cntl as u32);
        assert!(matches!(
            controller.enable_bios_write(),
            Err(InternalError::ChipsetEnable(_))
        ));
        assert!(!controller.writes_enabled());
        let wanted = (BIOS_CNTL_BLE | BIOS_CNTL_BWE) as u32;
        assert_eq!(
            controller.host.writes().last(),
            Some(&(bdf, PCI_REG_BIOS_CNTL as u16, wanted, 1))
        );

        controller.host.set_config32(
            bdf,
            PCI_REG_BIOS_CNTL as u16,
            (bios_cntl | BIOS_CNTL_BWE) as u32,
        );
        controller.enable_bios_write().unwrap();
        assert!(controller.writes_enabled());
        let writes = controller.host.writes();
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[2], (bdf, PCI_REG_BIOS_CNTL as u16, wanted, 1));
    }
    use crate::intel_pci::INTEL_CHIPSETS;

    fn detected_with_generation(generation: IchChipset) -> DetectedChipset {
        let enable = INTEL_CHIPSETS
            .iter()
            .find(|chipset| chipset.chipset == generation && !is_visible_spi_id(chipset.device_id))
            .expect("Intel chipset table entry for generation");

        DetectedChipset {
            enable,
            domain: 0,
            bus: 0,
            device: 0x1f,
            function: 0,
            revision_id: 0,
        }
    }

    fn detected_id(id: u16) -> DetectedChipset {
        let enable = INTEL_CHIPSETS.iter().find(|e| e.device_id == id).unwrap();
        DetectedChipset {
            enable,
            domain: 3,
            bus: 7,
            device: 0x1f,
            function: 0,
            revision_id: 0,
        }
    }

    #[test]
    fn pch_spi_mapping_and_identity() {
        for (id, target, spi_id) in [
            (0x9d48, (0x1f, 5), 0x9d24),
            (0xa1c0, (0x1f, 5), 0xa1a4),
            (0xa3c8, (0x1f, 5), 0xa3a4),
            (0x5ae8, (0x0d, 2), 0x5a96),
            (0x31e8, (0x0d, 2), 0x3196),
        ] {
            let chipset = detected_id(id);
            let bdf = PciAddress::new(3, 7, target.0, target.1);
            let host = FakeHost::default();
            host.set_config32(bdf, 0, (spi_id << 16) | 0x8086);
            host.set_config32(bdf, 0x10, 0xfed1_c000);
            assert_eq!(intel_config_bdf(&chipset).unwrap(), bdf);
            assert_eq!(
                get_spibar_address_with_host(&host, &chipset).unwrap(),
                0xfed1_c000
            );
            host.set_config32(bdf, 0, 0x4b23_8086);
            assert!(get_spibar_address_with_host(&host, &chipset).is_err());
            assert!(host.writes().is_empty());
        }
        for id in [
            0xa1a4, 0xa324, 0x43a4, 0x4b24, 0x5a96, 0x3196, 0x7e23, 0xa823, 0xae23,
        ] {
            let mut chipset = detected_id(id);
            chipset.device = 12;
            chipset.function = 3;
            let bdf = PciAddress::new(3, 7, 12, 3);
            let host = FakeHost::default();
            host.set_config32(bdf, 0, (u32::from(id) << 16) | 0x8086);
            host.set_config32(bdf, 0x10, 0xfed1_c000);
            assert_eq!(intel_config_bdf(&chipset).unwrap(), bdf);
            assert_eq!(
                get_spibar_address_with_host(&host, &chipset).unwrap(),
                0xfed1_c000
            );
        }
        assert!(crate::intel_pci::find_chipset(0x8086, 0x4b23, None).is_none());
        for id in [0xa303, 0x4380] {
            assert!(intel_config_bdf(&detected_id(id)).is_err());
        }
    }

    #[test]
    fn missing_spi_refuses_before_writes() {
        let host = FakeHost::default();
        let chipset = detected_id(0x5ae8);
        assert!(matches!(
            get_spibar_address_with_host(&host, &chipset),
            Err(InternalError::ChipsetEnable(_))
        ));
        assert!(host.writes().is_empty());
        let mut denied = host.clone();
        denied.deny_reads();
        assert!(matches!(
            get_spibar_address_with_host(&denied, &chipset),
            Err(InternalError::ChipsetEnable(_))
        ));
        assert!(denied.writes().is_empty());
    }

    #[test]
    fn pch_bios_control_uses_and_restores_spi_bdf() {
        let mut host = FakeHost::default();
        host.enable_write8_readback();
        let chipset = detected_id(0x5ae8);
        let spi = PciAddress::new(3, 7, 0x0d, 2);
        host.set_config32(spi, 0, 0x5a96_8086);
        host.set_config32(spi, 0x10, 0xfed1_c000);
        host.set_config32(spi, 0xdc, 0);
        assert_eq!(
            get_spibar_address_with_host(&host, &chipset).unwrap(),
            0xfed1_c000
        );
        let mut controller = test_controller();
        controller.host = host.clone();
        controller.config_bdf = intel_config_bdf(&chipset).unwrap();
        controller.enable_bios_write().unwrap();
        drop(controller);
        let writes = host.writes();
        assert_eq!(writes.len(), 2);
        assert!(writes.iter().all(|w| w.0 == spi && w.1 == 0xdc));
        assert_eq!(writes[0].2 & 1, 1);
        assert_eq!(writes[1].2 & 1, 0);

        host.set_config32(spi, 0xdc, 0);
        let mut denied = host.clone();
        denied.deny_writes();
        let mut controller = test_controller();
        controller.host = denied;
        controller.config_bdf = spi;
        assert!(controller.enable_bios_write().is_err());
        assert!(!controller.writes_enabled());
        drop(controller);
        assert_eq!(host.writes().len(), 2);
    }

    #[test]
    fn test_spimode_parse_is_no_alloc_case_insensitive() {
        assert_eq!(SpiMode::parse("AUTO"), Some(SpiMode::Auto));
        assert_eq!(SpiMode::parse("HwSeq"), Some(SpiMode::HardwareSequencing));
        assert_eq!(
            SpiMode::parse("software"),
            Some(SpiMode::SoftwareSequencing)
        );
        assert_eq!(SpiMode::parse("invalid"), None);
    }

    #[test]
    fn test_get_spibar_address_with_host_pch100_hidden_spi_function() {
        let host = FakeHost::default();
        let chipset = detected_with_generation(IchChipset::Series100SunrisePoint);
        let spi_bdf = PciAddress::new(chipset.domain, chipset.bus, chipset.device, 5);
        host.set_config32(spi_bdf, 0, 0x9d24_8086);
        host.set_config32(spi_bdf, PCI_REG_SPIBAR as u16, 0xfed1_c000);

        let spibar = get_spibar_address_with_host(&host, &chipset).unwrap();

        assert_eq!(spibar, 0xfed1_c000);
    }

    #[test]
    fn test_get_spibar_address_with_host_rejects_all_ones_spibar() {
        let host = FakeHost::default();
        let chipset = detected_with_generation(IchChipset::Series100SunrisePoint);
        let spi_bdf = PciAddress::new(chipset.domain, chipset.bus, chipset.device, 5);
        host.set_config32(spi_bdf, 0, 0x9d24_8086);
        host.set_config32(spi_bdf, PCI_REG_SPIBAR as u16, u32::MAX);

        let err = get_spibar_address_with_host(&host, &chipset).unwrap_err();

        assert!(matches!(err, InternalError::ChipsetEnable(_)));
    }

    #[test]
    fn pch_spi_bar_rejects_io_reserved_and_unassigned() {
        let host = FakeHost::default();
        let chipset = detected_id(0xa324);
        let bdf = PciAddress::new(3, 7, 0x1f, 0);
        host.set_config32(bdf, 0, 0xa324_8086);
        for bar in [
            0xfed1_c001,
            0xfed1_c002,
            0xfed1_c004,
            0,
            0xffff_f000,
            u32::MAX,
        ] {
            host.set_config32(bdf, 0x10, bar);
            assert!(
                get_spibar_address_with_host(&host, &chipset).is_err(),
                "BAR {bar:#x}"
            );
        }
        assert!(host.writes().is_empty());
        assert!(crate::intel_pci::find_chipset(0x8086, 0xa124, None).is_none());
    }

    #[test]
    fn silvermont_sbase_discovery_and_errors() {
        let host = FakeHost::default();
        let chipset = detected_with_generation(IchChipset::BayTrail);
        let bdf = PciAddress::new(
            chipset.domain,
            chipset.bus,
            chipset.device,
            chipset.function,
        );
        assert!(matches!(
            get_spibar_address_with_host(&host, &chipset),
            Err(InternalError::PciAccess(_))
        ));
        for value in [0, u32::MAX] {
            host.set_config32(bdf, 0x54, value);
            assert!(matches!(
                get_spibar_address_with_host(&host, &chipset),
                Err(InternalError::ChipsetEnable(_))
            ));
        }
        host.set_config32(bdf, 0x54, 0xfed0_13ff);
        assert_eq!(
            get_spibar_address_with_host(&host, &chipset).unwrap(),
            0xfed0_1200
        );
    }

    #[test]
    fn silvermont_device_scope_includes_braswell() {
        for id in [0x0f1c, 0x1f38, 0x229c] {
            let enable = INTEL_CHIPSETS
                .iter()
                .find(|entry| entry.device_id == id)
                .unwrap();
            let chipset = DetectedChipset {
                enable,
                ..detected_with_generation(IchChipset::BayTrail)
            };
            assert!(uses_silvermont_sbase(&chipset));
            let host = FakeHost::default();
            let bdf = PciAddress::new(
                chipset.domain,
                chipset.bus,
                chipset.device,
                chipset.function,
            );
            host.set_config32(bdf, 0x54, 0xfed0_1002);
            assert_eq!(
                get_spibar_address_with_host(&host, &chipset).unwrap(),
                0xfed0_1000
            );
        }
    }

    #[test]
    fn silvermont_bcr_enable_and_restore() {
        let mut controller = test_controller();
        controller.silvermont_bcr = true;
        let mmio = controller.spibar.clone();
        mmio.set32(0xfc, 0x1234_5604);
        controller.enable_bios_write().unwrap();
        controller.enable_bios_write().unwrap();
        assert_eq!(mmio.read32(0xfc), 0x1234_5605);
        // Other bits can change during the controller lifetime.
        mmio.set32(0xfc, 0x1234_5645);
        drop(controller);
        assert_eq!(mmio.read32(0xfc), 0x1234_5644);
        assert_eq!(
            mmio.writes(),
            vec![(0xfc, 0x1234_5605, 4), (0xfc, 0x1234_5644, 4)]
        );
    }

    #[test]
    fn silvermont_bcr_existing_enable_and_access_failures() {
        let mut controller = test_controller();
        controller.silvermont_bcr = true;
        let mmio = controller.spibar.clone();
        mmio.set32(0xfc, 1);
        controller.enable_bios_write().unwrap();
        drop(controller);
        assert!(mmio.writes().is_empty());

        let mut controller = test_controller();
        controller.silvermont_bcr = true;
        let mmio = controller.spibar.clone();
        mmio.set32(0xfc, u32::from(BIOS_CNTL_SMM_BWP));
        assert!(matches!(
            controller.enable_bios_write(),
            Err(InternalError::AccessDenied { .. })
        ));
        drop(controller);
        assert!(mmio.writes().is_empty());

        let mut controller = test_controller();
        controller.silvermont_bcr = true;
        controller.spibar.block_write32(0xfc);
        let mmio = controller.spibar.clone();
        assert!(matches!(
            controller.enable_bios_write(),
            Err(InternalError::ChipsetEnable(_))
        ));
        drop(controller);
        assert_eq!(mmio.writes(), vec![(0xfc, 1, 4), (0xfc, 0, 4)]);
    }

    #[test]
    fn test_get_spibar_address_with_host_ich7_legacy_rcba() {
        let host = FakeHost::default();
        let chipset = detected_with_generation(IchChipset::Ich7);
        let lpc_bdf = PciAddress::new(
            chipset.domain,
            chipset.bus,
            chipset.device,
            chipset.function,
        );
        host.set_config32(lpc_bdf, PCI_REG_RCBA as u16, 0xfed1_8001);

        let spibar = get_spibar_address_with_host(&host, &chipset).unwrap();

        assert_eq!(spibar, 0xfed1_8000 + RCBA_SPI_OFFSET_ICH7 as u64);
    }

    #[test]
    fn test_get_spibar_address_with_host_ich8_uses_ich7_rcba_offset() {
        let host = FakeHost::default();
        let chipset = detected_with_generation(IchChipset::Ich8);
        let lpc_bdf = PciAddress::new(
            chipset.domain,
            chipset.bus,
            chipset.device,
            chipset.function,
        );
        host.set_config32(lpc_bdf, PCI_REG_RCBA as u16, 0xfed1_c001);

        let spibar = get_spibar_address_with_host(&host, &chipset).unwrap();

        assert_eq!(spibar, 0xfed1_c000 + RCBA_SPI_OFFSET_ICH7 as u64);
    }

    #[test]
    fn test_get_spibar_address_with_host_ich9_uses_ich9_rcba_offset() {
        let host = FakeHost::default();
        let chipset = detected_with_generation(IchChipset::Ich9);
        let lpc_bdf = PciAddress::new(
            chipset.domain,
            chipset.bus,
            chipset.device,
            chipset.function,
        );
        host.set_config32(lpc_bdf, PCI_REG_RCBA as u16, 0xfed1_c001);

        let spibar = get_spibar_address_with_host(&host, &chipset).unwrap();

        assert_eq!(spibar, 0xfed1_c000 + RCBA_SPI_OFFSET_ICH9 as u64);
    }

    #[test]
    fn test_get_spibar_address_with_host_rejects_disabled_rcba() {
        let host = FakeHost::default();
        let chipset = detected_with_generation(IchChipset::Ich7);
        let lpc_bdf = PciAddress::new(
            chipset.domain,
            chipset.bus,
            chipset.device,
            chipset.function,
        );
        host.set_config32(lpc_bdf, PCI_REG_RCBA as u16, 0xfed1_8000);

        let err = get_spibar_address_with_host(&host, &chipset).unwrap_err();

        assert!(matches!(
            err,
            InternalError::ChipsetEnable("RCBA not enabled")
        ));
    }
}

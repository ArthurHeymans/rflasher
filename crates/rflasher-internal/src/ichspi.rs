//! Intel ICH/PCH SPI Controller Driver
//!
//! This module implements the SPI controller driver for Intel ICH/PCH chipsets.
//! It supports both hardware sequencing (hwseq) and software sequencing (swseq)
//! modes.
//!
//! # Supported Chipsets
//!
//! - ICH7: Original SPI controller (swseq only)
//! - ICH8-ICH10: Hardware sequencing introduced
//! - 5-9 Series (Ibex Peak through Wildcat Point)
//! - 100+ Series (Sunrise Point and later): New register layout
//!
//! # Operating Modes
//!
//! - **Hardware Sequencing**: The SPI controller handles read/write/erase
//!   operations internally. This is the default for PCH100+.
//! - **Software Sequencing**: We control the SPI protocol directly.
//!   More flexible but may not be available on locked-down systems.

use crate::chipset::IchChipset;
use crate::error::InternalError;
use crate::ich_regs::*;
use crate::pci::{
    pci_read_config32, pci_read_config32_direct, pci_read_config8, pci_write_config8,
};
use crate::physmap::PhysMap;
use crate::DetectedChipset;

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
        match s.to_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "hwseq" | "hardware" | "hw" => Some(Self::HardwareSequencing),
            "swseq" | "software" | "sw" => Some(Self::SoftwareSequencing),
            _ => None,
        }
    }
}

impl std::fmt::Display for SpiMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "auto"),
            Self::HardwareSequencing => write!(f, "hwseq"),
            Self::SoftwareSequencing => write!(f, "swseq"),
        }
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

/// Intel ICH/PCH SPI Controller
#[cfg(all(feature = "std", target_os = "linux"))]
pub struct IchSpiController {
    /// Memory-mapped SPI registers
    spibar: PhysMap,
    /// Chipset generation
    generation: IchChipset,
    /// PCI location of LPC/eSPI bridge
    lpc_bus: u8,
    lpc_device: u8,
    lpc_function: u8,
    /// Whether configuration is locked (HSFS.FLOCKDN)
    locked: bool,
    /// Whether software sequencing is locked (DLOCK.SSEQ_LOCKDN on PCH100+)
    swseq_locked: bool,
    /// Flash descriptor valid
    desc_valid: bool,
    /// Requested operating mode (from user)
    requested_mode: SpiMode,
    /// Actual operating mode (after validation)
    mode: SpiMode,
    /// Software sequencing registers
    swseq: SwseqRegs,
    /// Hardware sequencing data
    hwseq: HwseqData,
    /// Current opcodes (for software sequencing)
    opcodes: Option<Opcodes>,
    /// BBAR value
    bbar: u32,
}

#[cfg(all(feature = "std", target_os = "linux"))]
impl IchSpiController {
    /// Initialize a new SPI controller for the detected chipset
    pub fn new(chipset: &DetectedChipset, mode: SpiMode) -> Result<Self, InternalError> {
        let generation = chipset.chipset_type();

        // Get SPI BAR address
        let spibar_addr = Self::get_spibar_address(chipset)?;
        log::debug!("SPI BAR at physical address: {:#x}", spibar_addr);

        // Map the SPI registers
        let spibar = PhysMap::new(spibar_addr, 0x200)?;

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

        let mut controller = Self {
            spibar,
            generation,
            lpc_bus: chipset.bus,
            lpc_device: chipset.device,
            lpc_function: chipset.function,
            locked: false,
            swseq_locked: false,
            desc_valid: false,
            requested_mode: mode,
            mode: SpiMode::Auto, // Will be determined during init
            swseq,
            hwseq,
            opcodes: None,
            bbar: 0,
        };

        // Initialize the controller
        controller.init()?;

        Ok(controller)
    }

    /// Get the SPI BAR physical address from PCI config space
    fn get_spibar_address(chipset: &DetectedChipset) -> Result<u64, InternalError> {
        let gen = chipset.chipset_type();

        if gen.is_pch100_compatible() {
            // PCH100+ (Sunrise Point and later): SPI controller is a separate PCI device
            // at function 5 (00:1f.5), not part of the LPC bridge at function 0.
            // The chipset detection finds the LPC bridge, but we need to read BAR0
            // from the SPI controller device.
            //
            // IMPORTANT: The SPI device is often hidden by firmware (vendor/device IDs
            // read as 0xFFFF), so it doesn't appear in sysfs. We must use direct I/O
            // port access (PCI Configuration Mechanism 1) to read from it.
            const SPI_FUNCTION: u8 = 5;

            // Use direct I/O port access since the SPI device may be hidden
            let spibar_raw = pci_read_config32_direct(
                chipset.bus,
                chipset.device,
                SPI_FUNCTION,
                PCI_REG_SPIBAR,
            )?;

            // SPIBAR is a 32-bit memory BAR. Mask off the lower 12 bits (BAR type indicators)
            // to get the physical address (4KB aligned).
            let addr = (spibar_raw & 0xFFFF_F000) as u64;

            log::debug!(
                "Raw SPIBAR register: {:#010x}, masked addr: {:#010x}",
                spibar_raw,
                addr
            );

            if addr == 0 {
                // SPIBAR is 0 - the SPI device may be hidden or disabled
                // Note: RCBA does NOT exist on PCH100+, so we cannot fall back to it
                return Err(InternalError::ChipsetEnable(
                    "SPIBAR is 0 - SPI controller may be hidden or disabled by firmware",
                ));
            }

            log::debug!(
                "Read SPIBAR {:#x} from PCI {:02x}:{:02x}.{} (via direct I/O)",
                addr,
                chipset.bus,
                chipset.device,
                SPI_FUNCTION
            );

            Ok(addr)
        } else if gen.is_ich9_compatible() || gen == IchChipset::Ich7 {
            // ICH7-ICH10, 5-9 Series: SPI is at an offset within RCBA
            Self::get_spibar_via_rcba(chipset)
        } else {
            Err(InternalError::NotSupported(
                "Unsupported chipset generation",
            ))
        }
    }

    /// Get SPI BAR via RCBA (Root Complex Base Address)
    fn get_spibar_via_rcba(chipset: &DetectedChipset) -> Result<u64, InternalError> {
        // Read RCBA from LPC bridge config space
        let rcba = pci_read_config32(chipset.bus, chipset.device, chipset.function, PCI_REG_RCBA)?;

        // Check if RCBA is enabled (bit 0)
        if rcba & 1 == 0 {
            return Err(InternalError::ChipsetEnable("RCBA not enabled"));
        }

        // RCBA is 32-bit aligned, mask off lower bits
        let rcba_base = (rcba & !0x3FFF) as u64;

        // SPI offset depends on chipset generation
        let spi_offset = if chipset.chipset_type() == IchChipset::Ich7 {
            RCBA_SPI_OFFSET_ICH7
        } else {
            RCBA_SPI_OFFSET_ICH9
        };

        Ok(rcba_base + spi_offset as u64)
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
    fn init_ich7(&mut self) -> Result<(), InternalError> {
        let spis = self.spibar.read16(ICH7_REG_SPIS);
        log::debug!("ICH7 SPIS: {:#06x}", spis);

        // Check for lockdown
        if spis & (1 << 15) != 0 {
            log::warn!("SPI Configuration Lockdown activated");
            self.locked = true;
        }

        self.bbar = self.spibar.read32(0x50);
        log::debug!("ICH7 BBAR: {:#010x}", self.bbar);

        // Initialize opcodes
        self.init_opcodes()?;

        // Try to set BBAR to 0
        if !self.locked {
            self.set_bbar(0);
        }

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
        if self.locked {
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
    fn program_opcodes(&self, opcodes: &Opcodes) -> Result<(), InternalError> {
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

        self.spibar.write16(self.swseq.preop, preop);
        self.spibar.write16(self.swseq.optype, optype);
        self.spibar.write32(self.swseq.opmenu, opmenu_lo);
        self.spibar.write32(self.swseq.opmenu + 4, opmenu_hi);

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
            // Try to clear protection if not locked
            if !self.locked {
                self.set_pr(reg_pr0, i, false, false);
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
    fn set_pr(&self, reg_pr0: usize, index: usize, read_prot: bool, write_prot: bool) {
        let addr = reg_pr0 + index * 4;
        let mut pr = self.spibar.read32(addr);

        pr &= !((1 << PR_RP_OFF) | (1 << PR_WP_OFF));
        if read_prot {
            pr |= 1 << PR_RP_OFF;
        }
        if write_prot {
            pr |= 1 << PR_WP_OFF;
        }

        self.spibar.write32(addr, pr);
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
        self.spibar.write32(bbar_off, bbar);

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
                    "Hardware sequencing requested but not supported on {} (ICH7 only supports swseq)",
                    self.generation
                );
                return Err(InternalError::NotSupported(
                    "Hardware sequencing not available on ICH7",
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
                // ICH7: swseq only
                log::debug!("Using swseq (ICH7 has no hwseq support)");
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
    /// This is useful for the SpiMaster trait's `probe_opcode` method.
    pub fn has_opcode(&self, opcode: u8) -> bool {
        self.find_opcode_index(opcode).is_some()
    }

    /// Get the opcode type for an opcode in the table
    ///
    /// Returns the SPI opcode type (read/write, with/without address) if found.
    pub fn get_opcode_type(&self, opcode: u8) -> Option<u8> {
        let idx = self.find_opcode_index(opcode)?;
        self.opcodes.as_ref().map(|ops| ops.opcode[idx].spi_type)
    }

    /// Find if an opcode is in the preop table
    ///
    /// Returns the preop index (0 or 1) if found, None otherwise.
    /// This is used to detect if a command is a preop like WREN or EWSR.
    #[allow(dead_code)]
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
            | JEDEC_WRSR // 0x01 - Write Status Register
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

    /// Enable BIOS write access via BIOS_CNTL register
    pub fn enable_bios_write(&mut self) -> Result<(), InternalError> {
        let bios_cntl = pci_read_config8(
            self.lpc_bus,
            self.lpc_device,
            self.lpc_function,
            PCI_REG_BIOS_CNTL,
        )?;

        log::debug!("BIOS_CNTL: {:#04x}", bios_cntl);

        // Check if BIOS Lock Enable is set
        if bios_cntl & BIOS_CNTL_BLE != 0 {
            log::warn!("BIOS Lock Enable (BLE) is set - writes may trigger SMI");
        }

        // Check if SMM BIOS Write Protect is set
        if bios_cntl & BIOS_CNTL_SMM_BWP != 0 {
            log::warn!("SMM BIOS Write Protect is set - cannot enable writes");
            return Err(InternalError::AccessDenied { region: "BIOS" });
        }

        // Enable BIOS Write Enable
        if bios_cntl & BIOS_CNTL_BWE == 0 {
            let new_val = bios_cntl | BIOS_CNTL_BWE;
            pci_write_config8(
                self.lpc_bus,
                self.lpc_device,
                self.lpc_function,
                PCI_REG_BIOS_CNTL,
                new_val,
            )?;

            // Verify
            let verify = pci_read_config8(
                self.lpc_bus,
                self.lpc_device,
                self.lpc_function,
                PCI_REG_BIOS_CNTL,
            )?;

            if verify & BIOS_CNTL_BWE == 0 {
                log::error!("Failed to enable BIOS Write Enable");
                return Err(InternalError::ChipsetEnable("Cannot enable BIOS writes"));
            }

            log::info!("BIOS Write Enable activated");
        } else {
            log::debug!("BIOS Write Enable already active");
        }

        Ok(())
    }

    /// Get the current operating mode
    pub fn mode(&self) -> SpiMode {
        self.mode
    }

    /// Get the requested operating mode (from user)
    pub fn requested_mode(&self) -> SpiMode {
        self.requested_mode
    }

    /// Check if the controller is locked (HSFS.FLOCKDN)
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Check if software sequencing is locked (DLOCK.SSEQ_LOCKDN on PCH100+)
    pub fn is_swseq_locked(&self) -> bool {
        self.swseq_locked
    }

    /// Check if hardware sequencing is available
    ///
    /// Returns true if hwseq can be used (supported by chipset and descriptor valid)
    pub fn is_hwseq_available(&self) -> bool {
        self.generation.supports_hwseq() && self.desc_valid
    }

    /// Check if software sequencing is available
    ///
    /// Returns true if swseq can be used (not locked by DLOCK.SSEQ_LOCKDN)
    pub fn is_swseq_available(&self) -> bool {
        !self.swseq_locked
    }

    /// Check if flash descriptor is valid
    pub fn has_valid_descriptor(&self) -> bool {
        self.desc_valid
    }

    /// Get the chipset generation
    pub fn generation(&self) -> IchChipset {
        self.generation
    }

    // ========================================================================
    // Hardware Sequencing Operations
    // ========================================================================

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
    fn hwseq_wait_for_cycle(&self, timeout_us: u32) -> Result<(), InternalError> {
        let done_or_err = HSFS_FDONE | HSFS_FCERR;

        // Get start time
        let mut start = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut start);
        }

        let timeout_ns = (timeout_us as i64) * 1000;
        let end_nsec = start.tv_nsec + timeout_ns;
        let end_sec = start.tv_sec + (end_nsec / 1_000_000_000);
        let end_nsec = end_nsec % 1_000_000_000;

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

            // Check timeout using clock_gettime (busy loop for precision)
            let mut now = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            unsafe {
                libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now);
            }

            if now.tv_sec > end_sec || (now.tv_sec == end_sec && now.tv_nsec >= end_nsec) {
                return Err(InternalError::Io("Hardware sequencing timeout"));
            }

            // Hint to CPU that we're spinning (reduces power, improves perf on hyperthreaded cores)
            std::hint::spin_loop();
        }
    }

    /// Read data using hardware sequencing
    ///
    /// This is the main read path - optimized for throughput.
    pub fn hwseq_read(&self, addr: u32, buf: &mut [u8]) -> Result<(), InternalError> {
        let len = buf.len();
        if len == 0 {
            return Ok(());
        }

        let mut offset = 0;
        let mut current_addr = addr;

        // Clear FDONE, FCERR, AEL by writing 1s to them (do once at start)
        self.spibar
            .write16(ICH9_REG_HSFS, self.spibar.read16(ICH9_REG_HSFS));

        while offset < len {
            // Calculate block size (max 64 bytes, respect 256-byte page boundaries)
            let remaining = len - offset;
            let page_remaining = 256 - (current_addr as usize & 0xFF);
            let block_len = remaining.min(HWSEQ_MAX_DATA).min(page_remaining);

            self.hwseq_set_addr(current_addr);

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
            current_addr += block_len as u32;
        }

        Ok(())
    }

    /// Write data using hardware sequencing
    ///
    /// This is the main write path - optimized for throughput.
    pub fn hwseq_write(&self, addr: u32, data: &[u8]) -> Result<(), InternalError> {
        let len = data.len();
        if len == 0 {
            return Ok(());
        }

        let mut offset = 0;
        let mut current_addr = addr;

        // Clear FDONE, FCERR, AEL by writing 1s to them (do once at start)
        self.spibar
            .write16(ICH9_REG_HSFS, self.spibar.read16(ICH9_REG_HSFS));

        while offset < len {
            // Calculate block size (max 64 bytes, respect 256-byte page boundaries)
            let remaining = len - offset;
            let page_remaining = 256 - (current_addr as usize & 0xFF);
            let block_len = remaining.min(HWSEQ_MAX_DATA).min(page_remaining);

            self.hwseq_set_addr(current_addr);

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
            current_addr += block_len as u32;
        }

        Ok(())
    }

    /// Erase a block using hardware sequencing
    pub fn hwseq_erase(&self, addr: u32, len: u32) -> Result<(), InternalError> {
        // Verify alignment (hwseq always uses 4KB blocks on PCH100+)
        let erase_size: u32 = if self.hwseq.only_4k {
            4096
        } else {
            // TODO: Read actual erase size from BERASE bits
            4096
        };

        if addr & (erase_size - 1) != 0 || len & (erase_size - 1) != 0 {
            return Err(InternalError::Io(
                "Erase address/length not aligned to erase block size",
            ));
        }

        let mut current_addr = addr;
        let end_addr = addr + len;

        // Clear FDONE, FCERR, AEL by writing 1s to them (do once at start)
        self.spibar
            .write16(ICH9_REG_HSFS, self.spibar.read16(ICH9_REG_HSFS));

        while current_addr < end_addr {
            self.hwseq_set_addr(current_addr);

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
    fn swseq_wait_idle(&self, timeout_us: u32) -> Result<(), InternalError> {
        let mut start = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut start);
        }

        let timeout_ns = (timeout_us as i64) * 1000;
        let end_nsec = start.tv_nsec + timeout_ns;
        let end_sec = start.tv_sec + (end_nsec / 1_000_000_000);
        let end_nsec = end_nsec % 1_000_000_000;

        loop {
            let ssfs = self.spibar.read8(self.swseq.ssfsc);

            if ssfs & (SSFS_SCIP as u8) == 0 {
                return Ok(());
            }

            let mut now = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            unsafe {
                libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now);
            }

            if now.tv_sec > end_sec || (now.tv_sec == end_sec && now.tv_nsec >= end_nsec) {
                return Err(InternalError::Io("SCIP never cleared (swseq busy timeout)"));
            }

            std::hint::spin_loop();
        }
    }

    /// Wait for software sequencing cycle to complete
    fn swseq_wait_complete(&self, timeout_us: u32) -> Result<(), InternalError> {
        let done_or_err = SSFS_FDONE | SSFS_FCERR;

        let mut start = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut start);
        }

        let timeout_ns = (timeout_us as i64) * 1000;
        let end_nsec = start.tv_nsec + timeout_ns;
        let end_sec = start.tv_sec + (end_nsec / 1_000_000_000);
        let end_nsec = end_nsec % 1_000_000_000;

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

            let mut now = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            unsafe {
                libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now);
            }

            if now.tv_sec > end_sec || (now.tv_sec == end_sec && now.tv_nsec >= end_nsec) {
                return Err(InternalError::Io("Software sequencing timeout"));
            }

            std::hint::spin_loop();
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
    pub fn swseq_send_command(
        &self,
        writearr: &[u8],
        readarr: &mut [u8],
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

        let opcodes = self.opcodes.as_ref().unwrap();
        let spi_type = opcodes.opcode[opcode_index].spi_type;

        // Validate command format based on opcode type
        match spi_type {
            SPI_OPCODE_TYPE_READ_WITH_ADDRESS => {
                if writecnt != 4 {
                    return Err(InternalError::Io(
                        "Read with address requires exactly 4 write bytes",
                    ));
                }
            }
            SPI_OPCODE_TYPE_READ_NO_ADDRESS => {
                if writecnt != 1 {
                    return Err(InternalError::Io(
                        "Read without address requires exactly 1 write byte",
                    ));
                }
            }
            SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS => {
                if writecnt < 4 {
                    return Err(InternalError::Io(
                        "Write with address requires at least 4 write bytes",
                    ));
                }
                if readcnt > 0 {
                    return Err(InternalError::Io("Write commands cannot have read data"));
                }
            }
            SPI_OPCODE_TYPE_WRITE_NO_ADDRESS => {
                if readcnt > 0 {
                    return Err(InternalError::Io("Write commands cannot have read data"));
                }
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
            self.get_atomic_for_opcode(opcode)
        } else {
            0
        };

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
    pub fn swseq_read(&self, addr: u32, buf: &mut [u8]) -> Result<(), InternalError> {
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
            // Max 64 bytes per transfer
            let remaining = len - offset;
            let block_len = remaining.min(Self::SWSEQ_MAX_DATA);

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
    pub fn swseq_write(&self, addr: u32, data: &[u8]) -> Result<(), InternalError> {
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
    pub fn swseq_erase(&self, addr: u32, len: u32) -> Result<(), InternalError> {
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
    fn swseq_wait_wip(&self) -> Result<(), InternalError> {
        // Check we have RDSR opcode
        if self.find_opcode_index(JEDEC_RDSR).is_none() {
            // No RDSR available, just wait a fixed time
            std::thread::sleep(std::time::Duration::from_millis(100));
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
            std::thread::sleep(std::time::Duration::from_micros(100));
        }

        Err(InternalError::Io("Timeout waiting for WIP to clear"))
    }
}

// Non-Linux stub
#[cfg(not(all(feature = "std", target_os = "linux")))]
pub struct IchSpiController {
    _private: (),
}

#[cfg(not(all(feature = "std", target_os = "linux")))]
impl IchSpiController {
    pub fn new(_chipset: &DetectedChipset, _mode: SpiMode) -> Result<Self, InternalError> {
        Err(InternalError::NotSupported(
            "Intel internal programmer only supported on Linux",
        ))
    }
}

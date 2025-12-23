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
    /// Whether configuration is locked
    locked: bool,
    /// Flash descriptor valid
    desc_valid: bool,
    /// Current operating mode
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
            desc_valid: false,
            mode,
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

        // PCH100+ specific: read DLOCK
        if self.generation.is_pch100_compatible() {
            let dlock = self.spibar.read32(PCH100_REG_DLOCK);
            log::debug!("DLOCK: {:#010x}", dlock);
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
    fn determine_mode(&mut self) -> Result<(), InternalError> {
        let mut effective_mode = self.mode;

        if effective_mode == SpiMode::Auto {
            // Check if we should use hwseq
            if self.locked && self.missing_opcodes() {
                log::info!(
                    "Enabling hardware sequencing because some \
                           important opcode is locked"
                );
                effective_mode = SpiMode::HardwareSequencing;
            } else if self.generation.is_pch100_compatible() {
                log::debug!("Enabling hardware sequencing by default for PCH100+");
                effective_mode = SpiMode::HardwareSequencing;
            } else {
                effective_mode = SpiMode::SoftwareSequencing;
            }
        }

        if effective_mode == SpiMode::HardwareSequencing && !self.desc_valid {
            return Err(InternalError::InvalidDescriptor);
        }

        self.mode = effective_mode;
        log::info!("Using {:?} mode", self.mode);

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

    /// Check if the controller is locked
    pub fn is_locked(&self) -> bool {
        self.locked
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

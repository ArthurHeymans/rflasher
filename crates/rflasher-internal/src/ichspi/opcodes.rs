//! Software-sequencing opcode menus, atomic prefixes, and mutation gating.

use super::{Footprint, IchSpiController, MutationIntent, Opcode, Opcodes, SpiMode};
use crate::chipset::IchChipset;
use crate::error::InternalError;
use crate::host::{HostAccess, MmioAccess};
use crate::ich_regs::*;

impl<H: HostAccess> IchSpiController<H> {
    /// Initialize opcodes for ICH7 software sequencing
    pub(super) fn init_ich7_opcodes(&mut self) -> Result<(), InternalError> {
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
    pub(super) fn generate_ich7_opcodes(&self) -> Opcodes {
        let preop = self.spibar.read16(self.ich7_swseq.preop);
        let optype = self.spibar.read16(self.ich7_swseq.optype);
        let opmenu_lo = self.spibar.read32(self.ich7_swseq.opmenu);
        let opmenu_hi = self.spibar.read32(self.ich7_swseq.opmenu + 4);

        let mut opcodes = Opcodes {
            preop: [preop as u8, (preop >> 8) as u8],
            opcode: [Opcode::default(); 8],
        };

        let mut optype_val = optype;
        for op in &mut opcodes.opcode {
            op.spi_type = (optype_val & 0x3) as u8;
            optype_val >>= 2;
        }

        let mut opmenu = opmenu_lo;
        for op in &mut opcodes.opcode[..4] {
            op.opcode = (opmenu & 0xFF) as u8;
            opmenu >>= 8;
        }

        opmenu = opmenu_hi;
        for op in &mut opcodes.opcode[4..] {
            op.opcode = (opmenu & 0xFF) as u8;
            opmenu >>= 8;
        }

        opcodes
    }

    /// Program opcodes to ICH7 hardware registers
    pub(super) fn program_ich7_opcodes(&mut self, opcodes: &Opcodes) -> Result<(), InternalError> {
        let preop = (opcodes.preop[0] as u16) | ((opcodes.preop[1] as u16) << 8);

        let mut optype: u16 = 0;
        for (i, op) in opcodes.opcode.iter().enumerate() {
            optype |= (op.spi_type as u16) << (i * 2);
        }

        let mut opmenu_lo: u32 = 0;
        for (i, op) in opcodes.opcode[..4].iter().enumerate() {
            opmenu_lo |= (op.opcode as u32) << (i * 8);
        }

        let mut opmenu_hi: u32 = 0;
        for (i, op) in opcodes.opcode[4..].iter().enumerate() {
            opmenu_hi |= (op.opcode as u32) << (i * 8);
        }

        self.change_register(self.ich7_swseq.preop, true, preop.into(), 0xffff);
        self.change_register(self.ich7_swseq.optype, true, optype.into(), 0xffff);
        self.change_register(self.ich7_swseq.opmenu, false, opmenu_lo, u32::MAX);
        self.change_register(self.ich7_swseq.opmenu + 4, false, opmenu_hi, u32::MAX);

        Ok(())
    }

    /// Initialize opcodes for software sequencing
    pub(super) fn init_opcodes(&mut self) -> Result<(), InternalError> {
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
    pub(super) fn generate_opcodes(&self) -> Opcodes {
        let preop = self.spibar.read16(self.swseq.preop);
        let optype = self.spibar.read16(self.swseq.optype);
        let opmenu_lo = self.spibar.read32(self.swseq.opmenu);
        let opmenu_hi = self.spibar.read32(self.swseq.opmenu + 4);

        let mut opcodes = Opcodes {
            preop: [preop as u8, (preop >> 8) as u8],
            opcode: [Opcode::default(); 8],
        };

        let mut optype_val = optype;
        for op in &mut opcodes.opcode {
            op.spi_type = (optype_val & 0x3) as u8;
            optype_val >>= 2;
        }

        let mut opmenu = opmenu_lo;
        for op in &mut opcodes.opcode[..4] {
            op.opcode = (opmenu & 0xFF) as u8;
            opmenu >>= 8;
        }

        opmenu = opmenu_hi;
        for op in &mut opcodes.opcode[4..] {
            op.opcode = (opmenu & 0xFF) as u8;
            opmenu >>= 8;
        }

        opcodes
    }

    /// Program opcodes to hardware registers
    pub(super) fn program_opcodes(&mut self, opcodes: &Opcodes) -> Result<(), InternalError> {
        let preop = (opcodes.preop[0] as u16) | ((opcodes.preop[1] as u16) << 8);

        let mut optype: u16 = 0;
        for (i, op) in opcodes.opcode.iter().enumerate() {
            optype |= (op.spi_type as u16) << (i * 2);
        }

        let mut opmenu_lo: u32 = 0;
        for (i, op) in opcodes.opcode[..4].iter().enumerate() {
            opmenu_lo |= (op.opcode as u32) << (i * 8);
        }

        let mut opmenu_hi: u32 = 0;
        for (i, op) in opcodes.opcode[4..].iter().enumerate() {
            opmenu_hi |= (op.opcode as u32) << (i * 8);
        }

        self.change_register(self.swseq.preop, true, preop.into(), 0xffff);
        self.change_register(self.swseq.optype, true, optype.into(), 0xffff);
        self.change_register(self.swseq.opmenu, false, opmenu_lo, u32::MAX);
        self.change_register(self.swseq.opmenu + 4, false, opmenu_hi, u32::MAX);

        Ok(())
    }

    /// 0x35 is only sent as RDSR2 for a chip documented to read it that way.
    /// An unlocked menu also gets RDSR2, and RDSR3 for a chip with WPS, in
    /// place of opcodes that only identify a chip (REMS, RES) or that smaller
    /// erasers replace (chip erase). A locked menu stays as the firmware left it.
    pub(super) fn adopt_chip(&mut self, chip: &rflasher_core::chip::FlashChip) {
        use rflasher_core::chip::Features;
        self.rdsr2 = chip.features.documents_rdsr2();
        if self.mode != SpiMode::SoftwareSequencing || self.locked || self.swseq_locked {
            return;
        }
        let Some(mut opcodes) = self.opcodes.clone() else {
            return;
        };
        let wanted = [
            (self.rdsr2, 0x35),
            (chip.features.contains(Features::WP_WPS), 0x15),
        ];
        let mut changed = false;
        for (_, opcode) in wanted.into_iter().filter(|(needed, _)| *needed) {
            if opcodes.opcode.iter().any(|op| op.opcode == opcode) {
                continue;
            }
            let spare = [JEDEC_REMS, JEDEC_RES, JEDEC_CE_C7, JEDEC_CE_60]
                .into_iter()
                .find_map(|spare| opcodes.opcode.iter().position(|op| op.opcode == spare));
            let Some(spare) = spare else {
                continue;
            };
            opcodes.opcode[spare] = Opcode {
                opcode,
                spi_type: SPI_OPCODE_TYPE_READ_NO_ADDRESS,
                atomic: 0,
            };
            changed = true;
        }
        if !changed {
            return;
        }
        let programmed = if self.generation == IchChipset::Ich7 {
            self.program_ich7_opcodes(&opcodes)
        } else {
            self.program_opcodes(&opcodes)
        };
        if let Err(error) = programmed {
            log::warn!("Cannot add the chip's status reads to OPMENU: {error}");
        }
        // The registers decide what a cycle sends, whatever was accepted.
        self.opcodes = Some(if self.generation == IchChipset::Ich7 {
            self.generate_ich7_opcodes()
        } else {
            self.generate_opcodes()
        });
    }

    /// Check if any required opcodes are missing
    pub(super) fn missing_opcodes(&self) -> bool {
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
    pub(super) fn find_opcode(&self, opcode: u8) -> Option<usize> {
        self.opcodes
            .as_ref()?
            .opcode
            .iter()
            .position(|op| op.opcode == opcode)
    }

    /// Get the opcode type for an opcode in the table
    ///
    /// Returns the SPI opcode type (read/write, with/without address) if found.
    #[allow(dead_code)]
    pub(super) fn get_opcode_type(&self, opcode: u8) -> Option<u8> {
        let idx = self.find_opcode_index(opcode)?;
        self.opcodes.as_ref().map(|ops| ops.opcode[idx].spi_type)
    }

    // Software sequencing emits only three address bytes. Validate the whole
    // transfer before issuing any cycle, including when callers chunk it.
    pub(super) fn check_swseq_range(addr: u32, len: usize) -> Result<(), InternalError> {
        const LIMIT: usize = 1 << 24;
        let start = addr as usize;
        if start >= LIMIT || len > LIMIT - start {
            return Err(InternalError::NotSupported(
                "Address exceeds 3-byte swseq range",
            ));
        }
        Ok(())
    }

    pub(super) fn framing_fits(&self, opcode: u8, spi_type: u8) -> bool {
        match opcode {
            0x35 if self.rdsr2 => spi_type == SPI_OPCODE_TYPE_READ_NO_ADDRESS,
            JEDEC_READ | JEDEC_FAST_READ | JEDEC_REMS | JEDEC_RES => {
                spi_type == SPI_OPCODE_TYPE_READ_WITH_ADDRESS
            }
            JEDEC_RDSR | 0x15 | JEDEC_RDID => spi_type == SPI_OPCODE_TYPE_READ_NO_ADDRESS,
            0x4b => matches!(
                spi_type,
                SPI_OPCODE_TYPE_READ_NO_ADDRESS | SPI_OPCODE_TYPE_READ_WITH_ADDRESS
            ),
            JEDEC_BYTE_PROGRAM | JEDEC_SE | JEDEC_BE_52 | JEDEC_BE_D8 => {
                spi_type == SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS
            }
            JEDEC_CE_C7 | JEDEC_CE_60 | JEDEC_WRSR | 0x31 | 0x11 => {
                spi_type == SPI_OPCODE_TYPE_WRITE_NO_ADDRESS
            }
            _ => true,
        }
    }

    pub(super) fn menu_type(&self, opcode: u8) -> Result<u8, InternalError> {
        let spi_type = self
            .get_opcode_type(opcode)
            .ok_or(InternalError::NotSupported(
                "Opcode not available in OPMENU",
            ))?;
        if !self.framing_fits(opcode, spi_type) {
            return Err(InternalError::NotSupported(
                "OPMENU frames the opcode wrongly",
            ));
        }
        Ok(spi_type)
    }

    /// Find if an opcode is in the preop table
    ///
    /// Returns the preop index (0 or 1) if found, None otherwise.
    /// This is used to detect if a command is a preop like WREN or EWSR.
    pub(super) fn find_preop(&self, opcode: u8) -> Option<usize> {
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
    pub(super) fn get_atomic_for_opcode(&self, opcode: u8) -> Result<u8, InternalError> {
        let opcodes = match &self.opcodes {
            Some(ops) => ops,
            None => return Err(InternalError::NotSupported("No PREOP table")),
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
                return Ok(1); // Use preop[0]
            }
            // Check if WREN is in preop[1] position
            if opcodes.preop[1] == JEDEC_WREN {
                return Ok(2); // Use preop[1]
            }
            return Err(InternalError::NotSupported("WREN not in the PREOP table"));
        }

        Ok(0) // No preop needed
    }

    /// Atomic mode for a write `opcode`. A preop the caller sent first
    /// (`SpiMaster::execute`) selects its PREOP slot, so chips whose WRSR
    /// needs EWSR (older SST25) get it; otherwise WREN as needed.
    pub(super) fn select_atomic(&self, opcode: u8, preop: Option<u8>) -> Result<u8, InternalError> {
        if preop == Some(JEDEC_EWSR)
            && matches!(
                opcode,
                JEDEC_BYTE_PROGRAM
                    | JEDEC_SE
                    | JEDEC_BE_52
                    | JEDEC_BE_D8
                    | JEDEC_CE_C7
                    | JEDEC_CE_60
            )
        {
            return Err(InternalError::NotSupported(
                "Program and erase require WREN",
            ));
        }
        match preop.map(|preop| (preop, self.find_preop(preop))) {
            Some((_, Some(index))) => Ok(index as u8 + 1),
            Some((_, None)) => Err(InternalError::NotSupported("Preop not in the PREOP table")),
            None => self.get_atomic_for_opcode(opcode),
        }
    }

    /// Print HSFS register bits
    pub(super) fn print_hsfs(&self, hsfs: u16) {
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
    pub(super) fn print_dlock(&self, dlock: u32) {
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
    pub(super) fn print_opcodes(&self, opcodes: &Opcodes) {
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

    /// Clear unlocked protected ranges and enable writes lazily, before a
    /// flash mutation is issued.
    pub(super) fn mutation_gate(&mut self) -> Result<(), InternalError> {
        let journal = self.change_count;
        self.clear_protected_ranges()?;
        if !self.bios_writes_enabled
            && let Err(e) = self.enable_bios_write()
        {
            // Leave no range cleared for a mutation that cannot happen.
            self.roll_back(journal);
            return Err(e);
        }
        Ok(())
    }

    /// Classify supported commands by their effect, not their transfer framing.
    pub(super) fn opcode_intent(&self, opcode: u8) -> Option<MutationIntent> {
        match opcode {
            // Only the identified chip establishes what 0x35 does: it reads
            // status register 2 on some parts but enters QPI on Macronix
            // ones, and the OPMENU transfer type cannot tell them apart.
            0x35 if self.rdsr2 => Some(MutationIntent::ReadOnly),
            // RES can also release the device from deep power-down; it does not
            // modify flash contents or protection. 0x4b reads a unique ID.
            JEDEC_READ | JEDEC_FAST_READ | JEDEC_RDSR | 0x15 | JEDEC_REMS | JEDEC_RDID
            | JEDEC_RES | 0x4b => Some(MutationIntent::ReadOnly),
            JEDEC_BYTE_PROGRAM | JEDEC_SE | JEDEC_BE_52 | JEDEC_BE_D8 | JEDEC_CE_C7
            | JEDEC_CE_60 | JEDEC_WRSR | 0x31 | 0x11 => Some(MutationIntent::Mutating),
            _ => None,
        }
    }

    /// Refuse a command that may change restricted flash, then enable
    /// writes for it.
    pub(super) fn gate_opcode(
        &mut self,
        opcode: u8,
        allow_unknown: bool,
        footprint: Footprint,
    ) -> Result<(), InternalError> {
        match self.opcode_intent(opcode) {
            Some(MutationIntent::ReadOnly) => Ok(()),
            None if !allow_unknown => Err(InternalError::NotSupported("Unclassified SPI opcode")),
            _ => {
                self.check_footprint(footprint)?;
                self.mutation_gate()
            }
        }
    }
}

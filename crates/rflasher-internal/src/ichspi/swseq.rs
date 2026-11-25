//! Software-sequencing flash operations (ICH7 and ICH9+/PCH100+).

use super::{Deadline, IchSpiController};
use crate::error::InternalError;
use crate::host::{HostAccess, MmioAccess};
use crate::ich_regs::*;

use super::SpiMode;
use crate::chipset::IchChipset;
use rflasher_core::programmer::FlashAccess;

impl<H: HostAccess> IchSpiController<H> {
    // ========================================================================
    // Software Sequencing Operations
    // ========================================================================

    /// Maximum data length for software sequencing (64 bytes)
    pub const SWSEQ_MAX_DATA: usize = 64;

    /// Wait for software sequencing cycle to not be in progress
    ///
    /// This should be called before starting a new cycle to ensure the
    /// previous one has completed.
    pub(super) fn swseq_wait_idle(&self, timeout_us: u32) -> Result<(), InternalError> {
        let mut deadline = Deadline::new(timeout_us);

        loop {
            let ssfs = self.spibar.read8(self.swseq.ssfsc);

            if ssfs & (SSFS_SCIP as u8) == 0 {
                return Ok(());
            }

            if deadline.expired() {
                return Err(InternalError::Io("SCIP never cleared (swseq busy timeout)"));
            }

            deadline.wait_step(&self.host);
        }
    }

    /// Wait for software sequencing cycle to complete
    pub(super) fn swseq_wait_complete(&self, timeout_us: u32) -> Result<(), InternalError> {
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

            deadline.wait_step(&self.host);
        }
    }

    /// Find the index of an opcode in the opcode table
    pub(super) fn find_opcode_index(&self, opcode: u8) -> Option<usize> {
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
    pub(super) fn ich9_run_opcode(
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
    pub(super) fn ich7_swseq_wait_idle(&self, timeout_us: u32) -> Result<(), InternalError> {
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

            deadline.wait_step(&self.host);
        }
    }

    /// Wait for ICH7 software sequencing cycle to complete
    pub(super) fn ich7_swseq_wait_complete(&self, timeout_us: u32) -> Result<(), InternalError> {
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

            deadline.wait_step(&self.host);
        }
    }

    /// Read data from ICH7 SPID registers
    ///
    /// Optimized to read full 32-bit words and extract bytes.
    #[inline(always)]
    pub(super) fn ich7_read_data(&self, buf: &mut [u8]) {
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
    pub(super) fn ich7_fill_data(&self, data: &[u8]) {
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
    pub(super) fn ich7_run_opcode(
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

    fn swseq_run_opcode(
        &self,
        ich7: bool,
        opcode: u8,
        addr: u32,
        data: &mut [u8],
        is_write: bool,
        atomic: u8,
    ) -> Result<(), InternalError> {
        if ich7 {
            self.ich7_run_opcode(opcode, addr, data, is_write, atomic)
        } else {
            self.ich9_run_opcode(opcode, addr, data, is_write, atomic)
        }
    }

    fn swseq_send_command_impl(
        &mut self,
        ich7: bool,
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

        self.menu_type(opcode)?;
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
            SPI_OPCODE_TYPE_WRITE_NO_ADDRESS => (0, &writearr[1..], true),
            SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS => {
                let addr = ((writearr[1] as u32) << 16)
                    | ((writearr[2] as u32) << 8)
                    | (writearr[3] as u32);
                (addr, &writearr[4..], true)
            }
            SPI_OPCODE_TYPE_READ_WITH_ADDRESS => {
                let addr = ((writearr[1] as u32) << 16)
                    | ((writearr[2] as u32) << 8)
                    | (writearr[3] as u32);
                (addr, &[], false)
            }
            SPI_OPCODE_TYPE_READ_NO_ADDRESS => (0, &[], false),
            _ => (0, &[], false),
        };

        if data_slice.len() > Self::SWSEQ_MAX_DATA || readcnt > Self::SWSEQ_MAX_DATA {
            return Err(InternalError::Io("Data length exceeds swseq maximum"));
        }
        if matches!(
            spi_type,
            SPI_OPCODE_TYPE_READ_WITH_ADDRESS | SPI_OPCODE_TYPE_WRITE_WITH_ADDRESS
        ) {
            Self::check_swseq_range(addr, if is_write { data_slice.len() } else { readcnt })?;
        }

        // Determine if we need atomic mode (preop + main op)
        let atomic = if is_write {
            self.select_atomic(opcode, preop)?
        } else {
            0
        };

        let footprint = self.command_footprint(opcode, addr, data_slice.len());
        self.gate_opcode(opcode, allow_unknown, footprint)?;

        if is_write {
            let mut data_buf: [u8; 64] = [0; 64];
            let len = data_slice.len().min(64);
            data_buf[..len].copy_from_slice(&data_slice[..len]);
            self.swseq_run_opcode(ich7, opcode, addr, &mut data_buf[..len], true, atomic)
        } else {
            let len = readcnt.min(64);
            self.swseq_run_opcode(ich7, opcode, addr, &mut readarr[..len], false, 0)
        }
    }

    fn swseq_read_impl(
        &mut self,
        ich7: bool,
        addr: u32,
        buf: &mut [u8],
    ) -> Result<(), InternalError> {
        Self::check_swseq_range(addr, buf.len())?;
        // Software sequencing cannot emit dummy cycles, so FAST_READ (0x0B)
        // cannot be used as a drop-in fallback without shifting every block by
        // one dummy byte. Require standard JEDEC READ (0x03).
        if self.find_opcode_index(JEDEC_READ).is_none() {
            return Err(InternalError::NotSupported("No read opcode available"));
        }

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

            let writearr = [
                JEDEC_READ,
                ((current_addr >> 16) & 0xff) as u8,
                ((current_addr >> 8) & 0xff) as u8,
                (current_addr & 0xff) as u8,
            ];

            self.swseq_send_command_impl(
                ich7,
                &writearr,
                &mut buf[offset..offset + block_len],
                None,
                false,
            )?;

            offset += block_len;
            current_addr += block_len as u32;
        }

        Ok(())
    }

    fn swseq_write_impl(
        &mut self,
        ich7: bool,
        addr: u32,
        data: &[u8],
    ) -> Result<(), InternalError> {
        Self::check_swseq_range(addr, data.len())?;
        if self.find_opcode_index(JEDEC_BYTE_PROGRAM).is_none() {
            return Err(InternalError::NotSupported("No program opcode available"));
        }

        let len = data.len();
        // Refuse before the first page, not part-way through.
        self.check_range(FlashAccess::Modify, addr, len as u64)?;
        let mut offset = 0;
        let mut current_addr = addr;

        while offset < len {
            // Max 64 bytes per transfer, but also respect page boundaries (256 bytes)
            let remaining = len - offset;
            let page_remaining = 256 - (current_addr as usize & 0xff);
            let block_len = remaining.min(Self::SWSEQ_MAX_DATA).min(page_remaining);

            let mut writearr = [0u8; 68]; // 4 header + 64 data max
            writearr[0] = JEDEC_BYTE_PROGRAM;
            writearr[1] = ((current_addr >> 16) & 0xff) as u8;
            writearr[2] = ((current_addr >> 8) & 0xff) as u8;
            writearr[3] = (current_addr & 0xff) as u8;
            writearr[4..4 + block_len].copy_from_slice(&data[offset..offset + block_len]);

            // Send program command (WREN is handled automatically via atomic mode)
            self.swseq_send_command_impl(ich7, &writearr[..4 + block_len], &mut [], None, false)?;

            // Wait for programming to complete by polling status register
            self.swseq_wait_wip_impl(ich7)?;

            offset += block_len;
            current_addr += block_len as u32;
        }

        Ok(())
    }

    fn swseq_wait_wip_impl(&mut self, ich7: bool) -> Result<(), InternalError> {
        if self.find_opcode_index(JEDEC_RDSR).is_none() {
            // No RDSR available, just wait a fixed time
            self.host.delay_us(100_000);
            return Ok(());
        }

        let rdsr_arr = [JEDEC_RDSR];
        let mut status = [0u8; 1];

        // Poll for up to 60 seconds (for chip erase)
        for _ in 0..600_000 {
            self.swseq_send_command_impl(ich7, &rdsr_arr, &mut status, None, false)?;

            // WIP is bit 0
            if status[0] & 0x01 == 0 {
                return Ok(());
            }

            // Small delay between polls
            self.host.delay_us(100);
        }

        Err(InternalError::Io("Timeout waiting for WIP to clear"))
    }

    /// Send a raw SPI command using ICH7 software sequencing.
    ///
    /// Only classified opcodes in OPMENU are accepted. Mutating commands
    /// enable BIOS writes first; unknown commands are rejected, as is 0x35
    /// unless the identified chip reads status register 2 with it. Opcode
    /// meanings must match the connected flash.
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
    pub(super) fn ich7_swseq_send_command_after(
        &mut self,
        writearr: &[u8],
        readarr: &mut [u8],
        preop: Option<u8>,
        allow_unknown: bool,
    ) -> Result<(), InternalError> {
        self.swseq_send_command_impl(true, writearr, readarr, preop, allow_unknown)
    }

    /// Read data using ICH7 software sequencing
    pub fn ich7_swseq_read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), InternalError> {
        self.swseq_read_impl(true, addr, buf)
    }

    /// Write data using ICH7 software sequencing
    pub fn ich7_swseq_write(&mut self, addr: u32, data: &[u8]) -> Result<(), InternalError> {
        self.swseq_write_impl(true, addr, data)
    }

    /// Raw controller erase is unavailable without chip-specific geometry;
    /// see [`Self::swseq_erase_blocks`].
    pub fn ich7_swseq_erase(&mut self, _addr: u32, _len: u32) -> Result<(), InternalError> {
        Err(InternalError::NotSupported(
            "Software erase requires chip-specific geometry",
        ))
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
    /// BIOS writes first; unknown commands are rejected, as is 0x35 unless the
    /// identified chip reads status register 2 with it. Opcode meanings must
    /// match the connected flash.
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
    pub(super) fn swseq_send_command_after(
        &mut self,
        writearr: &[u8],
        readarr: &mut [u8],
        preop: Option<u8>,
        allow_unknown: bool,
    ) -> Result<(), InternalError> {
        self.swseq_send_command_impl(false, writearr, readarr, preop, allow_unknown)
    }

    /// Read data using software sequencing
    pub fn swseq_read(&mut self, addr: u32, buf: &mut [u8]) -> Result<(), InternalError> {
        self.swseq_read_impl(false, addr, buf)
    }

    /// Write data using software sequencing
    pub fn swseq_write(&mut self, addr: u32, data: &[u8]) -> Result<(), InternalError> {
        self.swseq_write_impl(false, addr, data)
    }

    /// Raw controller erase is unavailable without chip-specific geometry;
    /// see [`Self::swseq_erase_blocks`].
    pub fn swseq_erase(&mut self, _addr: u32, _len: u32) -> Result<(), InternalError> {
        Err(InternalError::NotSupported(
            "Software erase requires chip-specific geometry",
        ))
    }

    /// Erase `len` bytes at `addr` in `block_size` blocks with `opcode`, then
    /// wait for each to finish, using ICH7 or ICH9+ software sequencing.
    ///
    /// The caller supplies the geometry it verified for the connected chip:
    /// `opcode` must erase exactly `block_size` bytes there. This is the raw
    /// command path chip-aware callers already use, without a heap or the
    /// chip database. The full range must be writable, and the opcode menu
    /// must support RDSR polling; a fixed delay cannot guarantee completion.
    pub fn swseq_erase_blocks(
        &mut self,
        opcode: u8,
        block_size: u32,
        addr: u32,
        len: u32,
    ) -> Result<(), InternalError> {
        if self.mode == SpiMode::HardwareSequencing {
            return Err(InternalError::NotSupported(
                "Hardware sequencing erases by address",
            ));
        }
        if !matches!(opcode, JEDEC_SE | JEDEC_BE_52 | JEDEC_BE_D8) {
            return Err(InternalError::NotSupported("Not a block erase opcode"));
        }
        if !block_size.is_power_of_two()
            || !addr.is_multiple_of(block_size)
            || !len.is_multiple_of(block_size)
        {
            return Err(InternalError::Io("Erase address/length not aligned"));
        }
        // Reject the whole range before erasing any of it.
        Self::check_swseq_range(addr, len as usize)?;
        self.check_range(FlashAccess::Modify, addr, u64::from(len))?;
        // The legacy wait helpers fall back to a fixed delay without RDSR.
        // That cannot guarantee erase completion, so require real polling.
        self.menu_type(JEDEC_RDSR)?;
        let ich7 = self.generation == IchChipset::Ich7;
        for block in (addr..addr + len).step_by(block_size as usize) {
            let [_, high, middle, low] = block.to_be_bytes();
            let command = [opcode, high, middle, low];
            self.swseq_send_command_impl(ich7, &command, &mut [], None, false)?;
            self.swseq_wait_wip_impl(ich7)?;
        }
        Ok(())
    }
}

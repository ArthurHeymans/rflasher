//! SunxiFel device - main programmer struct implementing SpiMaster + OpaqueMaster
//!
//! Uses the xfel payload approach: a pre-compiled SPI driver runs natively
//! on the SoC, and the host drives SPI by writing bytecode commands to a
//! shared buffer and executing the payload via FEL.
//!
//! # Architecture
//!
//! SunxiFel implements both `SpiMaster` and `OpaqueMaster`:
//!
//! - **SpiMaster**: Generic SPI command execution for probe, status register
//!   reads, write protection, etc. Each `execute()` call builds bytecodes
//!   and runs the payload once.
//!
//! - **OpaqueMaster**: Firmware-accelerated bulk read/write. Uses
//!   `SPI_CMD_FAST` for small fixed commands (WREN, opcode+address),
//!   `SPI_CMD_TXBUF` for page data, and `SPI_CMD_SPINOR_WAIT` for on-SoC
//!   busy polling. Writes batch ~215 pages per payload execution, matching
//!   xfel's `spinor_helper_write` approach.
//!
//! Use with `HybridFlashDevice` for optimal performance:
//! - read/write/erase → OpaqueMaster (firmware-accelerated)
//! - WP/status regs   → SpiMaster (generic SPI commands)

use nusb::transfer::{Bulk, In, Out};
use nusb::MaybeFuture;
use rflasher_core::chip::EraseBlock;
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::flash::select_erase_block;
use rflasher_core::programmer::{OpaqueMaster, SpiFeatures, SpiMaster};
use rflasher_core::spi::SpiCommand;

use crate::chips::{self, spi_cmd, ChipFamily, SpiPayloadInfo};
use crate::error::{Error, Result};
use crate::protocol::{FelTransport, FelVersion, FEL_PID, FEL_VID};

/// Allwinner FEL SPI programmer
pub struct SunxiFel {
    transport: FelTransport,
    version: FelVersion,
    chip: ChipFamily,
    spi_info: SpiPayloadInfo,
    _interface: nusb::Interface,
    /// Whether to use 4-byte addressing for OpaqueMaster read/write.
    /// Set after probing via `set_use_4byte_addr()` when the flash is >16MB.
    use_4byte_addr: bool,
    /// Chip erase block table, set after probing via `set_erase_blocks()`.
    /// Used by `OpaqueMaster::erase()` to select the correct opcode and
    /// block size from the chip's actual capabilities (from RON database or SFDP).
    erase_blocks: Vec<EraseBlock>,
}

impl SunxiFel {
    /// Open the first available FEL device
    pub fn open() -> Result<Self> {
        let devices: Vec<_> = nusb::list_devices()
            .wait()
            .map_err(|e| Error::Usb(format!("failed to enumerate USB devices: {}", e)))?
            .filter(|d| d.vendor_id() == FEL_VID && d.product_id() == FEL_PID)
            .collect();

        let device_info = devices.first().ok_or(Error::DeviceNotFound)?;

        log::info!(
            "Found FEL device: bus={} addr={}",
            device_info.busnum(),
            device_info.device_address()
        );

        let device = device_info
            .open()
            .wait()
            .map_err(|e| Error::Usb(format!("failed to open device: {}", e)))?;

        let interface = device
            .claim_interface(0)
            .wait()
            .map_err(|e| Error::Usb(format!("failed to claim interface: {}", e)))?;

        // Find bulk endpoints
        let mut ep_in = None;
        let mut ep_out = None;
        if let Ok(config) = device.active_configuration() {
            for alt in config.interface_alt_settings() {
                if alt.interface_number() == 0 {
                    for ep in alt.endpoints() {
                        match ep.direction() {
                            nusb::transfer::Direction::In => {
                                if ep_in.is_none() {
                                    ep_in = Some(ep.address());
                                }
                            }
                            nusb::transfer::Direction::Out => {
                                if ep_out.is_none() {
                                    ep_out = Some(ep.address());
                                }
                            }
                        }
                    }
                    break;
                }
            }
        }

        let ep_in_addr = ep_in.ok_or_else(|| Error::Usb("no bulk IN endpoint".into()))?;
        let ep_out_addr = ep_out.ok_or_else(|| Error::Usb("no bulk OUT endpoint".into()))?;

        let out_ep = interface
            .endpoint::<Bulk, Out>(ep_out_addr)
            .map_err(|e| Error::Usb(format!("OUT endpoint: {}", e)))?;
        let in_ep = interface
            .endpoint::<Bulk, In>(ep_in_addr)
            .map_err(|e| Error::Usb(format!("IN endpoint: {}", e)))?;

        let mut transport = FelTransport::new(out_ep, in_ep);

        // Query FEL version
        let version = transport.fel_version()?;
        log::info!(
            "FEL version: ID=0x{:08x} firmware=0x{:08x} scratchpad=0x{:08x}",
            version.id,
            version.firmware,
            version.scratchpad
        );

        let chip = chips::detect_chip(version.id).ok_or(Error::UnsupportedSoc(version.id))?;
        log::info!("Detected SoC: {}", chip.name());

        // Upload SPI payload and initialize hardware
        let spi_info = chips::spi_init(&mut transport, chip)?;
        log::info!(
            "SPI initialized: swapbuf=0x{:08x} swaplen={} cmdlen={}",
            spi_info.swapbuf,
            spi_info.swaplen,
            spi_info.cmdlen
        );

        Ok(Self {
            transport,
            version,
            chip,
            spi_info,
            _interface: interface,
            use_4byte_addr: false,
            erase_blocks: Vec::new(),
        })
    }

    /// Set the chip's erase block table for `OpaqueMaster::erase()`.
    ///
    /// Call this after probing with the chip's erase blocks from the RON
    /// database or SFDP. Without this, `OpaqueMaster::erase()` will return
    /// an error and the hybrid adapter falls back to SPI-based erase.
    pub fn set_erase_blocks(&mut self, blocks: Vec<EraseBlock>) {
        self.erase_blocks = blocks;
    }

    /// Set whether to use 4-byte addressing for bulk read/write operations.
    ///
    /// Call this after probing if the flash chip requires 4-byte addressing
    /// (i.e., capacity >16 MiB). This affects `OpaqueMaster::read()` and
    /// `OpaqueMaster::write()`.
    pub fn set_use_4byte_addr(&mut self, use_4byte: bool) {
        self.use_4byte_addr = use_4byte;
    }

    /// Get the detected SoC name
    pub fn soc_name(&self) -> &'static str {
        self.chip.name()
    }

    /// Get the FEL version info
    pub fn version(&self) -> &FelVersion {
        &self.version
    }

    /// Perform a SPI transfer using the xfel bytecode protocol.
    ///
    /// This implements `fel_spi_xfer` from xfel:
    /// 1. Build command bytecodes (SELECT, TXBUF/FAST, RXBUF, DESELECT, END)
    /// 2. Write TX data to swap buffer (unless using FAST for small TX)
    /// 3. Execute the payload (which processes the bytecodes)
    /// 4. Read RX data from swap buffer
    ///
    /// Optimization: when TX data is small (≤64 bytes), uses `SPI_CMD_FAST`
    /// to embed the TX bytes directly in the command buffer, avoiding a
    /// separate FEL write for the swap buffer. This saves ~9 USB transfers
    /// per read chunk (significant for bulk reads).
    fn spi_xfer(&mut self, txbuf: &[u8], rxbuf: &mut [u8]) -> Result<()> {
        let txlen = txbuf.len() as u32;
        let rxlen = rxbuf.len() as u32;
        let swapbuf = self.spi_info.swapbuf;
        let swaplen = self.spi_info.swaplen;

        if txlen <= swaplen && rxlen <= swaplen {
            // Fast path: everything fits in one transfer.
            // Use SPI_CMD_FAST for small TX data to avoid a separate fel_write.
            let use_fast_tx = txlen > 0 && txlen <= 64;

            let mut cbuf = Vec::with_capacity(32 + txlen as usize);
            cbuf.push(spi_cmd::SELECT);

            if use_fast_tx {
                // Embed TX data directly in the command buffer
                cbuf.push(spi_cmd::FAST);
                cbuf.push(txlen as u8);
                cbuf.extend_from_slice(txbuf);
            } else if txlen > 0 {
                cbuf.push(spi_cmd::TXBUF);
                cbuf.extend_from_slice(&swapbuf.to_le_bytes());
                cbuf.extend_from_slice(&txlen.to_le_bytes());
            }
            if rxlen > 0 {
                cbuf.push(spi_cmd::RXBUF);
                cbuf.extend_from_slice(&swapbuf.to_le_bytes());
                cbuf.extend_from_slice(&rxlen.to_le_bytes());
            }

            cbuf.push(spi_cmd::DESELECT);
            cbuf.push(spi_cmd::END);

            if !use_fast_tx && txlen > 0 {
                self.transport.fel_write(swapbuf, txbuf)?;
            }

            chips::spi_run(&mut self.transport, &self.spi_info, &cbuf)?;

            if rxlen > 0 {
                let data = self.transport.fel_read(swapbuf, rxlen as usize)?;
                rxbuf.copy_from_slice(&data);
            }
        } else {
            // Slow path: chunk the transfer
            // Select
            let cbuf = [spi_cmd::SELECT, spi_cmd::END];
            chips::spi_run(&mut self.transport, &self.spi_info, &cbuf)?;

            // TX in chunks
            let mut tx_off = 0u32;
            let mut tx_rem = txlen;
            while tx_rem > 0 {
                let n = tx_rem.min(swaplen);
                let mut cbuf = Vec::with_capacity(16);
                cbuf.push(spi_cmd::TXBUF);
                cbuf.extend_from_slice(&swapbuf.to_le_bytes());
                cbuf.extend_from_slice(&n.to_le_bytes());
                cbuf.push(spi_cmd::END);

                self.transport
                    .fel_write(swapbuf, &txbuf[tx_off as usize..(tx_off + n) as usize])?;
                chips::spi_run(&mut self.transport, &self.spi_info, &cbuf)?;

                tx_off += n;
                tx_rem -= n;
            }

            // RX in chunks
            let mut rx_off = 0u32;
            let mut rx_rem = rxlen;
            while rx_rem > 0 {
                let n = rx_rem.min(swaplen);
                let mut cbuf = Vec::with_capacity(16);
                cbuf.push(spi_cmd::RXBUF);
                cbuf.extend_from_slice(&swapbuf.to_le_bytes());
                cbuf.extend_from_slice(&n.to_le_bytes());
                cbuf.push(spi_cmd::END);

                chips::spi_run(&mut self.transport, &self.spi_info, &cbuf)?;

                let data = self.transport.fel_read(swapbuf, n as usize)?;
                rxbuf[rx_off as usize..(rx_off + n) as usize].copy_from_slice(&data);

                rx_off += n;
                rx_rem -= n;
            }

            // Deselect
            let cbuf = [spi_cmd::DESELECT, spi_cmd::END];
            chips::spi_run(&mut self.transport, &self.spi_info, &cbuf)?;
        }

        Ok(())
    }

    /// Erase a block using on-SoC bytecodes (FAST for WREN + erase, SPINOR_WAIT).
    ///
    /// This is a single payload execution per erase block, matching xfel's
    /// `spinor_sector_erase_*` functions. The SoC firmware handles busy-wait
    /// locally, so there are no USB round-trips for status polling.
    fn erase_block_bytecode(&mut self, opcode: u8, addr: u32, use_4byte: bool) -> Result<()> {
        let fast_len: u8 = if use_4byte { 5 } else { 4 }; // opcode + addr bytes

        let mut cbuf = Vec::with_capacity(32);

        // WREN
        cbuf.push(spi_cmd::SELECT);
        cbuf.push(spi_cmd::FAST);
        cbuf.push(1);
        cbuf.push(0x06); // WREN opcode
        cbuf.push(spi_cmd::DESELECT);

        // Erase command (opcode + address)
        cbuf.push(spi_cmd::SELECT);
        cbuf.push(spi_cmd::FAST);
        cbuf.push(fast_len);
        cbuf.push(opcode);
        if use_4byte {
            cbuf.push((addr >> 24) as u8);
        }
        cbuf.push((addr >> 16) as u8);
        cbuf.push((addr >> 8) as u8);
        cbuf.push(addr as u8);
        cbuf.push(spi_cmd::DESELECT);

        // Wait for erase to complete (on-SoC RDSR polling)
        cbuf.push(spi_cmd::SELECT);
        cbuf.push(spi_cmd::SPINOR_WAIT);
        cbuf.push(spi_cmd::DESELECT);

        cbuf.push(spi_cmd::END);

        chips::spi_run(&mut self.transport, &self.spi_info, &cbuf)
    }

    /// Batched page programming matching xfel's `spinor_helper_write`.
    ///
    /// Packs multiple page programs into a single command+swap buffer pair
    /// for maximum throughput. Each page in the batch uses:
    /// - `SPI_CMD_FAST` for WREN (1 byte, from command buffer)
    /// - `SPI_CMD_TXBUF` for PP data (from swap buffer at page-specific offset)
    /// - `SPI_CMD_SPINOR_WAIT` for on-SoC busy polling
    ///
    /// With cmdlen=4096 and swaplen=65536: ~215 pages per batch (limited by
    /// 19 bytes/page in cmd buffer), reducing USB round-trips by ~128×
    /// compared to per-page execution.
    fn batched_write(
        &mut self,
        addr: u32,
        data: &[u8],
        page_size: usize,
        use_4byte: bool,
    ) -> Result<()> {
        let addr_len: usize = if use_4byte { 4 } else { 3 };
        let pp_opcode: u8 = 0x02; // Page Program
        let wren_opcode: u8 = 0x06;

        // Per-page overhead:
        //   cmd buffer:  SELECT(1) + FAST(1+1+1) + DESELECT(1) = 5 for WREN
        //                SELECT(1) + TXBUF(1+4+4) + DESELECT(1) = 11 for PP
        //                SELECT(1) + SPINOR_WAIT(1) + DESELECT(1) = 3 for wait
        //                Total: 19 bytes per page
        //   swap buffer: opcode(1) + addr(3|4) + data(up to page_size)
        let per_page_cbuf: usize = 19;
        let cmdlen = self.spi_info.cmdlen as usize;
        let swaplen = self.spi_info.swaplen as usize;
        let swapbuf = self.spi_info.swapbuf;
        let max_tx_per_page = 1 + addr_len + page_size;

        let mut remaining = data.len();
        let mut data_offset = 0usize;
        let mut current_addr = addr;

        while remaining > 0 {
            let mut cbuf = Vec::with_capacity(cmdlen);
            let mut txbuf = Vec::with_capacity(swaplen);

            // Pack as many pages as will fit (matching xfel's loop bounds:
            //   clen < cmdlen - 19 - 1  AND  txlen < swaplen - granularity - addr_overhead)
            while remaining > 0
                && cbuf.len() + per_page_cbuf < cmdlen
                && txbuf.len() + max_tx_per_page <= swaplen
            {
                // Respect page boundaries
                let page_offset = (current_addr as usize) % page_size;
                let bytes_to_page_end = page_size - page_offset;
                let n = remaining.min(bytes_to_page_end);

                // WREN via SPI_CMD_FAST
                cbuf.push(spi_cmd::SELECT);
                cbuf.push(spi_cmd::FAST);
                cbuf.push(1);
                cbuf.push(wren_opcode);
                cbuf.push(spi_cmd::DESELECT);

                // PP via SPI_CMD_TXBUF (offset into swap buffer for this page)
                let swap_offset = swapbuf + txbuf.len() as u32;
                let txlen = (1 + addr_len + n) as u32;
                cbuf.push(spi_cmd::SELECT);
                cbuf.push(spi_cmd::TXBUF);
                cbuf.extend_from_slice(&swap_offset.to_le_bytes());
                cbuf.extend_from_slice(&txlen.to_le_bytes());
                cbuf.push(spi_cmd::DESELECT);

                // Wait for page program to complete (on-SoC RDSR polling)
                cbuf.push(spi_cmd::SELECT);
                cbuf.push(spi_cmd::SPINOR_WAIT);
                cbuf.push(spi_cmd::DESELECT);

                // TX data: PP opcode + address + page data
                txbuf.push(pp_opcode);
                if use_4byte {
                    txbuf.push((current_addr >> 24) as u8);
                }
                txbuf.push((current_addr >> 16) as u8);
                txbuf.push((current_addr >> 8) as u8);
                txbuf.push(current_addr as u8);
                txbuf.extend_from_slice(&data[data_offset..data_offset + n]);

                current_addr += n as u32;
                data_offset += n;
                remaining -= n;
            }

            cbuf.push(spi_cmd::END);

            // Upload all page data to swap buffer, then execute all pages at once
            self.transport.fel_write(swapbuf, &txbuf)?;
            chips::spi_run(&mut self.transport, &self.spi_info, &cbuf)?;
        }

        Ok(())
    }

    /// Read flash data using SPI_CMD_FAST for the opcode+address header.
    ///
    /// For each chunk, embeds the READ command (opcode + address) directly in
    /// the command buffer via SPI_CMD_FAST, then receives data via RXBUF.
    /// This eliminates the separate FEL write for TX data that `spi_xfer`
    /// would do, saving ~9 USB transfers per chunk.
    fn fast_read(&mut self, addr: u32, buf: &mut [u8], use_4byte: bool) -> Result<()> {
        let swapbuf = self.spi_info.swapbuf;
        let swaplen = self.spi_info.swaplen as usize;
        let read_opcode: u8 = 0x03; // READ
        let addr_len: usize = if use_4byte { 4 } else { 3 };

        let mut offset = 0usize;
        let mut current_addr = addr;

        while offset < buf.len() {
            let chunk_len = (buf.len() - offset).min(swaplen);

            // Build command buffer: SELECT + FAST(opcode+addr) + RXBUF + DESELECT + END
            let fast_len = (1 + addr_len) as u8;
            let mut cbuf = Vec::with_capacity(32);
            cbuf.push(spi_cmd::SELECT);
            cbuf.push(spi_cmd::FAST);
            cbuf.push(fast_len);
            cbuf.push(read_opcode);
            if use_4byte {
                cbuf.push((current_addr >> 24) as u8);
            }
            cbuf.push((current_addr >> 16) as u8);
            cbuf.push((current_addr >> 8) as u8);
            cbuf.push(current_addr as u8);
            cbuf.push(spi_cmd::RXBUF);
            cbuf.extend_from_slice(&swapbuf.to_le_bytes());
            cbuf.extend_from_slice(&(chunk_len as u32).to_le_bytes());
            cbuf.push(spi_cmd::DESELECT);
            cbuf.push(spi_cmd::END);

            // Execute (no separate fel_write needed - TX is embedded in cmd buffer)
            chips::spi_run(&mut self.transport, &self.spi_info, &cbuf)?;

            // Read result from swap buffer
            let data = self.transport.fel_read(swapbuf, chunk_len)?;
            buf[offset..offset + chunk_len].copy_from_slice(&data);

            offset += chunk_len;
            current_addr += chunk_len as u32;
        }

        Ok(())
    }
}

// =============================================================================
// SpiMaster implementation (for probe, status registers, WP, generic SPI)
// =============================================================================

impl SpiMaster for SunxiFel {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::FOUR_BYTE_ADDR
    }

    fn max_read_len(&self) -> usize {
        self.spi_info.swaplen as usize
    }

    fn max_write_len(&self) -> usize {
        self.spi_info.swaplen as usize
    }

    fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];
        cmd.encode_header(&mut write_data);
        write_data[header_len..].copy_from_slice(cmd.write_data);

        self.spi_xfer(&write_data, cmd.read_buf)
            .map_err(|_| CoreError::ProgrammerError)
    }

    fn delay_us(&mut self, us: u32) {
        std::thread::sleep(std::time::Duration::from_micros(us as u64));
    }
}

// =============================================================================
// OpaqueMaster implementation (for fast bulk read/write via HybridFlashDevice)
// =============================================================================

impl OpaqueMaster for SunxiFel {
    fn size(&self) -> usize {
        // Size is determined by FlashContext after probing; HybridFlashDevice
        // uses FlashContext::total_size(), not this method.
        0
    }

    fn read(&mut self, addr: u32, buf: &mut [u8]) -> CoreResult<()> {
        self.fast_read(addr, buf, self.use_4byte_addr)
            .map_err(|_| CoreError::ReadError { addr })
    }

    fn write(&mut self, addr: u32, data: &[u8]) -> CoreResult<()> {
        // Batched page program with on-SoC busy-wait.
        // Page size is always 256 for standard SPI NOR.
        self.batched_write(addr, data, 256, self.use_4byte_addr)
            .map_err(|_| CoreError::WriteError { addr })
    }

    fn erase(&mut self, addr: u32, len: u32) -> CoreResult<()> {
        // Use the chip's actual erase block table (from RON/SFDP) to select
        // the right opcode and block size. If no erase blocks are configured,
        // return Err so the hybrid adapter falls back to SPI-based erase.
        if self.erase_blocks.is_empty() {
            return Err(CoreError::ProgrammerError);
        }

        let erase_block =
            select_erase_block(&self.erase_blocks, addr, len).ok_or(CoreError::ProgrammerError)?;

        let use_4byte = self.use_4byte_addr;
        let max_block_size = erase_block.max_block_size();
        let mut current = addr;
        let end = addr + len;

        while current < end {
            let offset_in_layout = current - addr;
            let block_size = erase_block
                .block_size_at_offset(offset_in_layout)
                .unwrap_or(max_block_size);

            self.erase_block_bytecode(erase_block.opcode, current, use_4byte)
                .map_err(|_| CoreError::ProgrammerError)?;

            current += block_size;
        }

        Ok(())
    }
}

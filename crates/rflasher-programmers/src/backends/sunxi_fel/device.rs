//! SunxiFel device - main programmer struct implementing SpiMaster
//!
//! Uses the xfel payload approach: a pre-compiled SPI driver runs natively
//! on the SoC, and the host drives SPI by writing bytecode commands to a
//! shared buffer and executing the payload via FEL.
//!
//! # Architecture
//!
//! - `SpiMaster::execute`: generic SPI command execution for probe, erase,
//!   status registers, write protection, etc. Each call builds bytecodes
//!   and runs the payload once.
//!
//! - `SpiMaster::read_flash` / `program_flash`: payload-accelerated bulk
//!   read and page programming. Uses `SPI_CMD_FAST` for small fixed
//!   commands (WREN, opcode+address), `SPI_CMD_TXBUF` for page data, and
//!   `SPI_CMD_SPINOR_WAIT` for on-SoC busy polling. Writes batch ~215 pages
//!   per payload execution, matching xfel's `spinor_helper_write` approach.
//!   Chips the payload cannot handle use the generic SPI protocol.

use nusb::transfer::{Bulk, In, Out};
use rflasher_core::chip::{Features, WriteGranularity};
use rflasher_core::error::{Error as CoreError, Result as CoreResult};
use rflasher_core::flash::{self, AddressMode, FlashContext};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::{AddressWidth, SpiCommand, opcodes};

use super::chips::{self, ChipFamily, SpiPayloadInfo, spi_cmd};
use super::error::{Error, Result};
use super::protocol::{FEL_PID, FEL_VID, FelTransport, FelVersion};

fn addressed_header(opcode: u8, opcode_4b: u8, addr: u32, four_byte: bool) -> ([u8; 5], usize) {
    let width = if four_byte {
        AddressWidth::FourByte
    } else {
        AddressWidth::ThreeByte
    };
    let mut header = [0; 5];
    header[0] = if four_byte { opcode_4b } else { opcode };
    width.encode(addr, &mut header[1..]);
    (header, 1 + width.bytes() as usize)
}

/// Allwinner FEL SPI programmer
pub struct SunxiFel {
    transport: FelTransport,
    version: FelVersion,
    chip: ChipFamily,
    spi_info: SpiPayloadInfo,
    _interface: nusb::Interface,
}

impl crate::UsbProgrammer for SunxiFel {
    const NAME: &'static str = "sunxi FEL";
    type Config = ();
    type Error = Error;

    async fn candidates(_config: &()) -> Result<Vec<nusb::DeviceInfo>> {
        let selector = nusb::DeviceSelector::all().with_vid_pid(FEL_VID, FEL_PID);
        crate::usb::list_devices(&[selector])
            .await
            .map_err(|e| Error::Usb(format!("failed to enumerate USB devices: {}", e)))
    }

    async fn open_device(device: nusb::DeviceInfo, _config: ()) -> Result<Self> {
        Self::open(device).await
    }
}

impl SunxiFel {
    /// Open a FEL device found with [`crate::UsbProgrammer::candidates`]
    pub async fn open(device_info: nusb::DeviceInfo) -> Result<Self> {
        let device = device_info
            .open()
            .await
            .map_err(|e| Error::Usb(format!("failed to open device: {}", e)))?;

        let interface = device
            .claim_interface(0)
            .await
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
        })
    }

    /// Whether the payload's 256-byte page-program loop supports this chip.
    fn supports_bulk_write(ctx: &FlashContext) -> bool {
        let chip = &ctx.chip;
        !chip.features.contains(Features::AAI_WORD)
            && chip.write_granularity != WriteGranularity::Byte
            && chip.page_size == 256
            && (ctx.address_mode == AddressMode::ThreeByte || chip.features.supports_4ba_program())
    }

    /// Whether the payload's plain read loop supports this chip.
    fn supports_bulk_read(ctx: &FlashContext) -> bool {
        // Mode-switch/EAR chips need the generic protocol's preparation.
        ctx.address_mode == AddressMode::ThreeByte || ctx.chip.features.supports_4ba_read()
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
                let (header, len) =
                    addressed_header(opcodes::PP, opcodes::PP_4B, current_addr, use_4byte);
                txbuf.extend_from_slice(&header[..len]);
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
            let (header, len) =
                addressed_header(opcodes::READ, opcodes::READ_4B, current_addr, use_4byte);
            cbuf.extend_from_slice(&header[..len]);
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

    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
        let header_len = cmd.header_len();
        let mut write_data = vec![0u8; header_len + cmd.write_data.len()];
        cmd.encode_header(&mut write_data);
        write_data[header_len..].copy_from_slice(cmd.write_data);

        self.spi_xfer(&write_data, cmd.read_buf)
            .map_err(|_| CoreError::ProgrammerError)
    }

    async fn delay_us(&mut self, us: u32) {
        std::thread::sleep(std::time::Duration::from_micros(us as u64));
    }

    async fn read_flash(
        &mut self,
        ctx: &FlashContext,
        addr: u32,
        buf: &mut [u8],
    ) -> CoreResult<()> {
        if !Self::supports_bulk_read(ctx) {
            return flash::spi_read(self, ctx, addr, buf).await;
        }
        let four_byte = ctx.address_mode == AddressMode::FourByte;
        self.fast_read(addr, buf, four_byte)
            .map_err(|_| CoreError::ReadError { addr })
    }

    async fn program_flash(
        &mut self,
        ctx: &FlashContext,
        addr: u32,
        data: &[u8],
    ) -> CoreResult<()> {
        if !Self::supports_bulk_write(ctx) {
            return flash::spi_program(self, ctx, addr, data).await;
        }
        // Batched page program with on-SoC busy-wait after every page.
        let four_byte = ctx.address_mode == AddressMode::FourByte;
        self.batched_write(addr, data, 256, four_byte)
            .map_err(|_| CoreError::WriteError { addr })
    }
}

#[cfg(test)]
mod tests;

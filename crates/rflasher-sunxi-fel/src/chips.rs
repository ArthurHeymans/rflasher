//! Allwinner SoC chip definitions and SPI payload management
//!
//! Each supported SoC has a pre-compiled SPI driver payload (from xfel)
//! that runs natively on the SoC. The payload handles all hardware init
//! (CCU clocks, GPIO pin mux, SPI controller registers) and implements
//! a bytecode interpreter for SPI transactions.
//!
//! The host uploads the payload via FEL, then drives SPI by writing
//! command bytecodes to a command buffer and executing the payload.

use crate::error::{Error, Result};
use crate::protocol::FelTransport;

/// SPI command bytecodes (interpreted by the on-SoC payload)
pub mod spi_cmd {
    pub const END: u8 = 0x00;
    pub const INIT: u8 = 0x01;
    pub const SELECT: u8 = 0x02;
    pub const DESELECT: u8 = 0x03;
    pub const FAST: u8 = 0x04;
    pub const TXBUF: u8 = 0x05;
    pub const RXBUF: u8 = 0x06;
    pub const SPINOR_WAIT: u8 = 0x07;
}

/// Memory layout for the SPI payload on the target SoC
#[derive(Debug, Clone, Copy)]
pub struct SpiPayloadInfo {
    /// Address where the payload code is loaded
    pub payload_addr: u32,
    /// Address of the command buffer (bytecodes written here)
    pub cmdbuf_addr: u32,
    /// Address of the swap buffer (TX/RX data exchanged here)
    pub swapbuf: u32,
    /// Size of the swap buffer in bytes
    pub swaplen: u32,
    /// Maximum command buffer length
    pub cmdlen: u32,
}

/// Supported Allwinner SoC chip families
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipFamily {
    /// D1/F133 (sun20iw1) - RISC-V C906
    D1,
    // Future: H2H3, V3sS3, F1C100s, R528, etc.
}

impl ChipFamily {
    /// Human-readable name for the SoC
    pub fn name(&self) -> &'static str {
        match self {
            ChipFamily::D1 => "D1/F133",
        }
    }
}

/// Detect chip family from FEL version ID
pub fn detect_chip(id: u32) -> Option<ChipFamily> {
    match id {
        0x00185900 => Some(ChipFamily::D1),
        _ => None,
    }
}

/// D1/F133 SPI payload (1480 bytes, RISC-V machine code from xfel)
///
/// This payload initializes CCU clocks, GPIO pin mux, and the SPI0
/// controller, then enters a bytecode interpreter loop that processes
/// SPI_CMD_* commands from the command buffer at 0x00021000.
const D1_SPI_PAYLOAD: &[u8] = include_bytes!("payloads/d1_f133.bin");

/// Get the SPI payload and memory layout for a chip
pub fn spi_payload(chip: ChipFamily) -> (&'static [u8], SpiPayloadInfo) {
    match chip {
        ChipFamily::D1 => (
            D1_SPI_PAYLOAD,
            SpiPayloadInfo {
                payload_addr: 0x00020000,
                cmdbuf_addr: 0x00021000,
                swapbuf: 0x00022000,
                swaplen: 65536,
                cmdlen: 4096,
            },
        ),
    }
}

/// Upload the SPI payload and send SPI_CMD_INIT
pub fn spi_init(transport: &mut FelTransport, chip: ChipFamily) -> Result<SpiPayloadInfo> {
    let (payload, info) = spi_payload(chip);

    // Upload payload to target SRAM
    transport.fel_write(info.payload_addr, payload)?;
    log::debug!(
        "Uploaded {} byte SPI payload to 0x{:08x}",
        payload.len(),
        info.payload_addr
    );

    // Send SPI_CMD_INIT to initialize hardware (clocks, GPIO, SPI controller)
    let init_cmd = [spi_cmd::INIT, spi_cmd::END];
    transport.fel_write(info.cmdbuf_addr, &init_cmd)?;
    transport.fel_exec(info.payload_addr)?;
    log::debug!("SPI_CMD_INIT complete");

    Ok(info)
}

/// Write command buffer and execute the SPI payload
pub fn spi_run(transport: &mut FelTransport, info: &SpiPayloadInfo, cbuf: &[u8]) -> Result<()> {
    if cbuf.len() as u32 > info.cmdlen {
        return Err(Error::Protocol(format!(
            "SPI command buffer too large: {} > {}",
            cbuf.len(),
            info.cmdlen
        )));
    }
    transport.fel_write(info.cmdbuf_addr, cbuf)?;
    transport.fel_exec(info.payload_addr)
}

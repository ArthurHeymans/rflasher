//! RPC endpoint handlers for postcard-spi

use crate::{Context, DEVICE_NAME, MAX_TRANSFER_SIZE, NUM_CS};
use defmt::{debug, trace, warn};
use postcard_rpc::header::VarHeader;
use postcard_spi_icd::*;

/// Handler for GetInfo endpoint
pub fn get_info_handler(context: &mut Context, _header: VarHeader, _req: ()) -> DeviceInfo {
    debug!("GetInfo request");

    DeviceInfo {
        name: *DEVICE_NAME,
        version: PROTOCOL_VERSION,
        max_transfer_size: MAX_TRANSFER_SIZE,
        num_cs: NUM_CS,
        current_cs: context.current_cs,
        supported_modes: IoModeFlags::new(
            IoModeFlags::SINGLE
                | IoModeFlags::DUAL_OUT
                | IoModeFlags::DUAL_IO
                | IoModeFlags::QUAD_OUT
                | IoModeFlags::QUAD_IO
                | IoModeFlags::QPI,
        ),
        current_speed_hz: context.current_speed_hz,
    }
}

/// Handler for SetSpeed endpoint
pub fn set_speed_handler(
    context: &mut Context,
    _header: VarHeader,
    req: SetSpeedReq,
) -> SetSpeedResp {
    debug!("SetSpeed request: {} Hz", req.hz);

    // Calculate actual achievable speed
    // RP2040 runs at 125 MHz by default, PIO divider is 16-bit with 8-bit fractional
    // For SPI, we need 2 PIO cycles per SCK cycle (rise + fall)
    let sys_clk = 125_000_000u32;
    let min_div = 1u32;
    let max_div = 65535u32;

    // SPI clock = sys_clk / (2 * divider)
    // divider = sys_clk / (2 * spi_clk)
    let target_div = sys_clk / (2 * req.hz.max(1));
    let actual_div = target_div.clamp(min_div, max_div);
    let actual_hz = sys_clk / (2 * actual_div);

    context.current_speed_hz = actual_hz;
    context.qspi.set_clock_divider(actual_div as u16);

    debug!("SetSpeed: requested {} Hz, actual {} Hz", req.hz, actual_hz);

    SetSpeedResp { actual_hz }
}

/// Handler for SetCs endpoint
pub fn set_cs_handler(context: &mut Context, _header: VarHeader, req: SetCsReq) {
    debug!("SetCs request: CS{}", req.cs);

    if req.cs >= NUM_CS {
        warn!("Invalid CS: {} (max {})", req.cs, NUM_CS - 1);
        return;
    }

    // Deassert current CS before switching
    context.cs_deassert_all();
    context.current_cs = req.cs;
}

/// Handler for Delay endpoint
pub fn delay_handler(_context: &mut Context, _header: VarHeader, req: DelayReq) {
    trace!("Delay request: {} us", req.us);

    // Use blocking delay for short delays, async for longer ones
    // Note: This is a blocking handler, so we can't use async directly
    // For now, use cortex_m delay
    let cycles = (req.us as u64 * 125) as u32; // 125 MHz clock
    cortex_m::asm::delay(cycles);
}

/// Handler for SpiTransfer endpoint (no data, just parameters)
pub fn spi_transfer_handler(
    context: &mut Context,
    _header: VarHeader,
    req: SpiTransferReq,
) -> SpiTransferResp {
    trace!(
        "SpiTransfer: opcode={:#04x}, addr={:?}, mode={:?}, write={}, read={}",
        req.opcode,
        req.address,
        req.io_mode,
        req.write_len,
        req.read_len
    );

    // For transfers without data, just send the opcode/address/dummy
    // This is mainly for simple commands like WREN, WRDI, etc.

    context.cs_assert();

    // Send opcode
    context.qspi.write_byte(req.opcode, req.io_mode);

    // Send address if present
    if let Some(addr) = req.address {
        let addr_bytes = match req.address_width {
            AddressWidth::ThreeByte => 3,
            AddressWidth::FourByte => 4,
            AddressWidth::None => 0,
        };

        for i in (0..addr_bytes).rev() {
            let byte = ((addr >> (i * 8)) & 0xFF) as u8;
            context.qspi.write_byte(byte, req.io_mode);
        }
    }

    // Send dummy cycles
    for _ in 0..req.dummy_cycles {
        context.qspi.write_byte(0xFF, req.io_mode);
    }

    context.cs_deassert();

    SpiTransferResp {
        success: true,
        bytes_read: 0,
    }
}

/// Handler for SpiTransferData endpoint (with inline data)
pub fn spi_transfer_data_handler(
    context: &mut Context,
    _header: VarHeader,
    req: SpiTransferReqWithData,
) -> SpiTransferRespWithData {
    trace!(
        "SpiTransferData: opcode={:#04x}, write={}, read={}",
        req.req.opcode,
        req.write_data.len(),
        req.req.read_len
    );

    let read_len = (req.req.read_len as usize).min(MAX_INLINE_DATA);

    context.cs_assert();

    // Determine I/O mode for each phase
    let cmd_mode = match req.req.io_mode {
        IoMode::Qpi => IoMode::Qpi,
        _ => IoMode::Single, // Command is always single-line except in QPI
    };

    let addr_mode = match req.req.io_mode {
        IoMode::Single | IoMode::DualOut | IoMode::QuadOut => IoMode::Single,
        IoMode::DualIo => IoMode::DualIo,
        IoMode::QuadIo => IoMode::QuadIo,
        IoMode::Qpi => IoMode::Qpi,
    };

    let data_mode = req.req.io_mode;

    // Send opcode (always single-line except QPI)
    context.qspi.write_byte(req.req.opcode, cmd_mode);

    // Send address if present
    if let Some(addr) = req.req.address {
        let addr_bytes = match req.req.address_width {
            AddressWidth::ThreeByte => 3,
            AddressWidth::FourByte => 4,
            AddressWidth::None => 0,
        };

        for i in (0..addr_bytes).rev() {
            let byte = ((addr >> (i * 8)) & 0xFF) as u8;
            context.qspi.write_byte(byte, addr_mode);
        }
    }

    // Send dummy cycles (in address mode width for multi-IO)
    let dummy_bytes = req.req.dummy_cycles.div_ceil(8);
    for _ in 0..dummy_bytes {
        context.qspi.write_byte(0xFF, addr_mode);
    }

    // Write data
    for byte in req.write_data.iter() {
        context.qspi.write_byte(*byte, data_mode);
    }

    // Read data into heapless::Vec
    let mut read_data = heapless::Vec::<u8, MAX_INLINE_DATA>::new();
    for _ in 0..read_len {
        let _ = read_data.push(context.qspi.read_byte(data_mode));
    }

    context.cs_deassert();

    SpiTransferRespWithData {
        resp: SpiTransferResp {
            success: true,
            bytes_read: read_data.len() as u16,
        },
        read_data,
    }
}

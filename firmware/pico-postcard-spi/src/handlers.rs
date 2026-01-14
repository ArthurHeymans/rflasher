//! RPC endpoint handlers for postcard-spi

use crate::{Context, DEVICE_NAME, MAX_TRANSFER_SIZE, NUM_CS};
use defmt::{debug, trace};
use postcard_rpc::header::VarHeader;
use postcard_spi_icd::{
    AddressWidth, BatchError, BatchOp, BatchOpResult, BatchRequest, BatchResponse, DeviceInfo,
    IoMode, IoModeFlags, SetSpeedReq, SetSpeedResp, SpiTransaction, MAX_BATCH_READS,
    MAX_BATCH_READ_SIZE, PROTOCOL_VERSION,
};

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

/// Handler for batch operations
///
/// Executes multiple SPI transactions in sequence with minimal overhead.
/// This is the primary interface for high-performance flash programming.
///
/// Each transaction is complete with automatic CS handling, making this
/// compatible with hardware SPI controllers that don't support arbitrary
/// CS manipulation.
pub fn batch_handler(
    context: &mut Context,
    _header: VarHeader,
    req: BatchRequest,
) -> BatchResponse {
    trace!("Batch request: {} ops", req.ops.len());

    let mut results = heapless::Vec::<BatchOpResult, MAX_BATCH_READS>::new();
    let mut ops_completed: u8 = 0;

    for op in req.ops.iter() {
        let result = execute_batch_op(context, op);

        // Track completion
        ops_completed += 1;

        // Only store results that return data or indicate special status
        match &result {
            BatchOpResult::Ok => {
                // Don't store - no data to return
            }
            BatchOpResult::Data(_)
            | BatchOpResult::PollOk(_)
            | BatchOpResult::PollTimeout(_)
            | BatchOpResult::Error(_) => {
                if results.push(result.clone()).is_err() {
                    // Results buffer full - return what we have
                    return BatchResponse {
                        results,
                        ops_completed,
                        success: false,
                    };
                }
            }
        }

        // Stop on error (but not on poll timeout - that's reported but not fatal)
        if matches!(result, BatchOpResult::Error(_)) {
            return BatchResponse {
                results,
                ops_completed,
                success: false,
            };
        }
    }

    BatchResponse {
        results,
        ops_completed,
        success: true,
    }
}

/// Execute a single batch operation
fn execute_batch_op(context: &mut Context, op: &BatchOp) -> BatchOpResult {
    match op {
        BatchOp::Transact(tx) => execute_transaction(context, tx),

        BatchOp::DelayUs(us) => {
            let cycles = (*us as u64 * 125) as u32; // 125 MHz clock
            cortex_m::asm::delay(cycles);
            BatchOpResult::Ok
        }

        BatchOp::Poll {
            cmd,
            mask,
            expected,
            timeout_ms,
        } => {
            // Poll by repeatedly sending command and reading status
            // Use simple timing based on system clock
            let timeout_cycles = (*timeout_ms as u64) * 125_000; // 125 MHz = 125k cycles/ms
            let mut elapsed: u64 = 0;
            let poll_interval = 100u32; // cycles between polls

            loop {
                // Execute a complete transaction: CS assert, cmd, read, CS deassert
                context.cs_assert();
                context.qspi.write_byte(*cmd, IoMode::Single);
                let status = context.qspi.read_byte(IoMode::Single);
                context.cs_deassert();

                // Check condition
                if (status & mask) == *expected {
                    return BatchOpResult::PollOk(status);
                }

                // Check timeout
                elapsed += poll_interval as u64;
                if elapsed >= timeout_cycles {
                    return BatchOpResult::PollTimeout(status);
                }

                // Small delay between polls
                cortex_m::asm::delay(poll_interval);
            }
        }

        BatchOp::SetCs(cs) => {
            if *cs >= NUM_CS {
                return BatchOpResult::Error(BatchError::InvalidCs);
            }
            context.cs_deassert_all();
            context.current_cs = *cs;
            BatchOpResult::Ok
        }
    }
}

/// Execute a complete SPI transaction
///
/// This handles CS automatically and supports the full command structure:
/// opcode -> address -> dummy -> write/read data
fn execute_transaction(context: &mut Context, tx: &SpiTransaction) -> BatchOpResult {
    // Determine I/O modes for each phase based on the transaction's io_mode
    let cmd_mode = match tx.io_mode {
        IoMode::Qpi => IoMode::Qpi,
        _ => IoMode::Single, // Command is always single-line except in QPI
    };

    let addr_mode = match tx.io_mode {
        IoMode::Single | IoMode::DualOut | IoMode::QuadOut => IoMode::Single,
        IoMode::DualIo => IoMode::DualIo,
        IoMode::QuadIo => IoMode::QuadIo,
        IoMode::Qpi => IoMode::Qpi,
    };

    let data_mode = tx.io_mode;

    // Start transaction
    context.cs_assert();

    // Send opcode
    context.qspi.write_byte(tx.opcode, cmd_mode);

    // Send address if present
    if let Some(addr) = tx.address {
        let addr_bytes = match tx.address_width {
            AddressWidth::ThreeByte => 3,
            AddressWidth::FourByte => 4,
            AddressWidth::None => 0,
        };

        for i in (0..addr_bytes).rev() {
            let byte = ((addr >> (i * 8)) & 0xFF) as u8;
            context.qspi.write_byte(byte, addr_mode);
        }
    }

    // Send dummy cycles (as bytes in the appropriate mode)
    let dummy_bytes = tx.dummy_cycles.div_ceil(8);
    for _ in 0..dummy_bytes {
        context.qspi.write_byte(0xFF, addr_mode);
    }

    // Write data phase (if any)
    for byte in tx.write_data.iter() {
        context.qspi.write_byte(*byte, data_mode);
    }

    // Read data phase (if any)
    let result = if tx.read_len > 0 {
        let mut data = heapless::Vec::<u8, MAX_BATCH_READ_SIZE>::new();
        for _ in 0..tx.read_len {
            if data.push(context.qspi.read_byte(data_mode)).is_err() {
                context.cs_deassert();
                return BatchOpResult::Error(BatchError::BufferOverflow);
            }
        }
        BatchOpResult::Data(data)
    } else {
        BatchOpResult::Ok
    };

    // End transaction
    context.cs_deassert();

    result
}

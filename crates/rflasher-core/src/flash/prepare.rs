//! Flash preparation and teardown
//!
//! Mirrors flashprog's `spi_prepare_io` / `spi_finish_io` (`spi25_prepare.c`).
//!
//! Before any flash operation, we need to:
//!
//! 1. Set 4-byte addressing mode if the chip requires it.
//! 2. Set the Quad-Enable bit if the programmer + chip both support any
//!    quad-IO read, so that IO2/IO3 become functional.
//! 3. Optionally enter QPI (4-4-4) mode if both sides support it.
//! 4. Pick the fastest read operation available and cache it on the context.
//!
//! On finish, undo the steps that have side-effects on the chip.

use crate::chip::Features;
use crate::error::Result;
use crate::flash::context::{AddressMode, FlashContext};
use crate::programmer::{SpiFeatures, SpiMaster};
use crate::protocol::{
    self, ChipReadCapabilities, DummyCycleOverrides, QuadEnableMethod, SpiReadOp,
};
use maybe_async::maybe_async;

/// Cached per-session state assembled by `prepare_io`.
///
/// Stored alongside the `FlashContext` (not mutated at read-time) so the
/// flash device can dispatch to the right read op and can undo side-effects
/// in `finish_io` (exit QPI, exit 4-byte addressing).
#[derive(Debug, Clone, Copy)]
pub struct PreparedState {
    /// Chosen read operation (opcode + mode + dummy cycles + address width)
    pub read_op: SpiReadOp,
    /// True if QPI mode is active for this session.
    pub in_qpi_mode: bool,
    /// Opcode to use when exiting QPI (0x00 if we didn't enter QPI).
    pub qpi_exit_opcode: u8,
    /// True if 4-byte addressing was entered via EN4B and needs EX4B on finish.
    pub entered_4ba: bool,
}

impl Default for PreparedState {
    fn default() -> Self {
        Self {
            read_op: SpiReadOp::sio_read(),
            in_qpi_mode: false,
            qpi_exit_opcode: 0x00,
            entered_4ba: false,
        }
    }
}

impl PreparedState {
    /// Create a session state for a chip when no programmer is available
    /// (e.g. WASM probe-only contexts). Falls back to slow single-I/O 0x03.
    pub fn default_for(_ctx: &FlashContext) -> Self {
        Self::default()
    }
}

fn chip_to_caps(features: Features, in_qpi_mode: bool) -> ChipReadCapabilities {
    ChipReadCapabilities {
        fast_read: features.contains(Features::FAST_READ),
        dout: features.contains(Features::FAST_READ_DOUT),
        dio: features.contains(Features::FAST_READ_DIO),
        qout: features.contains(Features::FAST_READ_QOUT),
        qio: features.contains(Features::FAST_READ_QIO),
        qpi_fast_read: features.intersects(Features::ANY_QPI) || in_qpi_mode,
        qpi4b: features.contains(Features::FAST_READ_QPI4B),
        native_4ba_read: features.contains(Features::FOUR_BYTE_NATIVE),
        in_qpi_mode,
    }
}

fn chip_qe_method(ctx: &FlashContext) -> QuadEnableMethod {
    use crate::chip::QeMethod;
    match ctx.chip.qe_method {
        QeMethod::None => QuadEnableMethod::None,
        QeMethod::Sr2Bit1WriteSr => QuadEnableMethod::Sr2Bit1WriteSr,
        QeMethod::Sr2Bit1WriteSr2 => QuadEnableMethod::Sr2Bit1WriteSr2,
        QeMethod::Sr1Bit6 => QuadEnableMethod::Sr1Bit6,
        QeMethod::Sr2Bit7 => QuadEnableMethod::Sr2Bit7,
    }
}

/// Prepare a flash chip for multi-IO operation.
///
/// Should be called after `probe`-ing and constructing the `FlashContext`,
/// but before issuing any read/write/erase. Returns a `PreparedState` that
/// the flash-device layer should carry for the session's lifetime.
///
/// If any step fails (QE-write rejected, QPI entry fails), the function
/// degrades gracefully to the slowest mode that still works — we'd rather
/// read at 1-1-1 than bail out entirely.
#[maybe_async]
pub async fn prepare_io<M: SpiMaster + ?Sized>(
    ctx: &mut FlashContext,
    master: &mut M,
) -> Result<PreparedState> {
    let master_features = master.features();
    let chip_features = ctx.chip.features;

    // ------ 1. 4-byte addressing mode ------
    let entered_4ba = ctx.address_mode == AddressMode::FourByte
        && !ctx.use_native_4byte
        && chip_features.contains(Features::FOUR_BYTE_ENTER);
    if entered_4ba {
        if let Err(e) = protocol::enter_4byte_mode(master).await {
            log::warn!("enter_4byte_mode failed: {e:?}; falling back to 3-byte addressing");
        }
    }

    // ------ 2. Quad Enable ------
    let mut effective_chip_features = chip_features;
    let wants_quad = chip_features.intersects(Features::ANY_QUAD)
        && master_features
            .intersects(SpiFeatures::QUAD_IN | SpiFeatures::QUAD_IO | SpiFeatures::QPI);
    if wants_quad {
        let method = chip_qe_method(ctx);
        match protocol::is_quad_enabled(master, method).await {
            Ok(true) => {
                log::debug!("QE bit already set for {}", ctx.chip.name);
            }
            Ok(false) => {
                log::info!("Setting QE bit for {}", ctx.chip.name);
                if let Err(e) = protocol::enable_quad_mode(master, method).await {
                    log::warn!(
                        "Failed to enable QE bit ({e:?}); disabling quad read modes for this session"
                    );
                    effective_chip_features &= !Features::ANY_QUAD;
                }
            }
            Err(e) => {
                log::warn!(
                    "Could not read QE bit ({e:?}); disabling quad read modes for this session"
                );
                effective_chip_features &= !Features::ANY_QUAD;
            }
        }
    }

    // ------ 3. QPI entry ------
    //
    // QPI (4-4-4) mode rewires ALL SPI commands — including status reads,
    // WREN, erase, and program — into 4-wire framing. The rest of our
    // flash operation pipeline (erase/write/WP/status polling paths in
    // `protocol::spi25` and the programmer `execute()` path) currently
    // issues single-IO commands unconditionally. Entering QPI here and
    // leaving it enabled across those operations would hang or corrupt.
    //
    // For now we deliberately skip QPI entry. The fine-grained 1-2-2 /
    // 1-1-4 / 1-4-4 paths already give the bulk of the speedup. QPI
    // support will require a broader refactor that brackets non-read ops
    // with `exit_qpi_with` / `enter_qpi_with`, mirroring flashprog's
    // `spi_prepare_io` / `spi_finish_io`.
    let in_qpi_mode = false;
    let qpi_exit_opcode = 0x00u8;
    if effective_chip_features.intersects(Features::ANY_QPI)
        && master_features.contains(SpiFeatures::QPI)
    {
        log::debug!(
            "QPI supported by both master and {} but not enabled (QPI-aware \
             non-read command paths not yet implemented)",
            ctx.chip.name,
        );
    }

    // ------ 4. Pick read op ------
    let caps = chip_to_caps(effective_chip_features, in_qpi_mode);
    let dc = DummyCycleOverrides {
        dc_112: ctx.chip.dummy_cycles_112,
        dc_122: ctx.chip.dummy_cycles_122,
        dc_114: ctx.chip.dummy_cycles_114,
        dc_144: ctx.chip.dummy_cycles_144,
        dc_qpi: ctx.chip.dummy_cycles_qpi,
    };
    let use_4byte = ctx.address_mode == AddressMode::FourByte && ctx.use_native_4byte;
    let read_op = protocol::select_read_op(master_features, caps, dc, use_4byte);

    log::debug!(
        "Selected read op for {}: opcode=0x{:02X} io_mode={:?} dummy={} native_4ba={}",
        ctx.chip.name,
        read_op.opcode,
        read_op.io_mode,
        read_op.dummy_cycles,
        read_op.native_4ba,
    );

    Ok(PreparedState {
        read_op,
        in_qpi_mode,
        qpi_exit_opcode,
        entered_4ba,
    })
}

/// Undo side-effects from `prepare_io`.
///
/// Safe to call even if `prepare_io` partially failed — each step checks
/// whether the corresponding entry was taken.
///
/// Note: the QE bit (when set) is left programmed non-volatile across
/// sessions — matches flashprog's default behavior and avoids disrupting
/// users who rely on the bit persisting.
#[maybe_async]
pub async fn finish_io<M: SpiMaster + ?Sized>(
    state: &PreparedState,
    master: &mut M,
) -> Result<()> {
    if state.in_qpi_mode {
        if let Err(e) = protocol::exit_qpi_with(master, state.qpi_exit_opcode).await {
            log::warn!("exit_qpi_with failed: {e:?}");
        }
    }
    if state.entered_4ba {
        if let Err(e) = protocol::exit_4byte_mode(master).await {
            log::warn!("exit_4byte_mode failed: {e:?}");
        }
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(all(test, feature = "is_sync", feature = "alloc"))]
mod tests {
    use super::*;
    use crate::chip::{EraseBlock, FlashChip, QeMethod, WriteGranularity};
    use crate::error::Result as CoreResult;
    use crate::programmer::SpiMaster;
    use crate::spi::{opcodes, SpiCommand};
    use alloc::vec;
    use alloc::vec::Vec;

    /// Minimal mock programmer that records every executed opcode and can
    /// have canned responses for register reads. Used to verify the prepare
    /// flow issues the right commands in the right order.
    struct MockMaster {
        features: SpiFeatures,
        sr1: u8,
        sr2: u8,
        executed: Vec<u8>,
    }

    impl MockMaster {
        fn new(features: SpiFeatures) -> Self {
            Self {
                features,
                sr1: 0,
                sr2: 0,
                executed: Vec::new(),
            }
        }
    }

    impl SpiMaster for MockMaster {
        fn features(&self) -> SpiFeatures {
            self.features
        }
        fn max_read_len(&self) -> usize {
            usize::MAX
        }
        fn max_write_len(&self) -> usize {
            usize::MAX
        }
        fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> CoreResult<()> {
            self.executed.push(cmd.opcode);
            match cmd.opcode {
                // RDSR1: return sr1
                opcodes::RDSR => {
                    if !cmd.read_buf.is_empty() {
                        cmd.read_buf[0] = self.sr1;
                    }
                }
                // RDSR2: return sr2
                opcodes::RDSR2 => {
                    if !cmd.read_buf.is_empty() {
                        cmd.read_buf[0] = self.sr2;
                    }
                }
                // WRSR: SR1 followed optionally by SR2
                opcodes::WRSR => {
                    if cmd.write_data.len() >= 1 {
                        self.sr1 = cmd.write_data[0];
                    }
                    if cmd.write_data.len() >= 2 {
                        self.sr2 = cmd.write_data[1];
                    }
                }
                // WRSR2
                opcodes::WRSR2 => {
                    if !cmd.write_data.is_empty() {
                        self.sr2 = cmd.write_data[0];
                    }
                }
                _ => {}
            }
            Ok(())
        }
        fn delay_us(&mut self, _us: u32) {}
    }

    fn test_chip_quad() -> FlashChip {
        FlashChip {
            vendor: "Test".into(),
            name: "TQ".into(),
            jedec_manufacturer: 0xEF,
            jedec_device: 0x4018,
            total_size: 16 * 1024 * 1024,
            page_size: 256,
            features: crate::chip::Features::FAST_READ
                | crate::chip::Features::FAST_READ_DOUT
                | crate::chip::Features::FAST_READ_DIO
                | crate::chip::Features::FAST_READ_QOUT
                | crate::chip::Features::FAST_READ_QIO,
            voltage_min_mv: 2700,
            voltage_max_mv: 3600,
            write_granularity: WriteGranularity::Page,
            erase_blocks: vec![EraseBlock::new(0xC7, 16 * 1024 * 1024)],
            tested: Default::default(),
            qe_method: QeMethod::Sr2Bit1WriteSr2,
            dummy_cycles_112: 0,
            dummy_cycles_122: 0,
            dummy_cycles_114: 0,
            dummy_cycles_144: 0,
            dummy_cycles_qpi: 0,
        }
    }

    #[test]
    fn prepare_enables_qe_when_not_set() {
        let chip = test_chip_quad();
        let mut ctx = FlashContext::new(chip);
        let mut master = MockMaster::new(
            SpiFeatures::QUAD_IN | SpiFeatures::QUAD_IO | SpiFeatures::FOUR_BYTE_ADDR,
        );
        // sr2 starts at 0, so QE bit is off -> should trigger a write.
        let state = prepare_io(&mut ctx, &mut master).unwrap();
        // With QIO flag set and master supports quad, we expect QuadIo read op.
        assert_eq!(state.read_op.io_mode, crate::spi::IoMode::QuadIo);
        // Check that RDSR2 was read and WRSR2 was issued.
        assert!(master.executed.contains(&opcodes::RDSR2));
        assert!(master.executed.contains(&opcodes::WRSR2));
        // QE bit should now be set.
        assert_eq!(master.sr2 & opcodes::SR2_QE, opcodes::SR2_QE);
    }

    #[test]
    fn prepare_skips_qe_when_already_set() {
        let chip = test_chip_quad();
        let mut ctx = FlashContext::new(chip);
        let mut master = MockMaster::new(
            SpiFeatures::QUAD_IN | SpiFeatures::QUAD_IO | SpiFeatures::FOUR_BYTE_ADDR,
        );
        // Pre-set QE bit.
        master.sr2 = opcodes::SR2_QE;
        prepare_io(&mut ctx, &mut master).unwrap();
        // RDSR2 read, but WRSR2 not issued.
        assert!(master.executed.contains(&opcodes::RDSR2));
        assert!(!master.executed.contains(&opcodes::WRSR2));
    }

    #[test]
    fn prepare_falls_back_to_single_without_quad_master() {
        let chip = test_chip_quad();
        let mut ctx = FlashContext::new(chip);
        let mut master = MockMaster::new(SpiFeatures::FOUR_BYTE_ADDR);
        let state = prepare_io(&mut ctx, &mut master).unwrap();
        assert_eq!(state.read_op.io_mode, crate::spi::IoMode::Single);
        // No QE interaction without quad master.
        assert!(!master.executed.contains(&opcodes::RDSR2));
    }
}

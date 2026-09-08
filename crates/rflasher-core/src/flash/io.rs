//! Immutable command plans and the single owner of temporary flash state.
//!
//! Construction/probe assumes an externally established ordinary-SPI, three-byte
//! baseline (and no command in flight). JEDEC probing does not reset a chip.
//! Cancellation requires hardware recovery and reprobe, not merely reconnecting USB.

use super::context::{AddressMode, FlashContext};
use crate::chip::{EraseBlock, Features, WriteGranularity};
use crate::error::{EraseFailure, Error, Result};
use crate::programmer::{SpiFeatures, SpiMaster};
use crate::protocol::{self, CommandAddressing, QuadEnableMethod, SpiReadOp};
use crate::spi::{AddressWidth, IoMode, SpiCommand, opcodes};

/// Command addressing, not a cache of the chip's current mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressPlan {
    /// Ordinary three-byte commands.
    ThreeByte,
    /// Dedicated four-byte opcodes; no chip mode change.
    NativeFourByte,
    /// Bracket compatibility commands with chip-specific mode entry/exit.
    EnterFourByte(Features),
    /// Three-byte commands with explicit bank selection and saved EAR restoration.
    Ear(Features),
}
impl AddressPlan {
    /// Address strategy consumed by raw SPI protocol helpers.
    pub fn command_addressing(self) -> CommandAddressing {
        match self {
            Self::ThreeByte => CommandAddressing::ThreeByte,
            Self::NativeFourByte | Self::EnterFourByte(_) => CommandAddressing::FourByte,
            Self::Ear(f) => CommandAddressing::ExtendedAddressRegister(f),
        }
    }
    /// Reject address truncation before submitting a planned transfer.
    pub fn validate_range(self, addr: u32, len: usize) -> Result<()> {
        let end = addr as u64 + len as u64;
        if end > (1u64 << 32) || (self == Self::ThreeByte && end > 0x0100_0000) {
            Err(Error::AddressOutOfBounds)
        } else {
            Ok(())
        }
    }
    /// Number of address bytes on the wire.
    pub fn width(self) -> AddressWidth {
        match self {
            Self::NativeFourByte | Self::EnterFourByte(_) => AddressWidth::FourByte,
            _ => AddressWidth::ThreeByte,
        }
    }
}

/// One read command and its residual path. Construct with `select_read_plan`.
#[derive(Debug, Clone, Copy)]
pub struct ReadPlan {
    /// Selected command and exact dummy timing.
    pub op: SpiReadOp,
    /// Addressing shared by every chunk of this operation.
    pub address: AddressPlan,
    /// A valid single-I/O residual command under the same addressing strategy.
    pub single_read: Option<SpiReadOp>,
    pub(crate) qe: QuadEnableMethod,
}
/// Program command selected independently from reads.
#[derive(Debug, Clone, Copy)]
pub struct WritePlan {
    /// Command opcode sent to the flash.
    pub opcode: u8,
    /// Addressing shared by every chunk of this operation.
    pub address: AddressPlan,
    /// Chip page boundary in bytes.
    pub page_size: usize,
    /// Minimum programming unit.
    pub granularity: WriteGranularity,
}
/// Erase command, layout and polling timing.
#[derive(Debug, Clone)]
pub struct ErasePlan {
    /// Command opcode sent to the flash.
    pub opcode: u8,
    /// Addressing shared by every chunk of this operation.
    pub address: AddressPlan,
    /// Validated chip erase layout.
    pub block: EraseBlock,
    /// Delay between status polls.
    pub poll_delay_us: u32,
    /// Erase completion timeout.
    pub timeout_us: u32,
}

fn addresses(ctx: &FlashContext, features: SpiFeatures, native: bool) -> [Option<AddressPlan>; 3] {
    if ctx.address_mode == AddressMode::ThreeByte {
        return [Some(AddressPlan::ThreeByte), None, None];
    }
    let f = ctx.chip.features;
    [
        (native && features.contains(SpiFeatures::FOUR_BYTE_ADDR))
            .then_some(AddressPlan::NativeFourByte),
        (!features.contains(SpiFeatures::NO_4BA_MODES)
            && features.contains(SpiFeatures::FOUR_BYTE_ADDR)
            && f.supports_4ba_mode_switch())
        .then_some(AddressPlan::EnterFourByte(f)),
        (!features.contains(SpiFeatures::NO_4BA_MODES) && f.supports_extended_address_register())
            .then_some(AddressPlan::Ear(f)),
    ]
}

fn qe_method(ctx: &FlashContext) -> QuadEnableMethod {
    use crate::chip::QeMethod;
    match ctx.chip.qe_method {
        QeMethod::None => QuadEnableMethod::None,
        QeMethod::Sr2Bit1WriteSr => QuadEnableMethod::Sr2Bit1WriteSr,
        QeMethod::Sr2Bit1WriteSr2 => QuadEnableMethod::Sr2Bit1WriteSr2,
        QeMethod::Sr1Bit6 => QuadEnableMethod::Sr1Bit6,
        QeMethod::Sr2Bit7 => QuadEnableMethod::Sr2Bit7,
    }
}

/// Pure selection, including the backend's complete bulk/residual representability.
pub fn select_read_plan<M: SpiMaster + ?Sized>(
    master: &M,
    ctx: &FlashContext,
    single: bool,
    accepts: impl Fn(&ReadPlan) -> bool,
) -> Result<ReadPlan> {
    use protocol::{ChipReadCapabilities, DummyCycleOverrides};
    if master.max_read_len() == 0 {
        return Err(Error::ChipNotSupported);
    }
    let f = ctx.chip.features;
    let qe = qe_method(ctx);
    // Unknown SFDP QER removes quad capabilities during conversion. Without
    // volatile writes, conservatively exclude quad even if QE might be set.
    let quad = !single
        && qe != QuadEnableMethod::Sr2Bit7
        && (qe == QuadEnableMethod::None || f.contains(Features::WRSR_EWSR));
    let dc = DummyCycleOverrides {
        dc_112: ctx.chip.dummy_cycles_112,
        dc_122: ctx.chip.dummy_cycles_122,
        dc_114: ctx.chip.dummy_cycles_114,
        dc_144: ctx.chip.dummy_cycles_144,
        dc_qpi: None,
    };
    let exact = |mode, cycles: Option<u8>, default| {
        master.supports_read_dummy_cycles(mode, cycles.unwrap_or(default))
    };
    let caps = ChipReadCapabilities {
        fast_read: f.contains(Features::FAST_READ) && exact(IoMode::Single, Some(8), 8),
        dout: !single
            && f.contains(Features::FAST_READ_DOUT)
            && exact(IoMode::DualOut, dc.dc_112, 8),
        dio: !single && f.contains(Features::FAST_READ_DIO) && exact(IoMode::DualIo, dc.dc_122, 4),
        qout: quad && f.contains(Features::FAST_READ_QOUT) && exact(IoMode::QuadOut, dc.dc_114, 8),
        qio: quad && f.contains(Features::FAST_READ_QIO) && exact(IoMode::QuadIo, dc.dc_144, 6),
        native_4ba_read: f.contains(Features::FOUR_BYTE_READ),
        native_4ba_fast_read: f.contains(Features::FOUR_BYTE_FAST_READ),
        native_4ba_dout: f.contains(Features::FOUR_BYTE_DUAL_OUT_READ),
        native_4ba_dio: f.contains(Features::FOUR_BYTE_DUAL_IO_READ),
        native_4ba_qout: f.contains(Features::FOUR_BYTE_QUAD_OUT_READ),
        native_4ba_qio: f.contains(Features::FOUR_BYTE_QUAD_IO_READ),
        qpi_fast_read: false,
        qpi4b: false,
        in_qpi_mode: false,
    };
    let compatibility = addresses(ctx, master.features(), false);
    let mut rejected = [false; 256];
    while let Some(mut op) = protocol::select_read_op(
        master.features(),
        caps,
        dc,
        ctx.address_mode == AddressMode::FourByte,
        compatibility.iter().any(Option::is_some),
        |opcode| !rejected[opcode as usize] && master.probe_opcode(opcode),
    ) {
        let strategies = if op.native_4ba {
            [Some(AddressPlan::NativeFourByte), None, None]
        } else {
            compatibility
        };
        for address in strategies.into_iter().flatten() {
            op.address_width = address.width();
            let single_read = single_read_op(master, f, address);
            let plan = ReadPlan {
                op,
                address,
                single_read,
                qe: if op.io_mode.requires_quad() {
                    qe
                } else {
                    QuadEnableMethod::None
                },
            };
            if accepts(&plan) {
                return Ok(plan);
            }
        }
        rejected[op.opcode as usize] = true;
    }
    Err(Error::ChipNotSupported)
}

fn single_read_op<M: SpiMaster + ?Sized>(
    master: &M,
    features: Features,
    address: AddressPlan,
) -> Option<SpiReadOp> {
    if address == AddressPlan::NativeFourByte {
        if features.contains(Features::FOUR_BYTE_READ) && master.probe_opcode(opcodes::READ_4B) {
            Some(SpiReadOp::sio_read_4b())
        } else if features.contains(Features::FOUR_BYTE_FAST_READ | Features::FAST_READ)
            && master.probe_opcode(opcodes::FAST_READ_4B)
            && master.supports_read_dummy_cycles(IoMode::Single, 8)
        {
            Some(SpiReadOp {
                opcode: opcodes::FAST_READ_4B,
                dummy_cycles: 8,
                ..SpiReadOp::sio_read_4b()
            })
        } else {
            None
        }
    } else {
        master.probe_opcode(opcodes::READ).then_some(SpiReadOp {
            address_width: address.width(),
            ..SpiReadOp::sio_read()
        })
    }
}

/// Select program addressing and backend representation without hardware I/O.
pub fn select_write_plan<M: SpiMaster + ?Sized>(
    master: &M,
    ctx: &FlashContext,
    accepts: impl Fn(&WritePlan) -> bool,
) -> Result<WritePlan> {
    if ctx.page_size() == 0 || master.max_write_len() == 0 {
        return Err(Error::ChipNotSupported);
    }
    for address in addresses(
        ctx,
        master.features(),
        ctx.chip.features.supports_4ba_program(),
    )
    .into_iter()
    .flatten()
    {
        let opcode =
            if ctx.chip.features.contains(Features::AAI_WORD) && master.max_write_len() >= 2 {
                if address != AddressPlan::ThreeByte {
                    continue;
                }
                opcodes::AAI_WP
            } else if address == AddressPlan::NativeFourByte {
                opcodes::PP_4B
            } else {
                opcodes::PP
            };
        let plan = WritePlan {
            opcode,
            address,
            page_size: ctx.page_size(),
            granularity: ctx.chip.write_granularity,
        };
        if master.probe_opcode(opcode) && accepts(&plan) {
            return Ok(plan);
        }
    }
    Err(Error::ChipNotSupported)
}

/// Select a range-aligned erase command without hardware I/O.
pub fn select_erase_plan<M: SpiMaster + ?Sized>(
    master: &M,
    ctx: &FlashContext,
    addr: u32,
    len: u32,
) -> Result<ErasePlan> {
    let block = super::operations::select_erase_block(ctx.chip.erase_blocks(), addr, len)
        .ok_or(Error::InvalidAlignment)?;
    for address in addresses(ctx, master.features(), block.opcode_4b.is_some())
        .into_iter()
        .flatten()
    {
        let opcode = block.opcode_for_address_width(address == AddressPlan::NativeFourByte);
        if !master.probe_opcode(opcode) {
            continue;
        }
        let (poll_delay_us, timeout_us) = match block.max_block_size() {
            0..=4096 => (10_000, 1_000_000),
            4097..=65536 => (100_000, 4_000_000),
            _ => (500_000, 60_000_000),
        };
        return Ok(ErasePlan {
            opcode,
            address,
            block,
            poll_delay_us,
            timeout_us,
        });
    }
    Err(Error::ChipNotSupported)
}

/// The only adapter-retained I/O state. Dropping a future never clears this latch.
#[derive(Default)]
pub(crate) struct IoLifecycle {
    recovery_required: bool,
}
impl IoLifecycle {
    pub fn recovery_required(&self) -> bool {
        self.recovery_required
    }
    pub fn check(&self) -> Result<()> {
        if self.recovery_required {
            Err(Error::RecoveryRequired)
        } else {
            Ok(())
        }
    }

    /// One bracket for a whole transfer. Undo obligations are local to this future;
    /// each is recorded before submitting the corresponding hardware mutation.
    pub async fn run<M, T, E, F>(
        &mut self,
        master: &mut M,
        address: AddressPlan,
        qe: QuadEnableMethod,
        transfer: F,
    ) -> core::result::Result<T, E>
    where
        M: SpiMaster + ?Sized,
        E: From<Error> + core::fmt::Debug + PartialEq,
        F: for<'a> AsyncFnOnce(&'a mut M) -> core::result::Result<T, E>,
    {
        self.check().map_err(E::from)?;
        self.recovery_required = true;
        let mut restore_qe = false;
        let mut exit_4b = false;
        let mut ear = None;
        let result = async {
            match address {
                AddressPlan::Ear(f) => {
                    ear = Some((f, read_ear(master, f).await.map_err(E::from)?));
                }
                AddressPlan::EnterFourByte(f) => {
                    if f.contains(Features::FOUR_BYTE_ENTER_EAR7)
                        && !f.intersects(Features::FOUR_BYTE_ENTER | Features::FOUR_BYTE_ENTER_WREN)
                    {
                        ear = Some((f, read_ear(master, f).await.map_err(E::from)?));
                    } else {
                        exit_4b = true;
                    }
                    protocol::enter_4byte_mode_with_features(master, f)
                        .await
                        .map_err(E::from)?;
                }
                _ => {}
            }
            if qe != QuadEnableMethod::None
                && !protocol::is_quad_enabled(master, qe)
                    .await
                    .map_err(E::from)?
            {
                restore_qe = true;
                protocol::enable_quad_mode_volatile(master, qe)
                    .await
                    .map_err(E::from)?;
                if !protocol::is_quad_enabled(master, qe)
                    .await
                    .map_err(E::from)?
                {
                    return Err(E::from(Error::ProgrammerError));
                }
            }
            transfer(master).await
        }
        .await;
        // An uncertain firmware engine may issue more commands even while the
        // flash reports idle. Without a quiescence barrier, even RDSR is unsafe.
        if result.as_ref().err() == Some(&E::from(Error::RecoveryRequired)) {
            return result;
        }
        // A timed-out program/erase must be confirmed idle before register/mode
        // restoration. If it is still busy, no mutation is safe and recovery is required.
        let cleanup = async {
            protocol::wait_ready(master, 1_000, 60_000_000).await?;
            let mut failure = None;
            if master.probe_opcode(opcodes::WRDI)
                && let Err(e) = protocol::write_disable(master).await
            {
                failure = Some(e);
            }
            if restore_qe {
                let restored = async {
                    protocol::disable_quad_mode_volatile(master, qe).await?;
                    if protocol::is_quad_enabled(master, qe).await? {
                        return Err(Error::ProgrammerError);
                    }
                    Ok(())
                }
                .await;
                if let Err(e) = restored {
                    failure = Some(e);
                }
            }
            if exit_4b
                && let AddressPlan::EnterFourByte(f) = address
                && let Err(e) = protocol::exit_4byte_mode_with_features(master, f).await
            {
                failure = Some(e);
            }
            if let Some((f, saved)) = ear {
                let restored = async {
                    protocol::set_extended_address(master, f, saved).await?;
                    if read_ear(master, f).await? != saved {
                        return Err(Error::ProgrammerError);
                    }
                    Ok(())
                }
                .await;
                if let Err(e) = restored {
                    failure = Some(e);
                }
            }
            failure.map_or(Ok(()), Err)
        }
        .await;
        if let Err(error) = cleanup {
            log::error!(
                "flash cleanup failed: {error:?}; operation error: {:?}; hardware recovery required",
                result.as_ref().err()
            );
            return Err(E::from(Error::RecoveryRequired));
        }
        self.recovery_required = false;
        result
    }
}

async fn read_ear<M: SpiMaster + ?Sized>(master: &mut M, f: Features) -> Result<u8> {
    let (opcode, _) = protocol::extended_address_opcodes(f)?;
    let mut value = [0];
    master
        .execute(&mut SpiCommand::read_reg(opcode, &mut value))
        .await?;
    Ok(value[0])
}

/// Execute all read chunks under a caller-owned scope. Does not change QE/mode.
pub async fn read_spi<M: SpiMaster + ?Sized>(
    master: &mut M,
    plan: &ReadPlan,
    addr: u32,
    buf: &mut [u8],
) -> Result<()> {
    protocol::read_io_with_addressing(
        master,
        plan.op.opcode,
        addr,
        buf,
        plan.address.command_addressing(),
        plan.op.io_mode,
        plan.op.dummy_cycles,
    )
    .await
}
/// Execute all program chunks under a caller-owned scope.
pub async fn write_spi<M: SpiMaster + ?Sized>(
    master: &mut M,
    plan: &WritePlan,
    addr: u32,
    data: &[u8],
) -> Result<()> {
    if plan.opcode == opcodes::AAI_WP {
        return protocol::aai_word_program(master, addr, data).await;
    }
    let mut offset = 0;
    while offset < data.len() {
        let address = addr + offset as u32;
        let len = if plan.granularity == WriteGranularity::Byte {
            1
        } else {
            (data.len() - offset)
                .min(plan.page_size - address as usize % plan.page_size)
                .min(master.max_write_len())
                .min(0x0100_0000 - (address as usize & 0xffffff))
        };
        if len == 0 {
            return Err(Error::ProgrammerError);
        }
        protocol::program_page_with_addressing(
            master,
            plan.opcode,
            address,
            &data[offset..offset + len],
            plan.address.command_addressing(),
        )
        .await?;
        offset += len;
    }
    Ok(())
}
/// Execute all erase blocks under a caller-owned scope.
pub async fn erase_spi<M: SpiMaster + ?Sized>(
    master: &mut M,
    plan: &ErasePlan,
    addr: u32,
    len: u32,
) -> Result<()> {
    let mut offset = 0;
    while offset < len {
        protocol::erase_block(
            master,
            plan.opcode,
            addr + offset,
            plan.address.command_addressing(),
            plan.poll_delay_us,
            plan.timeout_us,
        )
        .await?;
        offset += plan
            .block
            .block_size_at_offset(offset)
            .unwrap_or(plan.block.max_block_size());
    }
    Ok(())
}
pub(crate) async fn verify_erased<M: SpiMaster + ?Sized>(
    lifecycle: &mut IoLifecycle,
    master: &mut M,
    plan: &ReadPlan,
    addr: u32,
    len: u32,
) -> Result<()> {
    lifecycle
        .run(
            master,
            plan.address,
            QuadEnableMethod::None,
            async |master| {
                let mut buf = [0; 4096];
                let mut offset = 0;
                while offset < len {
                    let count = buf.len().min((len - offset) as usize);
                    read_spi(master, plan, addr + offset, &mut buf[..count]).await?;
                    if let Some(index) = buf[..count].iter().position(|b| *b != 0xff) {
                        return Err(Error::EraseError(EraseFailure::VerifyFailed {
                            addr: addr + offset + index as u32,
                            found: buf[index],
                        }));
                    }
                    offset += count as u32;
                }
                Ok(())
            },
        )
        .await
}

//! Write protection operations
//!
//! This module provides functions to read, write, and manipulate
//! write protection settings on SPI flash chips.

use super::ranges::{decode_range, find_bits_for_range};
use super::types::{
    BitWritability, RangeDecoder, RegBitInfo, StatusRegister, WpBits, WpConfig, WpMode, WpRange,
    WpRegBitMap,
};
use crate::error::Error;
use crate::programmer::SpiMaster;
use crate::protocol;

/// Write protection result type with detailed error information
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WpError {
    /// Chip does not support write protection
    ChipUnsupported,
    /// Failed to read status registers
    ReadFailed,
    /// Failed to write status registers
    WriteFailed,
    /// Written value did not match (verify failed)
    VerifyFailed,
    /// Requested range is not supported by chip
    RangeUnsupported,
    /// Requested mode is not supported by chip
    ModeUnsupported,
    /// Cannot enumerate available ranges
    RangeListUnavailable,
    /// Write Protect Selection (WPS) bit is set, indicating per-sector mode
    UnsupportedState,
    /// A volatile (temporary) status register write was requested but the
    /// chip has no volatile write-enable (EWSR) according to its definition
    VolatileUnsupported,
    /// No documented persistent status-register write procedure.
    PersistentUnsupported,
    /// Readable status registers may hold volatile overrides, so the persistent
    /// state a persistent write would build on is unknown.
    PersistentStateUnknown,
    /// SPI communication error
    SpiError(Error),
}

impl From<Error> for WpError {
    fn from(e: Error) -> Self {
        WpError::SpiError(e)
    }
}

impl core::fmt::Display for WpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WpError::ChipUnsupported => write!(f, "chip does not support write protection"),
            WpError::ReadFailed => write!(f, "failed to read status registers"),
            WpError::WriteFailed => write!(f, "failed to write status registers"),
            WpError::VerifyFailed => write!(f, "verify failed: written value did not match"),
            WpError::RangeUnsupported => write!(f, "requested range is not supported"),
            WpError::ModeUnsupported => write!(f, "requested mode is not supported"),
            WpError::RangeListUnavailable => write!(f, "cannot enumerate available ranges"),
            WpError::UnsupportedState => {
                write!(f, "WPS bit set, per-sector protection mode not supported")
            }
            WpError::VolatileUnsupported => {
                write!(f, "chip does not support volatile status register writes")
            }
            WpError::PersistentUnsupported => {
                write!(
                    f,
                    "chip has no documented persistent status register write procedure"
                )
            }
            WpError::PersistentStateUnknown => {
                write!(
                    f,
                    "persistent write refused: status registers may hold volatile overrides \
                     of unknown persistent values; use a temporary write"
                )
            }
            WpError::SpiError(e) => write!(f, "SPI error: {}", e),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for WpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WpError::SpiError(err) => Some(err),
            _ => None,
        }
    }
}

/// Result type for write protection operations
pub type WpResult<T> = core::result::Result<T, WpError>;

/// Per-operation cache of status register values so reading multiple fields
/// from the same physical register only issues one SPI transfer per register.
#[derive(Default)]
struct RegSnapshot {
    sr1: Option<u8>,
    sr2: Option<u8>,
    sr3: Option<u8>,
}

impl RegSnapshot {
    async fn read_reg<M: SpiMaster + ?Sized>(
        &mut self,
        master: &mut M,
        reg: StatusRegister,
    ) -> WpResult<u8> {
        let slot = match reg {
            StatusRegister::Status1 => &mut self.sr1,
            StatusRegister::Status2 | StatusRegister::Config => &mut self.sr2,
            StatusRegister::Status3 => &mut self.sr3,
        };
        if let Some(val) = *slot {
            return Ok(val);
        }
        let val = match reg {
            StatusRegister::Status1 => protocol::read_status1(master).await?,
            StatusRegister::Status2 | StatusRegister::Config => {
                protocol::read_status2(master).await?
            }
            StatusRegister::Status3 => protocol::read_status3(master).await?,
        };
        *slot = Some(val);
        Ok(val)
    }

    async fn read_bit<M: SpiMaster + ?Sized>(
        &mut self,
        master: &mut M,
        bit_info: &RegBitInfo,
    ) -> WpResult<Option<u8>> {
        let reg = match bit_info.reg {
            Some(r) => r,
            None => return Ok(None),
        };
        if matches!(bit_info.writability, BitWritability::NotPresent) {
            return Ok(None);
        }
        let sr_val = self.read_reg(master, reg).await?;
        Ok(Some((sr_val >> bit_info.bit_index) & 1))
    }
}

async fn read_wp_bits_with_snapshot<M: SpiMaster + ?Sized>(
    master: &mut M,
    bit_map: &WpRegBitMap,
    regs: &mut RegSnapshot,
) -> WpResult<WpBits> {
    let mut bits = WpBits::empty();

    // Read SRP
    bits.srp = regs.read_bit(master, &bit_map.srp).await?;

    // Read SRL
    bits.srl = regs.read_bit(master, &bit_map.srl).await?;

    // Read CMP
    bits.cmp = regs.read_bit(master, &bit_map.cmp).await?;

    // Read SEC
    bits.sec = regs.read_bit(master, &bit_map.sec).await?;

    // Read TB
    bits.tb = regs.read_bit(master, &bit_map.tb).await?;

    // Read BP bits
    bits.bp_count = bit_map.bp_count();
    for i in 0..bits.bp_count {
        if let Some(val) = regs.read_bit(master, &bit_map.bp[i]).await? {
            bits.bp[i] = val;
        }
    }

    // Check for WPS bit (per-sector protection mode)
    if let Some(1) = regs.read_bit(master, &bit_map.wps).await? {
        return Err(WpError::UnsupportedState);
    }

    if bit_map.sector_bp6_undefined && bits.sec == Some(1) && bits.bp_value() == 6 {
        return Err(WpError::UnsupportedState);
    }
    Ok(bits)
}

/// Read all write protection bits from the chip
pub async fn read_wp_bits<M: SpiMaster + ?Sized>(
    master: &mut M,
    bit_map: &WpRegBitMap,
) -> WpResult<WpBits> {
    let mut regs = RegSnapshot::default();
    read_wp_bits_with_snapshot(master, bit_map, &mut regs).await
}

/// Read the current write protection configuration
pub async fn read_wp_config<M: SpiMaster + ?Sized>(
    master: &mut M,
    bit_map: &WpRegBitMap,
    total_size: u32,
    decoder: RangeDecoder,
) -> WpResult<WpConfig> {
    let bits = read_wp_bits(master, bit_map).await?;
    let mode = bits.mode();
    let range = decode_range(&bits, total_size, decoder);

    Ok(WpConfig::new(mode, range))
}

/// Build status register values from WpBits
fn build_register_values(bits: &WpBits, bit_map: &WpRegBitMap) -> (u8, u8, u8) {
    let mut sr1: u8 = 0;
    let mut sr2: u8 = 0;
    let mut sr3: u8 = 0;

    // Helper to set a bit in the appropriate register
    let set_bit = |sr1: &mut u8, sr2: &mut u8, sr3: &mut u8, info: &RegBitInfo, val: u8| {
        if !info.is_present() || !info.is_writable() {
            return;
        }
        let bit = (val & 1) << info.bit_index;
        match info.reg {
            Some(StatusRegister::Status1) => *sr1 |= bit,
            Some(StatusRegister::Status2) | Some(StatusRegister::Config) => *sr2 |= bit,
            Some(StatusRegister::Status3) => *sr3 |= bit,
            None => {}
        }
    };

    // Set SRP
    if let Some(val) = bits.srp {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.srp, val);
    }

    // Set SRL
    if let Some(val) = bits.srl {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.srl, val);
    }

    // Set CMP
    if let Some(val) = bits.cmp {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.cmp, val);
    }

    // Set SEC
    if let Some(val) = bits.sec {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.sec, val);
    }

    // Set TB
    if let Some(val) = bits.tb {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.tb, val);
    }

    // Set BP bits
    for i in 0..bits.bp_count {
        set_bit(&mut sr1, &mut sr2, &mut sr3, &bit_map.bp[i], bits.bp[i]);
    }

    (sr1, sr2, sr3)
}

/// Read current register values preserving non-WP bits
async fn read_current_registers<M: SpiMaster + ?Sized>(
    master: &mut M,
    read_sr2: bool,
) -> WpResult<(u8, u8)> {
    let sr1 = protocol::read_status1(master).await?;
    let sr2 = if read_sr2 {
        protocol::read_status2(master).await?
    } else {
        0
    };
    Ok((sr1, sr2))
}

/// Create a mask for bits that should be modified
fn build_register_masks(bit_map: &WpRegBitMap, bits: &WpBits) -> (u8, u8, u8) {
    let mut mask1: u8 = 0;
    let mut mask2: u8 = 0;
    let mut mask3: u8 = 0;

    let add_mask = |m1: &mut u8, m2: &mut u8, m3: &mut u8, info: &RegBitInfo| {
        if !info.is_present() || !info.is_writable() {
            return;
        }
        let bit = 1u8 << info.bit_index;
        match info.reg {
            Some(StatusRegister::Status1) => *m1 |= bit,
            Some(StatusRegister::Status2) | Some(StatusRegister::Config) => *m2 |= bit,
            Some(StatusRegister::Status3) => *m3 |= bit,
            None => {}
        }
    };

    if bits.srp.is_some() {
        add_mask(&mut mask1, &mut mask2, &mut mask3, &bit_map.srp);
    }
    if bits.srl.is_some() {
        add_mask(&mut mask1, &mut mask2, &mut mask3, &bit_map.srl);
    }
    if bits.cmp.is_some() {
        add_mask(&mut mask1, &mut mask2, &mut mask3, &bit_map.cmp);
    }
    if bits.sec.is_some() {
        add_mask(&mut mask1, &mut mask2, &mut mask3, &bit_map.sec);
    }
    if bits.tb.is_some() {
        add_mask(&mut mask1, &mut mask2, &mut mask3, &bit_map.tb);
    }
    for info in &bit_map.bp[..bits.bp_count] {
        add_mask(&mut mask1, &mut mask2, &mut mask3, info);
    }

    (mask1, mask2, mask3)
}

/// The caller's persistence requirement, independent of wire commands.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteOptions {
    /// Temporary protection, lost at the next power cycle.
    pub volatile: bool,
}

/// Documented status-register enable mechanisms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterWriteEnable {
    /// Write enable (0x06).
    Wren,
    /// Enable status-register write (0x50).
    Ewsr,
}

/// A chip's persistent and volatile write procedures are separate capabilities.
#[derive(Debug, Clone, Copy)]
pub struct StatusWriteMethods {
    /// Whether the chip documents the combined SR1/SR2 WRSR format.
    pub extended: bool,
    /// Documented procedure that survives power loss.
    pub persistent: Option<RegisterWriteEnable>,
    /// Documented temporary procedure, lost at power-off.
    pub volatile: Option<RegisterWriteEnable>,
}

impl StatusWriteMethods {
    /// Convert explicit database/SFDP capabilities without inferring persistence.
    pub fn from_features(features: crate::chip::Features) -> Self {
        use crate::chip::Features;
        Self {
            extended: features.contains(Features::WRSR_EXT),
            persistent: if features.contains(Features::WRSR_WREN) {
                Some(RegisterWriteEnable::Wren)
            } else if features.contains(Features::WRSR_PERSISTENT_EWSR) {
                Some(RegisterWriteEnable::Ewsr)
            } else {
                None
            },
            volatile: if features.contains(Features::WRSR_VOLATILE_WREN) {
                Some(RegisterWriteEnable::Wren)
            } else if features.contains(Features::WRSR_EWSR) {
                Some(RegisterWriteEnable::Ewsr)
            } else {
                None
            },
        }
    }

    /// Resolve the requested persistence or fail before any register write.
    pub fn resolve(self, options: WriteOptions) -> WpResult<ResolvedWriteOptions> {
        let enable = if options.volatile {
            self.volatile.ok_or(WpError::VolatileUnsupported)?
        } else {
            self.persistent.ok_or(WpError::PersistentUnsupported)?
        };
        Ok(ResolvedWriteOptions {
            enable,
            extended: self.extended,
        })
    }
}

/// A checked procedure. Construct with [`StatusWriteMethods::resolve`].
#[derive(Debug, Clone, Copy)]
pub struct ResolvedWriteOptions {
    enable: RegisterWriteEnable,
    extended: bool,
}

impl WriteOptions {
    /// Resolve the request using independently documented enable mechanisms.
    pub fn for_features(self, features: crate::chip::Features) -> WpResult<ResolvedWriteOptions> {
        StatusWriteMethods::from_features(features).resolve(self)
    }
}

async fn write_wp_bits_with_snapshot<M: SpiMaster + ?Sized>(
    master: &mut M,
    bits: &WpBits,
    bit_map: &WpRegBitMap,
    options: ResolvedWriteOptions,
    regs: &mut RegSnapshot,
) -> WpResult<()> {
    // Check the protection mode before any mutation, including direct bit writes.
    if let Some(1) = regs.read_bit(master, &bit_map.wps).await? {
        return Err(WpError::UnsupportedState);
    }

    let (new_sr1, new_sr2, _new_sr3) = build_register_values(bits, bit_map);
    let (mask1, mask2, mask3) = build_register_masks(bit_map, bits);
    if mask3 != 0 {
        return Err(WpError::ChipUnsupported);
    }
    // The documented wire format applies even when SR2 has no WP fields.
    // A one-byte WRSR may clear SR2, so preserve it in either case.
    let has_sr2 = [
        &bit_map.srp,
        &bit_map.srl,
        &bit_map.cmp,
        &bit_map.sec,
        &bit_map.tb,
    ]
    .into_iter()
    .chain(bit_map.bp.iter())
    .any(|info| {
        info.is_present()
            && matches!(
                info.reg,
                Some(StatusRegister::Status2 | StatusRegister::Config)
            )
    });
    if has_sr2 && !options.extended {
        return Err(WpError::ChipUnsupported);
    }
    let curr_sr1 = regs.read_reg(master, StatusRegister::Status1).await?;
    let curr_sr2 = if options.extended {
        regs.read_reg(master, StatusRegister::Status2).await?
    } else {
        0
    };

    // Merge new values with current values (preserving non-WP bits)
    let final_sr1 = (curr_sr1 & !mask1) | (new_sr1 & mask1);
    let final_sr2 = (curr_sr2 & !mask2) | (new_sr2 & mask2);
    if bit_map.sector_bp6_undefined && final_sr1 & 0x5c == 0x58 {
        return Err(WpError::UnsupportedState);
    }

    // Determine which registers need to be written
    let need_sr1 = mask1 != 0;
    let need_sr2 = options.extended && (mask1 != 0 || mask2 != 0);

    if !need_sr1 && !need_sr2 {
        // Nothing to write
        return Ok(());
    }

    // Only a resolved, documented procedure reaches the wire.
    if need_sr2 {
        if options.enable == RegisterWriteEnable::Ewsr {
            protocol::write_status12_ewsr(master, final_sr1, final_sr2).await?;
        } else {
            protocol::write_status12(master, final_sr1, final_sr2).await?;
        }
    } else if options.enable == RegisterWriteEnable::Ewsr {
        protocol::write_status1_ewsr(master, final_sr1).await?;
    } else {
        protocol::write_status1(master, final_sr1).await?;
    }

    // Verify the write with a fresh read (not the pre-write snapshot)
    let (verify_sr1, verify_sr2) = read_current_registers(master, need_sr2).await?;
    if (verify_sr1 & mask1) != (final_sr1 & mask1) {
        return Err(WpError::VerifyFailed);
    }
    if need_sr2 && (verify_sr2 & mask2) != (final_sr2 & mask2) {
        return Err(WpError::VerifyFailed);
    }

    Ok(())
}

/// Write WP bits to the chip
pub async fn write_wp_bits<M: SpiMaster + ?Sized>(
    master: &mut M,
    bits: &WpBits,
    bit_map: &WpRegBitMap,
    options: ResolvedWriteOptions,
) -> WpResult<()> {
    let mut regs = RegSnapshot::default();
    write_wp_bits_with_snapshot(master, bits, bit_map, options, &mut regs).await
}

fn resolve_mode_bits(mode: WpMode, bit_map: &WpRegBitMap) -> WpResult<(Option<u8>, Option<u8>)> {
    // Only Disabled and Hardware modes can be set programmatically
    let (srp, srl) = match mode {
        WpMode::Disabled => (0, 0),
        WpMode::Hardware => (1, 0),
        WpMode::PowerCycle | WpMode::Permanent => {
            return Err(WpError::ModeUnsupported);
        }
    };

    let srp_bit = if bit_map.srp.is_writable() {
        Some(srp)
    } else if srp != 0 {
        return Err(WpError::ModeUnsupported);
    } else {
        None
    };

    let srl_bit = if bit_map.srl.is_writable() {
        Some(srl)
    } else if srl != 0 {
        return Err(WpError::ModeUnsupported);
    } else {
        None
    };

    Ok((srp_bit, srl_bit))
}

fn resolve_range_bits(
    range: &WpRange,
    bit_map: &WpRegBitMap,
    total_size: u32,
    decoder: RangeDecoder,
) -> WpResult<WpBits> {
    let mut template = WpBits::empty();
    template.bp_count = bit_map.bp_count();
    if bit_map.tb.is_present() {
        template.tb = Some(0);
    }
    if bit_map.sec.is_present() {
        template.sec = Some(0);
    }
    if bit_map.cmp.is_present() {
        template.cmp = Some(0);
    }

    find_bits_for_range(range, total_size, &template, decoder).ok_or(WpError::RangeUnsupported)
}

/// Set the write protection mode
pub async fn set_wp_mode<M: SpiMaster + ?Sized>(
    master: &mut M,
    mode: WpMode,
    bit_map: &WpRegBitMap,
    options: ResolvedWriteOptions,
) -> WpResult<()> {
    let (srp, srl) = resolve_mode_bits(mode, bit_map)?;
    let mut bits = WpBits::empty();
    bits.srp = srp;
    bits.srl = srl;

    write_wp_bits(master, &bits, bit_map, options).await
}

/// Set the protected range
pub async fn set_wp_range<M: SpiMaster + ?Sized>(
    master: &mut M,
    range: &WpRange,
    bit_map: &WpRegBitMap,
    total_size: u32,
    decoder: RangeDecoder,
    options: ResolvedWriteOptions,
) -> WpResult<()> {
    let mut write_bits = resolve_range_bits(range, bit_map, total_size, decoder)?;

    // Read current bits to check WPS and preserve SRP/SRL
    let mut regs = RegSnapshot::default();
    let current_bits = read_wp_bits_with_snapshot(master, bit_map, &mut regs).await?;
    write_bits.srp = current_bits.srp;
    write_bits.srl = current_bits.srl;

    write_wp_bits_with_snapshot(master, &write_bits, bit_map, options, &mut regs).await
}

/// Write complete write protection configuration in a single status-register write.
pub async fn write_wp_config<M: SpiMaster + ?Sized>(
    master: &mut M,
    config: &WpConfig,
    bit_map: &WpRegBitMap,
    total_size: u32,
    decoder: RangeDecoder,
    options: ResolvedWriteOptions,
) -> WpResult<()> {
    // Validate both range and mode before issuing any status-register write.
    let mut write_bits = resolve_range_bits(&config.range, bit_map, total_size, decoder)?;
    let (srp, srl) = resolve_mode_bits(config.mode, bit_map)?;
    write_bits.srp = srp;
    write_bits.srl = srl;

    write_wp_bits(master, &write_bits, bit_map, options).await
}

/// Disable all write protection
pub async fn disable_wp<M: SpiMaster + ?Sized>(
    master: &mut M,
    bit_map: &WpRegBitMap,
    options: ResolvedWriteOptions,
) -> WpResult<()> {
    let mut bits = WpBits::empty();

    // Set all BP bits to 0
    bits.bp_count = bit_map.bp_count();
    for i in 0..bits.bp_count {
        bits.bp[i] = 0;
    }

    // Set SRP/SRL to disabled
    if bit_map.srp.is_writable() {
        bits.srp = Some(0);
    }
    if bit_map.srl.is_writable() {
        bits.srl = Some(0);
    }

    // Clear other protection bits
    if bit_map.cmp.is_writable() {
        bits.cmp = Some(0);
    }
    if bit_map.tb.is_writable() {
        bits.tb = Some(0);
    }
    if bit_map.sec.is_writable() {
        bits.sec = Some(0);
    }

    write_wp_bits(master, &bits, bit_map, options).await
}

#[cfg(feature = "alloc")]
/// Get all available protection ranges for a chip
pub fn get_available_ranges(
    bit_map: &WpRegBitMap,
    total_size: u32,
    decoder: RangeDecoder,
) -> alloc::vec::Vec<WpRange> {
    // Create a template with all available bits
    let mut template = WpBits::empty();
    template.bp_count = bit_map.bp_count();
    if bit_map.tb.is_present() {
        template.tb = Some(0);
    }
    if bit_map.sec.is_present() {
        template.sec = Some(0);
    }
    if bit_map.cmp.is_present() {
        template.cmp = Some(0);
    }

    super::ranges::get_all_ranges(&template, total_size, decoder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "std")]
    #[test]
    fn register_read_errors_abort_before_write() {
        use crate::chip::Features;
        use crate::programmer::SpiFeatures;
        use crate::spi::SpiCommand;

        struct Master {
            fail: u8,
            commands: std::vec::Vec<u8>,
        }
        impl SpiMaster for Master {
            fn features(&self) -> SpiFeatures {
                SpiFeatures::empty()
            }
            fn max_read_len(&self) -> usize {
                256
            }
            fn max_write_len(&self) -> usize {
                256
            }
            async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> crate::error::Result<()> {
                self.commands.push(cmd.opcode);
                if cmd.opcode == self.fail {
                    return Err(Error::SpiTransferFailed);
                }
                cmd.read_buf.fill(0);
                Ok(())
            }
            async fn delay_us(&mut self, _: u32) {}
        }
        let options = WriteOptions::default()
            .for_features(Features::WRSR_WREN | Features::WRSR_EXT)
            .unwrap();
        let mut map = WpRegBitMap::winbond_standard();
        let mut bits = WpBits::empty();
        bits.set_bp_value(1, 3);
        for fail in [0x35, 0x15] {
            map.wps = if fail == 0x15 {
                RegBitInfo::new(StatusRegister::Status3, 2, BitWritability::ReadWrite)
            } else {
                RegBitInfo::not_present()
            };
            let mut master = Master {
                fail,
                commands: std::vec::Vec::new(),
            };
            assert!(
                futures_lite::future::block_on(write_wp_bits(&mut master, &bits, &map, options))
                    .is_err()
            );
            assert!(!master.commands.contains(&0x01));
            assert!(!master.commands.contains(&0x06));
        }
    }

    #[test]
    fn write_enable_command_follows_chip_features_and_volatility() {
        use crate::chip::Features;
        let resolve = |features, volatile| {
            WriteOptions { volatile }
                .for_features(features)
                .map(|o| o.enable == RegisterWriteEnable::Ewsr)
        };
        let wren = Features::WRSR_WREN;
        let ewsr = Features::WRSR_EWSR;

        // Ordinary chip: non-volatile via WREN, no volatile write available.
        assert_eq!(resolve(wren, false), Ok(false));
        assert_eq!(resolve(wren, true), Err(WpError::VolatileUnsupported));
        // Chips that can do both must not silently turn a normal (persistent)
        // write into a volatile one just because they also know EWSR.
        assert_eq!(resolve(wren | ewsr, false), Ok(false));
        assert_eq!(resolve(wren | ewsr, true), Ok(true));
        // EWSR alone is not evidence that protection survives a power cycle.
        assert_eq!(resolve(ewsr, false), Err(WpError::PersistentUnsupported));
        assert_eq!(resolve(ewsr, true), Ok(true));
        assert_eq!(resolve(Features::WRSR_PERSISTENT_EWSR, false), Ok(true));
        assert_eq!(
            resolve(Features::WRSR_PERSISTENT_EWSR, true),
            Err(WpError::VolatileUnsupported)
        );
        assert_eq!(resolve(Features::WRSR_VOLATILE_WREN, true), Ok(false));
        // Undocumented chip: never guess a persistent-write mechanism.
        assert_eq!(
            resolve(Features::empty(), false),
            Err(WpError::PersistentUnsupported)
        );
        assert_eq!(
            resolve(Features::empty(), true),
            Err(WpError::VolatileUnsupported)
        );
    }

    #[test]
    fn test_build_register_values() {
        let bit_map = WpRegBitMap::winbond_standard();
        let mut bits = WpBits::empty();
        bits.set_bp_value(0b111, 3);
        bits.tb = Some(1);
        bits.sec = Some(0);
        bits.srp = Some(1);

        let (sr1, sr2, _sr3) = build_register_values(&bits, &bit_map);

        // BP0=bit2, BP1=bit3, BP2=bit4, TB=bit5, SEC=bit6, SRP=bit7
        assert_eq!(sr1 & 0b00011100, 0b00011100); // BP0-BP2 set
        assert_eq!(sr1 & 0b00100000, 0b00100000); // TB set
        assert_eq!(sr1 & 0b01000000, 0); // SEC clear
        assert_eq!(sr1 & 0b10000000, 0b10000000); // SRP set
        assert_eq!(sr2, 0); // No SR2 bits set
    }

    #[cfg(feature = "std")]
    #[test]
    fn read_wp_bits_caches_status_registers_once_per_register() {
        use crate::programmer::SpiFeatures;
        use crate::spi::SpiCommand;

        struct RegMaster {
            sr1: u8,
            sr2: u8,
            sr3: u8,
            commands: std::vec::Vec<u8>,
        }
        impl SpiMaster for RegMaster {
            fn features(&self) -> SpiFeatures {
                SpiFeatures::empty()
            }
            fn max_read_len(&self) -> usize {
                256
            }
            fn max_write_len(&self) -> usize {
                256
            }
            async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> crate::error::Result<()> {
                self.commands.push(cmd.opcode);
                match cmd.opcode {
                    0x05 => cmd.read_buf[0] = self.sr1,
                    0x35 => cmd.read_buf[0] = self.sr2,
                    0x15 => cmd.read_buf[0] = self.sr3,
                    _ => return Err(Error::OpcodeNotSupported),
                }
                Ok(())
            }
            async fn delay_us(&mut self, _: u32) {}
        }

        let mut map = WpRegBitMap::winbond_standard();
        map.wps = RegBitInfo::new(StatusRegister::Status3, 2, BitWritability::ReadWrite);
        let mut master = RegMaster {
            sr1: 0b1011_0100, // SRP=1, SEC=0, TB=1, BP=0b101
            sr2: 0b0100_0000, // CMP=1, SRL=0
            sr3: 0,
            commands: std::vec::Vec::new(),
        };

        let bits = futures_lite::future::block_on(read_wp_bits(&mut master, &map)).unwrap();
        assert_eq!(bits.srp, Some(1));
        assert_eq!(bits.srl, Some(0));
        assert_eq!(bits.cmp, Some(1));
        assert_eq!(bits.sec, Some(0));
        assert_eq!(bits.tb, Some(1));
        assert_eq!(bits.bp_value(), 0b101);
        // Exactly one SPI read per physical status register.
        assert_eq!(master.commands, std::vec![0x05, 0x35, 0x15]);
    }

    #[cfg(feature = "std")]
    #[test]
    fn write_wp_config_uses_single_write_and_validates_mode_before_mutating() {
        use crate::chip::Features;
        use crate::programmer::SpiFeatures;
        use crate::spi::SpiCommand;

        struct FlashRegs {
            sr1: u8,
            sr2: u8,
            commands: std::vec::Vec<u8>,
        }
        impl SpiMaster for FlashRegs {
            fn features(&self) -> SpiFeatures {
                SpiFeatures::empty()
            }
            fn max_read_len(&self) -> usize {
                256
            }
            fn max_write_len(&self) -> usize {
                256
            }
            async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> crate::error::Result<()> {
                self.commands.push(cmd.opcode);
                match cmd.opcode {
                    0x06 => {} // WREN
                    0x05 => cmd.read_buf[0] = self.sr1,
                    0x35 => cmd.read_buf[0] = self.sr2,
                    0x01 => {
                        self.sr1 = cmd.write_data[0];
                        if cmd.write_data.len() >= 2 {
                            self.sr2 = cmd.write_data[1];
                        }
                    }
                    _ => return Err(Error::OpcodeNotSupported),
                }
                Ok(())
            }
            async fn delay_us(&mut self, _: u32) {}
        }

        let map = WpRegBitMap::winbond_standard();
        let total_size = 16 * 1024 * 1024;
        let options = WriteOptions::default()
            .for_features(Features::WRSR_WREN | Features::WRSR_EXT)
            .unwrap();

        // Unsupported mode must fail before issuing any WREN/WRSR.
        let mut master = FlashRegs {
            sr1: 0,
            sr2: 0,
            commands: std::vec::Vec::new(),
        };
        let bad_cfg = WpConfig::new(WpMode::PowerCycle, WpRange::full(total_size));
        assert_eq!(
            futures_lite::future::block_on(write_wp_config(
                &mut master,
                &bad_cfg,
                &map,
                total_size,
                RangeDecoder::Spi25,
                options,
            )),
            Err(WpError::ModeUnsupported)
        );
        assert!(master.commands.is_empty());
        assert_eq!((master.sr1, master.sr2), (0, 0));

        // Valid range + mode must perform exactly one WRSR (0x01) write.
        let good_cfg = WpConfig::new(WpMode::Hardware, WpRange::full(total_size));
        futures_lite::future::block_on(write_wp_config(
            &mut master,
            &good_cfg,
            &map,
            total_size,
            RangeDecoder::Spi25,
            options,
        ))
        .unwrap();
        let wrsr_count = master.commands.iter().filter(|&&op| op == 0x01).count();
        assert_eq!(wrsr_count, 1);

        let readback = futures_lite::future::block_on(read_wp_config(
            &mut master,
            &map,
            total_size,
            RangeDecoder::Spi25,
        ))
        .unwrap();
        assert_eq!(readback.mode, good_cfg.mode);
        assert_eq!(readback.range, good_cfg.range);
    }
}

use super::*;
use crate::host::{HostAccess, MmioAccess, PciAddress, PciConfigAccess};
use futures_lite::future::block_on;
use rflasher_core::flash::{FlashContext, FlashDevice, SpiFlashDevice};
use std::sync::{Arc, Mutex};

const SPIBAR: u64 = 0xfec1_0000;
const ROM_ACCESS: u8 = 0xc0; // SpiAccessMacRomEn | SpiHostAccessRomEn
const SPEED_RESET: u8 = 0x70; // FastSpeed=01, NormSpeed=11 (16.5 MHz)
const PROTECT_LOW_64K: u32 = 0xffc0_0000 | (63 << 2) | 1;

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedCmd {
    opcode: u8,
    tx: Vec<u8>,
    rx: usize,
}

struct FakeChip {
    flash: Vec<u8>,
    wel: bool,
}

impl FakeChip {
    fn new() -> Self {
        Self {
            flash: vec![0xff; 0x2_0000], // 128 KiB backing store
            wel: false,
        }
    }

    fn run(&mut self, opcode: u8, tx: &[u8], rx: usize) -> Vec<u8> {
        let addr = |b: &[u8]| ((b[0] as usize) << 16) | ((b[1] as usize) << 8) | (b[2] as usize);
        match opcode {
            opcodes::RDID => [0xef, 0x40, 0x16].into_iter().cycle().take(rx).collect(),
            opcodes::RDSR => vec![if self.wel { opcodes::SR1_WEL } else { 0 }; rx],
            opcodes::WREN => {
                self.wel = true;
                vec![0; rx]
            }
            opcodes::READ => {
                let start = addr(tx);
                (0..rx)
                    .map(|i| self.flash.get(start + i).copied().unwrap_or(0xff))
                    .collect()
            }
            opcodes::FAST_READ => {
                let start = addr(tx);
                (0..rx)
                    .map(|i| self.flash.get(start + i).copied().unwrap_or(0xff))
                    .collect()
            }
            opcodes::PP => {
                if self.wel {
                    let start = addr(tx);
                    let page = start & !0xff;
                    for (i, &byte) in tx[3..].iter().enumerate() {
                        let off = page + ((start + i) & 0xff);
                        if let Some(slot) = self.flash.get_mut(off) {
                            *slot &= byte;
                        }
                    }
                    self.wel = false;
                }
                vec![0; rx]
            }
            opcodes::SE_20 => {
                if self.wel {
                    let start = addr(tx) & !0xfff;
                    if start + 0x1000 <= self.flash.len() {
                        self.flash[start..start + 0x1000].fill(0xff);
                    }
                    self.wel = false;
                }
                vec![0; rx]
            }
            _ => vec![0xaa; rx],
        }
    }
}

struct Model {
    regs: [u8; SPI_WINDOW],
    fifo: [u8; SB600_FIFO_SIZE],
    ptr: usize,
    busy_reads: u32,
    exec_pending: u32,
    spi_busy_reads: u32,
    intruder: bool,
    count_intruder: bool,
    speed_stuck: bool,
    chip: FakeChip,
    commands: Vec<RecordedCmd>,
    writes: Vec<(usize, u8)>,
    maps: Vec<(u64, usize)>,
}

impl Default for Model {
    fn default() -> Self {
        let mut regs = [0; SPI_WINDOW];
        regs[2] = ROM_ACCESS | 0x08;
        regs[0x0d] = SPEED_RESET;
        Self {
            regs,
            fifo: [0; SB600_FIFO_SIZE],
            ptr: 0,
            busy_reads: 0,
            exec_pending: 0,
            spi_busy_reads: 0,
            intruder: false,
            count_intruder: false,
            speed_stuck: false,
            chip: FakeChip::new(),
            commands: Vec::new(),
            writes: Vec::new(),
            maps: Vec::new(),
        }
    }
}

impl Model {
    fn execute(&mut self) {
        let opcode = self.regs[0];
        let counts = self.regs[1];
        let tx_len = usize::from(counts & 0x0f);
        // Hardware quirk: when tx_len == 0, RxByteCount must be rx_len + 1.
        let rx_len = usize::from(counts >> 4).saturating_sub(usize::from(tx_len == 0));
        let tx: Vec<u8> = (0..tx_len)
            .map(|_| {
                let b = self.fifo[self.ptr];
                self.ptr = (self.ptr + 1) % SB600_FIFO_SIZE;
                b
            })
            .collect();
        for b in self.chip.run(opcode, &tx, rx_len) {
            self.fifo[self.ptr] = b;
            self.ptr = (self.ptr + 1) % SB600_FIFO_SIZE;
        }
        self.commands.push(RecordedCmd {
            opcode,
            tx,
            rx: rx_len,
        });
        if self.intruder {
            self.ptr = (self.ptr + 1) % SB600_FIFO_SIZE;
        }
        if self.count_intruder {
            self.regs[1] ^= 1;
        }
        self.exec_pending = self.busy_reads;
    }

    fn read_byte(&mut self, offset: usize) -> u8 {
        match offset {
            2 => {
                let busy = self.exec_pending > 0;
                if busy && self.exec_pending != u32::MAX {
                    self.exec_pending -= 1;
                }
                self.regs[2] | u8::from(busy)
            }
            3 => {
                let busy = self.spi_busy_reads > 0;
                if busy && self.spi_busy_reads != u32::MAX {
                    self.spi_busy_reads -= 1;
                }
                self.regs[3] | (u8::from(busy) << 7)
            }
            0x0c => {
                let b = self.fifo[self.ptr];
                self.ptr = (self.ptr + 1) % SB600_FIFO_SIZE;
                b
            }
            0x0d => (self.regs[0x0d] & !SPEED_FIFO_PTR_MASK) | self.ptr as u8,
            _ => self.regs[offset],
        }
    }

    fn write_byte(&mut self, offset: usize, value: u8) {
        self.writes.push((offset, value));
        match offset {
            2 => {
                let old = self.regs[2];
                // Preserve clear-once ROM access bits; update writable control bits.
                self.regs[2] = (old & !(CMD_FAST_READ_ENABLE | 0x08 | ROM_ACCESS))
                    | (value & (CMD_FAST_READ_ENABLE | 0x08))
                    | (old & value & ROM_ACCESS);
                if value & CMD_FIFO_PTR_CLR != 0 {
                    self.ptr = 0;
                }
                if value & CMD_FIFO_PTR_INC != 0 {
                    self.ptr = (self.ptr + 1) % SB600_FIFO_SIZE;
                }
                if value & CMD_EXECUTE_OPCODE != 0 {
                    self.execute();
                }
            }
            0x0c => {
                self.fifo[self.ptr] = value;
                self.ptr = (self.ptr + 1) % SB600_FIFO_SIZE;
            }
            0x0d if !self.speed_stuck => self.regs[0x0d] = value & !SPEED_FIFO_PTR_MASK,
            0x0d => {
                self.regs[0x0d] =
                    (self.regs[0x0d] & SPEED_NORM_MASK) | (value & !(SPEED_NORM_MASK | 0x07))
            }
            _ => self.regs[offset] = value,
        }
    }
}

#[derive(Clone, Default)]
struct FakeHost(Arc<Mutex<Model>>);

impl MmioAccess for FakeHost {
    fn read8(&self, offset: usize) -> u8 {
        self.0.lock().unwrap().read_byte(offset)
    }
    fn read16(&self, offset: usize) -> u16 {
        let mut m = self.0.lock().unwrap();
        u16::from_le_bytes([m.read_byte(offset), m.read_byte(offset + 1)])
    }
    fn read32(&self, offset: usize) -> u32 {
        let mut m = self.0.lock().unwrap();
        u32::from_le_bytes(core::array::from_fn(|i| m.read_byte(offset + i)))
    }
    fn write8(&self, offset: usize, value: u8) {
        self.0.lock().unwrap().write_byte(offset, value);
    }
    fn write16(&self, offset: usize, value: u16) {
        let mut m = self.0.lock().unwrap();
        for (i, b) in value.to_le_bytes().into_iter().enumerate() {
            m.write_byte(offset + i, b);
        }
    }
    fn write32(&self, offset: usize, value: u32) {
        let mut m = self.0.lock().unwrap();
        for (i, b) in value.to_le_bytes().into_iter().enumerate() {
            m.write_byte(offset + i, b);
        }
    }
}

impl PciConfigAccess for FakeHost {
    type Error = InternalError;
    fn read8(&self, _: PciAddress, _: u16) -> Result<u8, InternalError> {
        unreachable!("controller must not access PCI config space")
    }
    fn read16(&self, _: PciAddress, _: u16) -> Result<u16, InternalError> {
        unreachable!("controller must not access PCI config space")
    }
    fn read32(&self, _: PciAddress, _: u16) -> Result<u32, InternalError> {
        unreachable!("controller must not access PCI config space")
    }
    fn write8(&self, _: PciAddress, _: u16, _: u8) -> Result<(), InternalError> {
        unreachable!("controller must not access PCI config space")
    }
    fn write16(&self, _: PciAddress, _: u16, _: u16) -> Result<(), InternalError> {
        unreachable!("controller must not access PCI config space")
    }
    fn write32(&self, _: PciAddress, _: u16, _: u32) -> Result<(), InternalError> {
        unreachable!("controller must not access PCI config space")
    }
}

impl HostAccess for FakeHost {
    type MmioRegion = FakeHost;

    unsafe fn map_mmio(&self, phys_addr: u64, size: usize) -> Result<Self, InternalError> {
        self.0.lock().unwrap().maps.push((phys_addr, size));
        Ok(self.clone())
    }

    fn delay_us(&self, _us: u32) {}
}

fn sb950_config() -> Sb600SpiConfig {
    Sb600SpiConfig {
        imc: Sb600Imc::StrappedOff,
        ..Sb600SpiConfig::new(Sb600Generation::Sb89xx)
    }
}

fn chip_4mib() -> FlashContext {
    let db = rflasher_chips::ChipDatabase::new();
    FlashContext::new(db.find_by_name("W25Q32.V")[0].clone())
}

fn open(
    host: &FakeHost,
    config: Sb600SpiConfig,
) -> Result<Sb600SpiController<FakeHost>, InternalError> {
    // SAFETY: FakeHost performs no real physical MMIO.
    unsafe { Sb600SpiController::new_with_host(host.clone(), SPIBAR, config) }
}

#[test]
fn opcode_only_readoffby1_and_ring_buffer_wrap() {
    let host = FakeHost::default();
    let mut ctrl = open(&host, sb950_config()).unwrap();

    // Opcode-only RDID: must program RxByteCount = 3 + 1 = 4 (readoffby1).
    let mut id = [0u8; 3];
    ctrl.send_command(&[opcodes::RDID], &mut id).unwrap();
    assert_eq!(id, [0xef, 0x40, 0x16]);
    assert_eq!(host.0.lock().unwrap().regs[SPI_CNTRL0_COUNTS], 4 << 4);

    // Opcode-only WREN with 0 RX bytes: RxByteCount = 1, TX = 0.
    ctrl.send_command(&[opcodes::WREN], &mut []).unwrap();
    assert_eq!(host.0.lock().unwrap().regs[SPI_CNTRL0_COUNTS], 1 << 4);

    // 3 TX address bytes + 8 RX bytes = 11 bytes wrapping around the 8-byte FIFO.
    let pattern = [0x10, 0x21, 0x32, 0x43, 0x54, 0x65, 0x76, 0x87];
    host.0.lock().unwrap().chip.flash[0x1234..0x123c].copy_from_slice(&pattern);
    let mut readback = [0u8; 8];
    ctrl.send_command(&[opcodes::READ, 0x00, 0x12, 0x34], &mut readback)
        .unwrap();
    assert_eq!(readback, pattern);
    assert_eq!(host.0.lock().unwrap().regs[SPI_CNTRL0_COUNTS], (8 << 4) | 3);
}

#[test]
fn fifo_and_address_limits() {
    let host = FakeHost::default();
    let mut ctrl = open(&host, sb950_config()).unwrap();

    assert!(ctrl.send_command(&[], &mut []).is_err());
    assert!(
        ctrl.send_command(&[opcodes::READ, 0, 0, 0], &mut [0; 9])
            .is_err()
    );
    assert!(
        ctrl.send_command(&[opcodes::PP, 0, 0, 0, 1, 2, 3, 4, 5, 6], &mut [])
            .is_err()
    );

    // 3-byte address + 5 data bytes = 8 TX bytes succeeds; 6 data bytes fails.
    let mut ok_pp = SpiCommand::write_3b(opcodes::PP, 0x100, &[0xaa; 5]);
    Controller::execute(&mut ctrl, &mut ok_pp).unwrap();
    let mut too_big = SpiCommand::write_3b(opcodes::PP, 0x100, &[0xaa; 6]);
    assert_eq!(
        Controller::execute(&mut ctrl, &mut too_big),
        Err(CoreError::OpcodeNotSupported)
    );

    // 4-byte addressing, 24-bit overflow/wraparound, and non-byte dummy cycles are refused.
    let mut buf = [0u8; 4];
    let mut cmd4b = SpiCommand::read_4b(opcodes::READ_4B, 0, &mut buf);
    assert_eq!(
        Controller::execute(&mut ctrl, &mut cmd4b),
        Err(CoreError::OpcodeNotSupported)
    );
    let mut overflow = SpiCommand::read_3b(opcodes::READ, 0x0100_0000, &mut buf);
    assert_eq!(
        Controller::execute(&mut ctrl, &mut overflow),
        Err(CoreError::AddressOutOfBounds)
    );
    let mut wrap = SpiCommand::read_3b(opcodes::READ, 0x00ff_ffff, &mut buf[..2]);
    assert_eq!(
        Controller::execute(&mut ctrl, &mut wrap),
        Err(CoreError::AddressOutOfBounds)
    );
    let mut last_byte = SpiCommand::read_3b(opcodes::READ, 0x00ff_ffff, &mut buf[..1]);
    Controller::execute(&mut ctrl, &mut last_byte).unwrap();
    let mut bad_dummy = SpiCommand::read_3b(opcodes::FAST_READ, 0, &mut buf).with_dummy_cycles(4);
    assert_eq!(
        Controller::execute(&mut ctrl, &mut bad_dummy),
        Err(CoreError::OpcodeNotSupported)
    );

    // Byte-aligned dummy cycle goes through the FIFO as 0xFF.
    let mut fast = SpiCommand::read_3b(opcodes::FAST_READ, 0x10, &mut buf).with_dummy_cycles(8);
    Controller::execute(&mut ctrl, &mut fast).unwrap();
    let last = host.0.lock().unwrap().commands.last().unwrap().clone();
    assert_eq!(last.opcode, opcodes::FAST_READ);
    assert_eq!(last.tx, vec![0x00, 0x00, 0x10, 0xff]);
}

#[test]
fn timeout_poisons_until_idle_and_skips_drop_restore() {
    let host = FakeHost::default();
    {
        let mut m = host.0.lock().unwrap();
        m.regs[2] |= CMD_FAST_READ_ENABLE;
        m.regs[0x0d] = 0x50; // 33 MHz
    }
    let mut ctrl = open(&host, sb950_config()).unwrap();

    // Short busy wait succeeds.
    host.0.lock().unwrap().busy_reads = 3;
    ctrl.send_command(&[opcodes::RDSR], &mut [0]).unwrap();
    assert!(!ctrl.is_poisoned());

    // Timeout poisons handle; subsequent command writes nothing while busy.
    host.0.lock().unwrap().busy_reads = u32::MAX;
    assert!(matches!(
        ctrl.send_command(&[opcodes::RDSR], &mut [0]),
        Err(InternalError::Io("SPI transfer timeout"))
    ));
    assert!(ctrl.is_poisoned());

    host.0.lock().unwrap().writes.clear();
    assert!(matches!(
        ctrl.send_command(&[opcodes::RDID], &mut [0; 3]),
        Err(InternalError::Io("SPI command engine busy"))
    ));
    assert!(host.0.lock().unwrap().writes.is_empty());

    // Dropping while still busy must not write clock/fast-read registers.
    drop(ctrl);
    assert!(host.0.lock().unwrap().writes.is_empty());

    // If a timed-out command completes late before drop, Drop restores clock & fastReadEnable.
    let host_late = FakeHost::default();
    {
        let mut m = host_late.0.lock().unwrap();
        m.regs[2] |= CMD_FAST_READ_ENABLE;
        m.regs[0x0d] = 0x50; // 33 MHz
    }
    let mut ctrl_late = open(&host_late, sb950_config()).unwrap();
    host_late.0.lock().unwrap().busy_reads = u32::MAX;
    assert!(ctrl_late.send_command(&[opcodes::RDSR], &mut [0]).is_err());
    {
        let mut m = host_late.0.lock().unwrap();
        m.exec_pending = 0;
        m.busy_reads = 0;
    }
    drop(ctrl_late);
    {
        let m = host_late.0.lock().unwrap();
        assert_eq!(m.regs[2] & CMD_FAST_READ_ENABLE, CMD_FAST_READ_ENABLE);
        assert_eq!(m.regs[0x0d], 0x50);
    }

    // Once idle again, a recovered controller resumes normally.
    let host2 = FakeHost::default();
    let mut ctrl2 = open(&host2, sb950_config()).unwrap();
    host2.0.lock().unwrap().busy_reads = u32::MAX;
    assert!(ctrl2.send_command(&[opcodes::RDSR], &mut [0]).is_err());
    {
        let mut m = host2.0.lock().unwrap();
        m.exec_pending = 0;
        m.busy_reads = 0;
    }
    let mut id = [0u8; 3];
    ctrl2.send_command(&[opcodes::RDID], &mut id).unwrap();
    assert_eq!(id, [0xef, 0x40, 0x16]);
    assert!(!ctrl2.is_poisoned());
}

#[test]
fn busy_engine_and_concurrent_fifo_corruption_are_detected() {
    // Busy during open -> no writes.
    let host = FakeHost::default();
    {
        let mut m = host.0.lock().unwrap();
        m.exec_pending = u32::MAX;
        m.regs[2] |= CMD_FAST_READ_ENABLE;
        m.regs[0x0d] = 0x50;
    }
    assert!(open(&host, sb950_config()).is_err());
    assert!(host.0.lock().unwrap().writes.is_empty());

    // SpiBusy on SB8xx blocks staging until clear.
    let host = FakeHost::default();
    let mut ctrl = open(&host, sb950_config()).unwrap();
    host.0.lock().unwrap().spi_busy_reads = u32::MAX;
    host.0.lock().unwrap().writes.clear();
    assert!(ctrl.send_command(&[opcodes::RDID], &mut [0; 3]).is_err());
    assert!(host.0.lock().unwrap().writes.is_empty());

    // Concurrent FIFO pointer corruption.
    let host = FakeHost::default();
    host.0.lock().unwrap().intruder = true;
    let mut ctrl = open(&host, sb950_config()).unwrap();
    assert!(matches!(
        ctrl.send_command(&[opcodes::RDID], &mut [0; 3]),
        Err(InternalError::Io("SPI FIFO pointer corruption"))
    ));

    // Concurrent counts register change.
    let host = FakeHost::default();
    host.0.lock().unwrap().count_intruder = true;
    let mut ctrl = open(&host, sb950_config()).unwrap();
    assert!(matches!(
        ctrl.send_command(&[opcodes::RDID], &mut [0; 3]),
        Err(InternalError::Io("SPI opcode or read/write count changed"))
    ));
}

#[test]
fn spi25_erase_write_read_roundtrip_and_4ba_rejection() {
    let host = FakeHost::default();
    // Seed the target sector with non-erased bytes so a no-op erase would be caught.
    host.0.lock().unwrap().chip.flash[0x1000..0x2000].fill(0x5a);
    let ctrl = open(&host, sb950_config()).unwrap();
    let mut dev = SpiFlashDevice::new(ctrl, chip_4mib());

    block_on(dev.erase(0x1000, 0x1000)).unwrap();
    assert!(
        host.0.lock().unwrap().chip.flash[0x1000..0x2000]
            .iter()
            .all(|&b| b == 0xff)
    );
    let data: Vec<u8> = (0..300).map(|i| (i * 7) as u8).collect();
    block_on(dev.write(0x10fe, &data)).unwrap();
    let mut readback = vec![0u8; data.len()];
    block_on(dev.read(0x10fe, &mut readback)).unwrap();
    assert_eq!(readback, data);

    let programs: Vec<_> = host
        .0
        .lock()
        .unwrap()
        .commands
        .iter()
        .filter(|c| c.opcode == opcodes::PP)
        .cloned()
        .collect();
    // Every PP chunk fits in 5 data bytes and respects the 256-byte page boundary.
    assert!(
        programs
            .iter()
            .all(|c| c.tx.len() <= 3 + SB600_MAX_DATA_WRITE)
    );
    assert_eq!(programs[0].tx, vec![0x00, 0x10, 0xfe, data[0], data[1]]);

    // Chips > 16 MiB must be refused cleanly without entering 4BA/EAR modes.
    let db = rflasher_chips::ChipDatabase::new();
    let large = FlashContext::new(db.find_by_name("W25Q256JW")[0].clone());
    let host_big = FakeHost::default();
    let mut dev_big = SpiFlashDevice::new(open(&host_big, sb950_config()).unwrap(), large);
    assert_eq!(
        block_on(dev_big.read(0, &mut [0u8; 4])),
        Err(CoreError::ChipNotSupported)
    );
    assert!(host_big.0.lock().unwrap().commands.is_empty());
}

#[test]
fn locked_controller_and_unverified_imc_refused_without_writes() {
    for cleared in [0x40u8, 0x80] {
        let host = FakeHost::default();
        {
            let mut m = host.0.lock().unwrap();
            m.regs[2] &= !cleared;
            m.regs[2] |= CMD_FAST_READ_ENABLE;
            m.regs[0x0d] = 0x50;
        }
        assert!(matches!(
            open(&host, sb950_config()),
            Err(InternalError::AccessDenied { .. })
        ));
        assert!(host.0.lock().unwrap().writes.is_empty());
    }

    for imc in [Sb600Imc::Enabled, Sb600Imc::Unknown, Sb600Imc::NotPresent] {
        let host = FakeHost::default();
        let cfg = Sb600SpiConfig {
            imc,
            ..sb950_config()
        };
        assert!(open(&host, cfg).is_err());
        assert!(host.0.lock().unwrap().maps.is_empty());
    }
    // SB600 has no IMC and opens with NotPresent.
    let host = FakeHost::default();
    open(&host, Sb600SpiConfig::new(Sb600Generation::Sb6xx)).unwrap();
}

#[test]
fn restricted_opcodes_are_refused_while_other_opcodes_pass() {
    let host = FakeHost::default();
    {
        let mut m = host.0.lock().unwrap();
        m.regs[SPI_RESTRICTED_CMD] = opcodes::CE_C7;
        m.regs[SPI_RESTRICTED_CMD2 + 1] = opcodes::WRSR;
        m.regs[SPI_CNTRL1 + 2] = opcodes::CE_60;
    }
    let mut ctrl = open(&host, sb950_config()).unwrap();
    assert_eq!(
        ctrl.restricted_opcodes().collect::<Vec<_>>(),
        vec![opcodes::CE_C7, opcodes::WRSR, opcodes::CE_60]
    );
    for op in [opcodes::CE_C7, opcodes::WRSR, opcodes::CE_60] {
        assert!(!Controller::probe_opcode(&ctrl, op));
        assert_eq!(
            Controller::execute(&mut ctrl, &mut SpiCommand::simple(op)),
            Err(CoreError::OpcodeNotSupported)
        );
        assert!(matches!(
            ctrl.send_command(&[op], &mut []),
            Err(InternalError::AccessDenied { .. })
        ));
    }
    assert!(host.0.lock().unwrap().commands.is_empty());

    // Unrestricted opcodes (including vendor-specific ones) are allowed.
    assert!(Controller::probe_opcode(&ctrl, 0x2b));
    let mut sr = [0u8; 1];
    ctrl.send_command(&[0x2b], &mut sr).unwrap();
    assert_eq!(sr, [0xaa]);
}

#[test]
fn rom_protect_translation_and_enforcement() {
    let protect = RomProtect::from_raw(PROTECT_LOW_64K);
    assert_eq!(protect.base(), 0xffc0_0000);
    assert_eq!(protect.size(), 0x10000);
    assert!(protect.protects(4 << 20, 0, 1));
    assert!(!protect.protects(4 << 20, 0x10000, 0x20000));
    // Aliasing on a smaller chip and wraparound across chip end.
    assert!(protect.protects(1 << 20, 0, 1));
    let wrap = RomProtect::from_raw(0xfff1_c000 | (31 << 2) | 1);
    assert!(wrap.protects(0x20000, 0x1ffff, 0x20000));
    assert!(wrap.protects(0x20000, 0x3fff, 0x4000));
    assert!(!wrap.protects(0x20000, 0x4000, 0x1c000));
    // Below top 16 MiB or non-power-of-two chip conservatively protects all.
    assert!(RomProtect::from_raw(0x000e_0000 | (127 << 2) | 1).protects(
        4 << 20,
        0x100000,
        0x100001
    ));
    assert!(protect.protects(3 << 20, 0x200000, 0x200001));

    // Write-protected range blocks erase/write via both SpiFlashDevice and raw execute without MMIO.
    let host = FakeHost::default();
    let mut cfg = sb950_config();
    cfg.rom_protect[0] = protect;
    let mut ctrl = open(&host, cfg).unwrap();
    // Before chip_identified, active Rom Protect conservatively denies.
    assert_eq!(
        Controller::check_access(&ctrl, FlashAccess::Modify, 0x10000, 1),
        Err(CoreError::RegionProtected)
    );
    Controller::chip_identified(&mut ctrl, &chip_4mib().chip);
    host.0.lock().unwrap().writes.clear();
    assert_eq!(
        Controller::execute(
            &mut ctrl,
            &mut SpiCommand::write_3b(opcodes::PP, 0x1000, &[0x55])
        ),
        Err(CoreError::RegionProtected)
    );
    assert_eq!(
        Controller::execute(
            &mut ctrl,
            &mut SpiCommand::write_3b(opcodes::SE_20, 0x1000, &[])
        ),
        Err(CoreError::RegionProtected)
    );
    assert_eq!(
        Controller::execute(&mut ctrl, &mut SpiCommand::simple(opcodes::CE_C7)),
        Err(CoreError::RegionProtected)
    );
    assert_eq!(
        Controller::execute(&mut ctrl, &mut SpiCommand::simple(0x99)),
        Err(CoreError::RegionProtected)
    );
    assert!(host.0.lock().unwrap().writes.is_empty());

    let mut dev = SpiFlashDevice::new(ctrl, chip_4mib());
    assert_eq!(
        block_on(dev.erase(0x1000, 0x1000)),
        Err(CoreError::RegionProtected)
    );
    assert_eq!(
        block_on(dev.write(0x1000, &[0x55])),
        Err(CoreError::RegionProtected)
    );
    assert!(host.0.lock().unwrap().commands.is_empty());
    block_on(dev.read(0x1000, &mut [0u8; 4])).unwrap();
    block_on(dev.erase(0x10000, 0x1000)).unwrap();

    // Read-protected range blocks reads via raw execute, controller_read, and SpiFlashDevice,
    // including chip-boundary wraparound and extra transmit bytes that would shift read offsets.
    let host = FakeHost::default();
    let mut cfg = sb950_config();
    cfg.rom_protect[0] = RomProtect::from_raw(PROTECT_LOW_64K | 0b10);
    let mut ctrl = open(&host, cfg).unwrap();
    Controller::chip_identified(&mut ctrl, &chip_4mib().chip);
    host.0.lock().unwrap().writes.clear();
    assert_eq!(
        Controller::execute(
            &mut ctrl,
            &mut SpiCommand::read_3b(opcodes::READ, 0x1000, &mut [0; 4])
        ),
        Err(CoreError::RegionProtected)
    );
    // Crossing the 4 MiB physical end would roll over into protected offset 0.
    assert_eq!(
        Controller::execute(
            &mut ctrl,
            &mut SpiCommand::read_3b(opcodes::READ, 0x3f_ffff, &mut [0; 2])
        ),
        Err(CoreError::RegionProtected)
    );
    // Extra transmit bytes on READ or FAST_READ shift the captured bytes past the checked address.
    assert!(matches!(
        ctrl.send_command(&[opcodes::READ, 0x01, 0x00, 0x00, 0xff], &mut [0; 4]),
        Err(InternalError::AccessDenied { .. })
    ));
    assert!(matches!(
        ctrl.send_command(
            &[opcodes::FAST_READ, 0x01, 0x00, 0x00, 0xff, 0xff],
            &mut [0; 4]
        ),
        Err(InternalError::AccessDenied { .. })
    ));
    assert!(host.0.lock().unwrap().writes.is_empty());
    // Standard 4-byte FAST_READ and last legal byte at 0x3f_ffff (len 1) are allowed.
    let mut fast_buf = [0; 4];
    let mut fast_ok =
        SpiCommand::read_3b(opcodes::FAST_READ, 0x10000, &mut fast_buf).with_dummy_cycles(8);
    Controller::execute(&mut ctrl, &mut fast_ok).unwrap();
    Controller::execute(
        &mut ctrl,
        &mut SpiCommand::read_3b(opcodes::READ, 0x3f_ffff, &mut [0; 1]),
    )
    .unwrap();
    assert_eq!(
        Controller::controller_read(&mut ctrl, 0x1000, &mut [0u8; 4], 4 << 20),
        Err(CoreError::RegionProtected)
    );
    let mut dev = SpiFlashDevice::new(ctrl, chip_4mib());
    assert_eq!(
        block_on(dev.read(0x1000, &mut [0u8; 4])),
        Err(CoreError::RegionProtected)
    );
    block_on(dev.read(0x10000, &mut [0u8; 4])).unwrap();
}

#[test]
fn clock_and_fast_read_configured_and_restored_on_drop() {
    let host = FakeHost::default();
    {
        let mut m = host.0.lock().unwrap();
        m.regs[2] |= CMD_FAST_READ_ENABLE;
        m.regs[0x0d] = 0x90; // FastSpeed=10, NormSpeed=01 (33 MHz)
    }
    let ctrl = open(&host, sb950_config()).unwrap();
    {
        let m = host.0.lock().unwrap();
        assert_eq!(m.regs[2] & CMD_FAST_READ_ENABLE, 0);
        assert_eq!(m.regs[0x0d], 0xb0); // FastSpeed preserved, NormSpeed=11 (16.5 MHz)
    }
    drop(ctrl);
    {
        let m = host.0.lock().unwrap();
        assert_eq!(m.regs[2] & CMD_FAST_READ_ENABLE, CMD_FAST_READ_ENABLE);
        assert_eq!(m.regs[0x0d], 0x90);
        assert_eq!(m.regs[2] & ROM_ACCESS, ROM_ACCESS);
        // No trigger bits written during init/restore.
        assert!(
            m.writes
                .iter()
                .all(|&(off, v)| off != 2 || v & CMD_TRIGGERS == 0)
        );
    }

    // SB7xx does not touch fastReadEnable (bit 18 reserved).
    let host = FakeHost::default();
    host.0.lock().unwrap().regs[2] |= CMD_FAST_READ_ENABLE;
    let ctrl = open(
        &host,
        Sb600SpiConfig {
            speed: Some(Sb600SpiSpeed::Mhz33),
            imc: Sb600Imc::StrappedOff,
            ..Sb600SpiConfig::new(Sb600Generation::Sb7xx)
        },
    )
    .unwrap();
    assert_eq!(
        host.0.lock().unwrap().regs[2] & CMD_FAST_READ_ENABLE,
        CMD_FAST_READ_ENABLE
    );
    assert_eq!(host.0.lock().unwrap().regs[0x0d], 0x50);
    drop(ctrl);
    assert_eq!(host.0.lock().unwrap().regs[0x0d], SPEED_RESET);

    // If setting NormSpeed does not stick, fastReadEnable is restored on failure.
    let host = FakeHost::default();
    {
        let mut m = host.0.lock().unwrap();
        m.regs[2] |= CMD_FAST_READ_ENABLE;
        m.speed_stuck = true;
    }
    let cfg = Sb600SpiConfig {
        speed: Some(Sb600SpiSpeed::Mhz33),
        ..sb950_config()
    };
    assert!(matches!(
        open(&host, cfg),
        Err(InternalError::SpiInit("Setting SPI clock failed"))
    ));
    assert_eq!(
        host.0.lock().unwrap().regs[2] & CMD_FAST_READ_ENABLE,
        CMD_FAST_READ_ENABLE
    );
}

#[test]
fn generation_and_speed_tables() {
    use Sb600Generation::*;
    for (lpc, rev, expected) in [
        (0x438d, 0x00, Some(Sb6xx)),
        (0x439d, 0x39, Some(Sb7xx)),
        (0x439d, 0x3b, Some(Sb7xx)), // SB7xx A13 (matches flashprog 0x39..=0x3d)
        (0x439d, 0x3d, Some(Sb7xx)),
        (0x439d, 0x3e, None),
        (0x439d, 0x40, Some(Sb89xx)),
        (0x439d, 0x42, Some(Sb89xx)),
        (0x439d, 0x43, None),
        (0x780e, 0x14, Some(Hudson234)),
        (0x780e, 0x16, Some(Bolton)),
        (0x780e, 0x3a, Some(Yangtze)),
        (0x790e, 0x4a, Some(Yangtze)),
        (0x790e, 0x4b, Some(Promontory)),
    ] {
        assert_eq!(Sb600Generation::from_ids(lpc, rev), expected);
    }

    assert_eq!(
        Sb600SpiSpeed::parse("16.5 MHz"),
        Some(Sb600SpiSpeed::Mhz16_5)
    );
    assert_eq!(Sb600SpiSpeed::parse("33mhz"), Some(Sb600SpiSpeed::Mhz33));
    assert_eq!(Sb600SpiSpeed::parse("22 MHZ"), Some(Sb600SpiSpeed::Mhz22));
    assert_eq!(Sb600SpiSpeed::parse("66mhz"), None);
    assert_eq!(Sb600SpiSpeed::parse("100mhz"), None);
}

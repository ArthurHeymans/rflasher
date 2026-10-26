//! Stateful protection emulator driving the real SPI, fast-path and erased APIs.
#![cfg(feature = "std")]

use futures_lite::future::block_on;
use rflasher_core::chip::{
    ChipTestStatus, EraseBlock, Features, FlashChip, Unlock, WriteGranularity,
};
use rflasher_core::error::{EraseFailure, Error, Result};
use rflasher_core::flash::unified::{self, NoProgress};
use rflasher_core::flash::{self, FlashContext, FlashDevice, SpiFlashDevice};
use rflasher_core::programmer::{SpiFeatures, SpiMaster};
use rflasher_core::spi::SpiCommand;
use rflasher_programmers::ErasedFlashDevice;
use std::sync::{Arc, Mutex};

struct State {
    unlock: Unlock,
    sr1: u8,
    sr2: u8,
    sr3: u8,
    wp_high: bool,
    wel: bool,
    ewsr: bool,
    sectors: Vec<bool>,
    bpr: Vec<u8>,
    data: Vec<u8>,
    commands: Vec<(u8, Vec<u8>)>,
    fail_mutation: bool,
    ignore_unlock: bool,
    ignore_erase: bool,
    deny_opcode: Option<u8>,
    fail_restore: bool,
    error_flag: u8,
    fast_read: bool,
    fast_write: bool,
    bulk_writes: usize,
    bulk_reads: usize,
    bulk_aai: bool,
    aai_next: Option<usize>,
    fail_aai_at: Option<usize>,
    erase_busy_us: u32,
    busy_us: u32,
    fail_busy_read: bool,
}

impl State {
    fn status(&self) -> u8 {
        let protection = if self.unlock == Unlock::At2x {
            let swp = if self.sectors.iter().all(|p| *p) {
                0x0c
            } else if self.sectors.iter().any(|p| *p) {
                0x04
            } else {
                0
            };
            (self.sr1 & 0xa0) | swp | if self.wp_high { 0x10 } else { 0 }
        } else {
            self.sr1
        };
        protection | if self.wel { 2 } else { 0 } | if self.busy_us > 0 { 1 } else { 0 }
    }
    fn protected(&self, addr: usize, len: usize) -> bool {
        if self.unlock == Unlock::At2x {
            self.sectors[addr / 4096..(addr + len).div_ceil(4096)]
                .iter()
                .any(|p| *p)
        } else if !self.bpr.is_empty() {
            self.bpr.iter().any(|b| *b != 0)
        } else {
            self.sr1 & self.unlock.status_masks().map_or(0x1c, |m| m.0) != 0
        }
    }
    fn program(&mut self, addr: usize, bytes: &[u8]) -> Result<()> {
        if self.fail_mutation {
            return Err(Error::SpiTransferFailed);
        }
        if !self.protected(addr, bytes.len()) {
            for (dest, src) in self.data[addr..addr + bytes.len()].iter_mut().zip(bytes) {
                *dest &= *src;
            }
        }
        self.sr1 |= self.error_flag;
        Ok(())
    }
}

#[derive(Clone)]
struct Master(Arc<Mutex<State>>);
impl SpiMaster for Master {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::empty()
    }
    fn max_read_len(&self) -> usize {
        4096
    }
    fn max_write_len(&self) -> usize {
        256
    }
    fn probe_opcode(&self, opcode: u8) -> bool {
        self.0.lock().unwrap().deny_opcode != Some(opcode)
    }
    async fn delay_us(&mut self, us: u32) {
        let mut s = self.0.lock().unwrap();
        s.busy_us = s.busy_us.saturating_sub(us);
    }
    async fn execute(&mut self, cmd: &mut SpiCommand<'_>) -> Result<()> {
        let mut s = self.0.lock().unwrap();
        s.commands.push((cmd.opcode, cmd.write_data.to_vec()));
        let addr = cmd.address.unwrap_or(0) as usize;
        // AAI mode accepts only AAI, RDSR and WRDI. Busy erases accept RDSR only.
        if (s.aai_next.is_some() && !matches!(cmd.opcode, 0xad | 0x05 | 0x04))
            || (s.busy_us > 0 && cmd.opcode != 0x05)
        {
            return Err(Error::SpiTransferFailed);
        }
        match cmd.opcode {
            0x06 => s.wel = true,
            0x50 => s.ewsr = true,
            0x04 => {
                s.wel = false;
                s.aai_next = None;
            }
            0x05 => {
                if s.busy_us > 0 && s.fail_busy_read {
                    s.fail_busy_read = false;
                    return Err(Error::SpiTransferFailed);
                }
                cmd.read_buf.fill(s.status());
            }
            0x35 => cmd.read_buf.fill(s.sr2),
            0x15 => {
                if s.deny_opcode == Some(0x15) {
                    return Err(Error::OpcodeNotSupported);
                }
                cmd.read_buf.fill(s.sr3);
            }
            0x01 => {
                if !s.wel && !s.ewsr {
                    return Ok(());
                }
                s.wel = false;
                s.ewsr = false;
                let value = cmd.write_data[0];
                if s.fail_restore && value & 0x3c != 0 && value != 0x04 {
                    return Err(Error::SpiTransferFailed);
                }
                if s.ignore_unlock || (s.sr1 & 0x80 != 0 && !s.wp_high) {
                    return Ok(());
                }
                if s.unlock == Unlock::At2x {
                    if s.sr1 & 0x80 == 0 {
                        match value & 0x3c {
                            0 => s.sectors.fill(false),
                            0x3c => s.sectors.fill(true),
                            _ => {}
                        }
                    }
                    s.sr1 = (s.sr1 & !0x80) | (value & 0x80);
                } else {
                    s.sr1 = value;
                }
                if cmd.write_data.len() == 2 {
                    s.sr2 = cmd.write_data[1];
                }
            }
            0x3c => cmd
                .read_buf
                .fill(if s.sectors[addr / 4096] { 0xff } else { 0 }),
            0x36 | 0x39 => {
                if s.wel && s.sr1 & 0x80 == 0 {
                    s.sectors[addr / 4096] = cmd.opcode == 0x36;
                }
                s.wel = false;
            }
            0x72 => cmd.read_buf.copy_from_slice(&s.bpr),
            0x98 => {
                if s.wel && !s.ignore_unlock {
                    s.bpr.fill(0);
                }
                s.wel = false;
            }
            0x42 => {
                if s.fail_restore {
                    return Err(Error::SpiTransferFailed);
                }
                if s.wel {
                    s.bpr.copy_from_slice(cmd.write_data);
                }
                s.wel = false;
            }
            0x03 => cmd
                .read_buf
                .copy_from_slice(&s.data[addr..addr + cmd.read_buf.len()]),
            0x02 => {
                if s.wel {
                    s.program(addr, cmd.write_data)?;
                }
                s.wel = false;
            }
            0xad => {
                let addr = if let Some(next) = s.aai_next {
                    assert!(cmd.address.is_none());
                    next
                } else {
                    assert!(s.wel);
                    assert!(cmd.address.is_some());
                    addr
                };
                s.program(addr, cmd.write_data)?;
                s.aai_next = Some(addr + 2);
                // Simulate a command accepted by the chip but lost on transport.
                if s.fail_aai_at == Some(addr) {
                    return Err(Error::SpiTransferFailed);
                }
            }
            0x20 | 0x52 | 0xc4 | 0xc7 => {
                if s.fail_mutation {
                    return Err(Error::SpiTransferFailed);
                }
                let len = match cmd.opcode {
                    0x20 => 4096,
                    0x52 => 8192,
                    0xc4 => s.data.len() / 2,
                    _ => s.data.len(),
                };
                if s.wel && !s.ignore_erase && !s.protected(addr, len) {
                    s.data[addr..addr + len].fill(0xff);
                }
                s.wel = false;
                s.sr1 |= s.error_flag;
                s.busy_us = s.erase_busy_us;
            }
            _ => return Err(Error::OpcodeNotSupported),
        }
        Ok(())
    }
    async fn read_flash(&mut self, ctx: &FlashContext, addr: u32, buf: &mut [u8]) -> Result<()> {
        if !self.0.lock().unwrap().fast_read {
            return flash::spi_read(self, ctx, addr, buf).await;
        }
        let mut s = self.0.lock().unwrap();
        s.bulk_reads += 1;
        buf.copy_from_slice(&s.data[addr as usize..addr as usize + buf.len()]);
        Ok(())
    }
    async fn program_flash(&mut self, ctx: &FlashContext, addr: u32, data: &[u8]) -> Result<()> {
        if !self.0.lock().unwrap().fast_write {
            return flash::spi_program(self, ctx, addr, data).await;
        }
        let aai = {
            let mut s = self.0.lock().unwrap();
            s.bulk_writes += 1;
            s.bulk_aai
        };
        if aai {
            rflasher_core::protocol::aai_word_program(self, addr, data).await
        } else {
            self.0.lock().unwrap().program(addr as usize, data)
        }
    }
}

/// Enable the master's bulk read and program fast paths.
fn with_fast_paths(master: Master) -> Master {
    let mut s = master.0.lock().unwrap();
    s.fast_read = true;
    s.fast_write = true;
    drop(s);
    master
}

fn fixture(unlock: Unlock, sr1: u8, features: Features, size: usize) -> (Master, FlashContext) {
    let bpr_len = match unlock {
        Unlock::Sst26_6 => 6,
        Unlock::Sst26_10 => 10,
        Unlock::Sst26_18 => 18,
        _ => 0,
    };
    let mut bpr = vec![0xff; bpr_len];
    if bpr_len > 0 {
        bpr[..2].fill(0x55);
    }
    let master = Master(Arc::new(Mutex::new(State {
        unlock,
        sr1,
        sr2: 0x02,
        sr3: 0,
        wp_high: true,
        wel: false,
        ewsr: false,
        sectors: vec![true; size / 4096],
        bpr,
        data: vec![0; size],
        commands: vec![],
        fail_mutation: false,
        ignore_unlock: false,
        ignore_erase: false,
        deny_opcode: None,
        fail_restore: false,
        error_flag: 0,
        fast_read: false,
        fast_write: false,
        bulk_writes: 0,
        bulk_reads: 0,
        bulk_aai: false,
        aai_next: None,
        fail_aai_at: None,
        erase_busy_us: 0,
        busy_us: 0,
        fail_busy_read: false,
    })));
    let ctx = FlashContext::new(FlashChip {
        vendor: "Test".into(),
        name: "Protected".into(),
        jedec_manufacturer: 0x1f,
        jedec_device: 0x4700,
        total_size: size as u32,
        page_size: 256,
        features,
        unlock,
        voltage_min_mv: 2700,
        voltage_max_mv: 3600,
        write_granularity: WriteGranularity::Page,
        erase_blocks: vec![
            EraseBlock::with_count(0x20, 4096, size as u32 / 4096),
            EraseBlock::new(0xc7, size as u32),
        ],
        tested: ChipTestStatus::default(),
    });
    (master, ctx)
}

#[test]
fn protected_at25df321_bios_update_works_through_erased_spi_and_bulk_devices() {
    block_on(async {
        for (fast_read, fast_write) in [(false, false), (true, false), (true, true)] {
            let (master, ctx) = fixture(Unlock::At2x, 0x80, Features::WRSR_WREN, 4 * 1024 * 1024);
            let state = master.0.clone();
            {
                let mut s = state.lock().unwrap();
                s.fast_read = fast_read;
                s.fast_write = fast_write;
            }
            let mut device = ErasedFlashDevice::new(SpiFlashDevice::new(master, ctx));
            // Erasing a sector must preserve descriptor/GbE and data elsewhere.
            let target = vec![0x5f; 4096];
            let mut policy = rflasher_core::flash::MutationPolicy::default();
            unified::smart_write_region_with_policy(
                &mut device,
                0x310000,
                &target,
                &mut NoProgress,
                &mut policy,
            )
            .await
            .unwrap();
            let s = state.lock().unwrap();
            assert_eq!(&s.data[0x310000..0x311000], target);
            assert!(s.data[..0x310000].iter().all(|b| *b == 0));
            assert!(s.data[0x311000..].iter().all(|b| *b == 0));
            assert!(s.sectors.iter().all(|p| *p));
            assert_eq!(s.status() & 0x8c, 0x8c);
            assert!(
                s.commands
                    .iter()
                    .any(|(op, data)| *op == 0x01 && data == &[0])
            );
            assert_eq!(s.bulk_writes > 0, fast_write);
            assert_eq!(s.bulk_reads > 0, fast_read);
            assert!(!device.wp_supported());
        }
    });
}

#[test]
fn mixed_atmel_sector_protection_is_restored_on_success_and_failure() {
    block_on(async {
        for fail in [false, true] {
            let (master, ctx) = fixture(Unlock::At2x, 0x80, Features::WRSR_WREN, 8192);
            let state = master.0.clone();
            {
                let mut s = state.lock().unwrap();
                s.sectors[0] = false;
                s.fail_mutation = fail;
            }
            let mut device = SpiFlashDevice::new(master, ctx);
            assert_eq!(
                device.erase(4096, 4096).await,
                if fail {
                    Err(Error::SpiTransferFailed)
                } else {
                    Ok(())
                }
            );
            let s = state.lock().unwrap();
            assert_eq!(s.sectors, [false, true]);
            assert_eq!(s.sr1 & 0x80, 0x80);
        }
    });
}

#[test]
fn masks_preserve_unrelated_status_bits_and_restore_after_failed_program() {
    block_on(async {
        for (unlock, sr1) in [
            (Unlock::Status, 0x44),
            (Unlock::Bp1Srwd, 0x8c),
            (Unlock::Bp2Srwd, 0x9c),
            (Unlock::Bp3Srwd, 0xbc),
            (Unlock::Bp4Srwd, 0xfc),
            (Unlock::At25f, 0x8c),
            (Unlock::At25f512a, 0x84),
            (Unlock::At25f512b, 0x94),
            (Unlock::At25fs010, 0xec),
            (Unlock::At25fs040, 0xfc),
            (Unlock::N25q, 0xfc),
        ] {
            for fail in [false, true] {
                let (master, ctx) = fixture(unlock, sr1, Features::WRSR_WREN, 8192);
                let state = master.0.clone();
                {
                    let mut s = state.lock().unwrap();
                    s.data.fill(0xff);
                    s.fail_mutation = fail;
                }
                let mut device = SpiFlashDevice::new(master, ctx);
                assert_eq!(
                    device.write(0, &[0x55]).await,
                    if fail {
                        Err(Error::SpiTransferFailed)
                    } else {
                        Ok(())
                    }
                );
                let s = state.lock().unwrap();
                assert_eq!(s.sr1, sr1, "{unlock:?}");
                let (bp, lock, _, preserve) = unlock.status_masks().unwrap();
                assert!(
                    s.commands
                        .iter()
                        .any(|(op, bytes)| *op == 0x01
                            && bytes == &[(sr1 & !(bp | lock)) & preserve]),
                    "{unlock:?}"
                );
            }
        }
    });
}

#[test]
fn sst26_registers_are_unlocked_and_restored_at_each_documented_width() {
    block_on(async {
        for unlock in [Unlock::Sst26_6, Unlock::Sst26_10, Unlock::Sst26_18] {
            for fail in [false, true] {
                let (master, ctx) = fixture(unlock, 0, Features::WRSR_WREN, 8192);
                let state = master.0.clone();
                let original = {
                    let mut s = state.lock().unwrap();
                    s.fail_mutation = fail;
                    s.bpr.clone()
                };
                let mut device = SpiFlashDevice::new(with_fast_paths(master), ctx);
                assert_eq!(
                    device.erase(0, 4096).await,
                    if fail {
                        Err(Error::SpiTransferFailed)
                    } else {
                        Ok(())
                    }
                );
                let s = state.lock().unwrap();
                assert_eq!(s.bpr, original);
                assert!(s.commands.iter().any(|(op, _)| *op == 0x98));
                assert!(!s.commands.iter().any(|(op, _)| *op == 0x01));
            }
        }
    });
}

#[test]
fn unknown_and_unprotected_chips_do_not_receive_status_writes() {
    block_on(async {
        for unlock in [Unlock::Unknown, Unlock::None, Unlock::Bp2Srwd, Unlock::At2x] {
            let (master, ctx) = fixture(unlock, 0, Features::WRSR_WREN, 8192);
            let state = master.0.clone();
            state.lock().unwrap().sectors.fill(false);
            let mut device = SpiFlashDevice::new(master, ctx);
            device.erase(0, 4096).await.unwrap();
            assert!(
                !state
                    .lock()
                    .unwrap()
                    .commands
                    .iter()
                    .any(|(op, _)| *op == 0x01)
            );
        }
    });
}

#[test]
fn hardware_lock_and_ignored_unprotect_are_refused_before_erase() {
    block_on(async {
        for hard_lock in [false, true] {
            let (master, ctx) = fixture(Unlock::At2x, 0x80, Features::WRSR_WREN, 8192);
            let state = master.0.clone();
            {
                let mut s = state.lock().unwrap();
                s.wp_high = !hard_lock;
                s.ignore_unlock = !hard_lock;
            }
            let mut device = SpiFlashDevice::new(master, ctx);
            assert_eq!(device.erase(0, 4096).await, Err(Error::WriteProtected));
            let s = state.lock().unwrap();
            assert!(!s.commands.iter().any(|(op, _)| *op == 0x20));
            assert!(s.sectors.iter().all(|p| *p));
        }
    });
}

#[test]
fn invalid_requests_and_undocumented_status_enables_do_not_unlock() {
    block_on(async {
        let (master, ctx) = fixture(Unlock::At2x, 0x80, Features::empty(), 8192);
        let state = master.0.clone();
        let mut device = SpiFlashDevice::new(master, ctx);
        assert_eq!(device.erase(1, 4096).await, Err(Error::InvalidAlignment));
        assert_eq!(
            device.write(8191, &[0, 0]).await,
            Err(Error::AddressOutOfBounds)
        );
        assert!(state.lock().unwrap().commands.is_empty());
        assert_eq!(device.erase(0, 4096).await, Err(Error::ChipNotSupported));
        assert!(
            !state
                .lock()
                .unwrap()
                .commands
                .iter()
                .any(|(op, _)| *op == 0x01)
        );
    });
}

#[test]
fn persistent_ewsr_and_combined_status_writes_follow_chip_metadata() {
    block_on(async {
        for features in [
            Features::WRSR_PERSISTENT_EWSR,
            Features::WRSR_WREN | Features::WRSR_EXT,
        ] {
            let (master, ctx) = fixture(Unlock::Bp2Srwd, 0x9c, features, 8192);
            let state = master.0.clone();
            let mut device = SpiFlashDevice::new(master, ctx);
            device.erase(0, 4096).await.unwrap();
            let s = state.lock().unwrap();
            let enable = if features.contains(Features::WRSR_WREN) {
                0x06
            } else {
                0x50
            };
            for (i, (_, bytes)) in s
                .commands
                .iter()
                .enumerate()
                .filter(|(_, (op, _))| *op == 0x01)
            {
                assert_eq!(s.commands[i - 1].0, enable);
                assert_eq!(
                    bytes.len(),
                    if features.contains(Features::WRSR_EXT) {
                        2
                    } else {
                        1
                    }
                );
            }
            assert_eq!(s.sr2, 0x02);
        }
    });
}

#[test]
fn either_chips_use_volatile_ewsr_for_temporary_unlock_and_restore() {
    block_on(async {
        // EITHER chip: persistent writes go through WREN, but the temporary
        // unlock borrowed for one mutation must use volatile EWSR (0x50),
        // never WREN (0x06), before each status write — for single- and
        // two-byte status writes, and on mutation failure as well as success.
        for (extended, fail) in [(false, false), (true, false), (false, true)] {
            let mut features = Features::WRSR_WREN | Features::WRSR_EWSR;
            if extended {
                features |= Features::WRSR_EXT;
            }
            let (master, ctx) = fixture(Unlock::Bp2Srwd, 0x9c, features, 8192);
            let state = master.0.clone();
            state.lock().unwrap().fail_mutation = fail;
            let mut device = SpiFlashDevice::new(master, ctx);
            assert_eq!(
                device.erase(0, 4096).await,
                if fail {
                    Err(Error::SpiTransferFailed)
                } else {
                    Ok(())
                }
            );
            let s = state.lock().unwrap();
            assert_eq!(s.sr1, 0x9c);
            let status_writes: Vec<_> = s
                .commands
                .iter()
                .enumerate()
                .filter(|(_, (op, _))| *op == 0x01)
                .collect();
            assert!(!status_writes.is_empty());
            for (i, (_, bytes)) in status_writes {
                assert_eq!(s.commands[i - 1].0, 0x50);
                assert_eq!(bytes.len(), if extended { 2 } else { 1 });
            }
        }
    });
}

#[test]
fn erase_and_bulk_program_error_flags_are_reported_and_protection_restored() {
    block_on(async {
        for (unlock, sr1, flag) in [(Unlock::At2x, 0x80, 0x20), (Unlock::Bp2EpSrwd, 0x9c, 0x60)] {
            for erase in [false, true] {
                let (master, ctx) = fixture(unlock, sr1, Features::WRSR_WREN, 8192);
                let state = master.0.clone();
                state.lock().unwrap().error_flag = flag;
                let mut device = SpiFlashDevice::new(with_fast_paths(master), ctx);
                let result = if erase {
                    device.erase(0, 4096).await
                } else {
                    device.write(0, &[0]).await
                };
                assert_eq!(
                    result,
                    if erase {
                        Err(Error::EraseError(EraseFailure::CommandFailed { addr: 0 }))
                    } else {
                        Err(Error::WriteError { addr: 0 })
                    }
                );
                let s = state.lock().unwrap();
                if unlock == Unlock::At2x {
                    assert!(s.sectors.iter().all(|p| *p));
                } else {
                    assert_eq!(s.sr1 & 0x9c, sr1);
                }
            }
        }
    });
}

#[test]
fn ready_is_not_a_substitute_for_erase_verification() {
    block_on(async {
        let (master, ctx) = fixture(Unlock::At2x, 0x80, Features::WRSR_WREN, 8192);
        let state = master.0.clone();
        {
            let mut s = state.lock().unwrap();
            s.data.fill(0x5f);
            s.ignore_erase = true;
        }
        let mut device = SpiFlashDevice::new(with_fast_paths(master), ctx);
        assert_eq!(
            device.erase(4096, 4096).await,
            Err(Error::EraseError(EraseFailure::VerifyFailed {
                addr: 4096,
                found: 0x5f
            }))
        );
        assert!(state.lock().unwrap().sectors.iter().all(|p| *p));
    });
}

#[test]
fn unsupported_mutation_and_restore_opcodes_are_rejected_before_unprotect() {
    block_on(async {
        let (master, ctx) = fixture(Unlock::At2x, 0x80, Features::WRSR_WREN, 8192);
        let state = master.0.clone();
        {
            let mut s = state.lock().unwrap();
            s.sectors[0] = false;
            s.deny_opcode = Some(0x36);
        }
        let mut device = SpiFlashDevice::new(master, ctx);
        assert_eq!(
            device.erase(4096, 4096).await,
            Err(Error::OpcodeNotSupported)
        );
        assert!(
            !state
                .lock()
                .unwrap()
                .commands
                .iter()
                .any(|(op, _)| *op == 0x01)
        );
        let (master, ctx) = fixture(Unlock::At2x, 0x80, Features::WRSR_WREN, 8192);
        let state = master.0.clone();
        state.lock().unwrap().deny_opcode = Some(0x02);
        let mut device = SpiFlashDevice::new(master, ctx);
        assert_eq!(device.write(0, &[0]).await, Err(Error::OpcodeNotSupported));
        assert!(state.lock().unwrap().commands.is_empty());
    });
}

#[test]
fn undocumented_read_locks_and_winbond_complement_protection_are_not_cleared() {
    block_on(async {
        let (master, ctx) = fixture(Unlock::Sst26_6, 0, Features::WRSR_WREN, 8192);
        let state = master.0.clone();
        state.lock().unwrap().bpr[0] |= 0x80;
        let mut device = SpiFlashDevice::new(master, ctx);
        assert_eq!(device.erase(0, 4096).await, Err(Error::WriteProtected));
        assert!(
            !state
                .lock()
                .unwrap()
                .commands
                .iter()
                .any(|(op, _)| *op == 0x98)
        );
        let features = Features::WRSR_WREN | Features::WRSR_EXT | Features::WP_WINBOND;
        let (master, ctx) = fixture(Unlock::Status, 0, features, 8192);
        let state = master.0.clone();
        state.lock().unwrap().sr2 |= 0x40;
        let mut device = SpiFlashDevice::new(with_fast_paths(master), ctx);
        assert_eq!(device.erase(0, 4096).await, Err(Error::WriteProtected));
        assert!(
            !state
                .lock()
                .unwrap()
                .commands
                .iter()
                .any(|(op, _)| *op == 0x20)
        );
    });
}

#[test]
fn audited_cmp_and_srl_are_refused_before_status_or_array_mutations() {
    block_on(async {
        for unlock in [Unlock::Status, Unlock::Bp4Srwd] {
            for sr1 in [0, 0x1c] {
                for sr2 in [0x40, 0x01, 0x41] {
                    for fast in [false, true] {
                        for erase in [false, true] {
                            let features =
                                Features::WRSR_WREN | Features::WP_CMP | Features::WP_CMP_SR2;
                            let (master, ctx) = fixture(unlock, sr1, features, 8192);
                            let state = master.0.clone();
                            state.lock().unwrap().sr2 = sr2;
                            let mut device = if fast {
                                ErasedFlashDevice::new(SpiFlashDevice::new(
                                    with_fast_paths(master),
                                    ctx,
                                ))
                            } else {
                                ErasedFlashDevice::new(SpiFlashDevice::new(master, ctx))
                            };
                            let result = if erase {
                                device.erase(0, 4096).await
                            } else {
                                device.write(0, &[0]).await
                            };
                            assert_eq!(result, Err(Error::WriteProtected));
                            let s = state.lock().unwrap();
                            assert_eq!((s.sr1, s.sr2), (sr1, sr2));
                            assert!(s.data.iter().all(|b| *b == 0));
                            assert_eq!(s.bulk_writes, 0);
                            assert!(s.commands.iter().any(|(op, _)| *op == 0x35));
                            assert!(s.commands.iter().all(|(op, _)| matches!(op, 0x05 | 0x35)));
                        }
                    }
                }
            }
        }
    });
}

#[test]
fn w25q128_array_mutations_preserve_status_registers() {
    block_on(async {
        for sr1 in [0, 0x1c] {
            let (master, mut ctx) = fixture(
                Unlock::Status,
                sr1,
                Features::WRSR_WREN | Features::WRSR_EXT | Features::WP_WINBOND,
                8192,
            );
            ctx.chip.jedec_manufacturer = 0xef;
            ctx.chip.jedec_device = 0x4018;
            let state = master.0.clone();
            let mut device = SpiFlashDevice::new(with_fast_paths(master), ctx);
            device.erase(0, 4096).await.unwrap();
            device.write(0, &[0x5a]).await.unwrap();
            let state = state.lock().unwrap();
            assert_eq!((state.sr1, state.sr2), (sr1, 0x02));
            assert_eq!(state.data[0], 0x5a);
            assert!(
                state
                    .commands
                    .iter()
                    .filter(|(op, _)| *op == 0x01)
                    .all(|(_, bytes)| bytes.len() == 2)
            );
        }
    });
}

#[test]
fn known_wps_is_checked_and_ambiguous_shared_id_fails_closed() {
    block_on(async {
        let features = Features::WRSR_WREN | Features::WRSR_EXT | Features::WP_WINBOND;
        for (wps_known, sr3, fail) in [
            (false, 0, false),
            (true, 0, false),
            (true, 4, false),
            (true, 0, true),
            (true, 0xff, false),
        ] {
            let (master, mut ctx) = fixture(
                Unlock::Status,
                0,
                features
                    | if wps_known {
                        Features::WP_WPS
                    } else {
                        Features::empty()
                    },
                8192,
            );
            ctx.chip.jedec_manufacturer = 0xef;
            ctx.chip.jedec_device = 0x4018;
            let state = master.0.clone();
            state.lock().unwrap().sr3 = sr3;
            if fail {
                state.lock().unwrap().deny_opcode = Some(0x15);
            }
            let mut device = SpiFlashDevice::new(master, ctx);
            let result = device.read_wp_config().await;
            assert_eq!(result.is_ok(), wps_known && sr3 == 0 && !fail);
            assert_eq!(
                state
                    .lock()
                    .unwrap()
                    .commands
                    .iter()
                    .any(|(op, _)| *op == 0x15),
                wps_known
            );
        }
    });
}

#[test]
fn readable_sr2_without_documented_width_is_not_overwritten() {
    block_on(async {
        for sr2 in [0, 2] {
            let (master, ctx) = fixture(
                Unlock::Status,
                0x1c,
                Features::WRSR_WREN | Features::STATUS_REG_2,
                8192,
            );
            let state = master.0.clone();
            state.lock().unwrap().sr2 = sr2;
            let mut device = SpiFlashDevice::new(master, ctx);
            assert_eq!(device.erase(0, 4096).await, Err(Error::ChipNotSupported));
            let state = state.lock().unwrap();
            assert_eq!(state.sr2, sr2);
            assert!(!state.commands.iter().any(|(op, _)| *op == 0x01));
        }
    });
}

#[test]
fn extended_status_still_checks_cmp_and_srl() {
    block_on(async {
        for sr1 in [0, 0x1c] {
            for sr2 in [0x01, 0x40] {
                let features = Features::WRSR_WREN | Features::WRSR_EXT | Features::WP_CMP;
                let (master, ctx) = fixture(Unlock::Bp2Srwd, sr1, features, 8192);
                let state = master.0.clone();
                state.lock().unwrap().sr2 = sr2;
                let mut device = SpiFlashDevice::new(master, ctx);
                assert_eq!(device.erase(0, 4096).await, Err(Error::WriteProtected));
                assert!(
                    state
                        .lock()
                        .unwrap()
                        .commands
                        .iter()
                        .all(|(op, _)| matches!(op, 0x05 | 0x35))
                );
            }
        }
    });
}

#[test]
fn macronix_qpi_does_not_receive_rdsr2_opcode() {
    block_on(async {
        for sr1 in [0, 0x1c] {
            let features = Features::WRSR_WREN | Features::STATUS_REG_2 | Features::QE_SR2;
            let (master, mut ctx) = fixture(Unlock::Bp2Srwd, sr1, features, 8192);
            ctx.chip.jedec_manufacturer = 0xc2;
            let state = master.0.clone();
            let mut device = SpiFlashDevice::new(master, ctx);
            let result = device.erase(0, 4096).await;
            assert_eq!(
                result,
                if sr1 == 0 {
                    Ok(())
                } else {
                    Err(Error::ChipNotSupported)
                }
            );
            let state = state.lock().unwrap();
            assert!(
                !state
                    .commands
                    .iter()
                    .any(|(op, _)| *op == 0x35 || *op == 0x01)
            );
        }
    });
}

#[test]
fn cmp_register_requires_documented_extended_status_write() {
    block_on(async {
        for extended in [false, true] {
            let features = Features::WRSR_WREN | Features::WP_CMP | Features::WP_CMP_SR2;
            let features = if extended {
                features | Features::WRSR_EXT
            } else {
                features
            };
            let (master, ctx) = fixture(Unlock::Bp4Srwd, 0x1c, features, 8192);
            let state = master.0.clone();
            let mut device = SpiFlashDevice::new(with_fast_paths(master), ctx);
            let result = device.erase(0, 4096).await;
            if !extended {
                assert_eq!(result, Err(Error::ChipNotSupported));
                let s = state.lock().unwrap();
                assert_eq!((s.sr1, s.sr2), (0x1c, 0x02));
                assert!(s.commands.iter().all(|(op, _)| *op != 0x01));
                continue;
            }
            result.unwrap();
            device.write(0, &[0x5a]).await.unwrap();
            let s = state.lock().unwrap();
            assert_eq!((s.sr1, s.sr2), (0x1c, 0x02));
            assert_eq!(s.data[0], 0x5a);
            assert_eq!(s.bulk_writes, 1);
            let writes: Vec<_> = s.commands.iter().filter(|(op, _)| *op == 0x01).collect();
            assert!(!writes.is_empty());
            assert!(writes.iter().all(|(_, bytes)| bytes.len() == 2));
        }
    });
}

#[test]
fn unknown_cmp_layouts_and_unavailable_sr2_reads_do_not_unlock() {
    block_on(async {
        for sr1 in [0, 0x1c] {
            let features = Features::WRSR_WREN | Features::WP_CMP;
            let (master, ctx) = fixture(Unlock::Bp2Srwd, sr1, features, 8192);
            let state = master.0.clone();
            let mut device = SpiFlashDevice::new(master, ctx);
            assert_eq!(device.erase(0, 4096).await, Err(Error::ChipNotSupported));
            assert!(
                !state
                    .lock()
                    .unwrap()
                    .commands
                    .iter()
                    .any(|(op, _)| *op == 0x01)
            );

            let (master, ctx) = fixture(Unlock::Status, sr1, features | Features::WP_CMP_SR2, 8192);
            let state = master.0.clone();
            state.lock().unwrap().deny_opcode = Some(0x35);
            let mut device = SpiFlashDevice::new(master, ctx);
            assert_eq!(device.write(0, &[0]).await, Err(Error::OpcodeNotSupported));
            assert!(
                !state
                    .lock()
                    .unwrap()
                    .commands
                    .iter()
                    .any(|(op, _)| *op == 0x01)
            );
        }
    });
}

#[test]
fn spi_and_bulk_aai_programming_preserve_protection_and_adjacent_bytes() {
    block_on(async {
        for (aai, bulk) in [(false, false), (true, false), (true, true)] {
            for (addr, len) in [(0, 256), (1, 256), (0, 257), (1, 1)] {
                let features = Features::WRSR_WREN
                    | if aai {
                        Features::AAI_WORD
                    } else {
                        Features::empty()
                    };
                let (master, mut ctx) = fixture(Unlock::Status, 0x3c, features, 8192);
                ctx.chip.write_granularity = WriteGranularity::Byte;
                let state = master.0.clone();
                {
                    let mut s = state.lock().unwrap();
                    s.data.fill(0xff);
                    s.bulk_aai = bulk;
                }
                let mut device = if bulk {
                    ErasedFlashDevice::new(SpiFlashDevice::new(with_fast_paths(master), ctx))
                } else {
                    ErasedFlashDevice::new(SpiFlashDevice::new(master, ctx))
                };
                let data: Vec<_> = (0..len).map(|i| i as u8).collect();
                device.write(addr, &data).await.unwrap();
                let mut readback = vec![0; len];
                device.read(addr, &mut readback).await.unwrap();
                assert_eq!(readback, data);
                let s = state.lock().unwrap();
                assert_eq!(s.bulk_writes, usize::from(bulk));
                assert_eq!(s.sr1, 0x3c);
                assert_eq!(s.aai_next, None);
                assert!(s.data[..addr as usize].iter().all(|b| *b == 0xff));
                assert!(s.data[addr as usize + len..].iter().all(|b| *b == 0xff));
                assert!(
                    s.commands
                        .iter()
                        .filter(|(op, _)| *op == 0x02)
                        .all(|(_, bytes)| bytes.len() == 1)
                );
                let words = s.commands.iter().filter(|(op, _)| *op == 0xad).count();
                assert_eq!(
                    words,
                    if aai {
                        (len - addr as usize % 2) / 2
                    } else {
                        0
                    }
                );
            }
        }
    });
}

#[test]
fn aai_transfer_failures_exit_mode_before_restoring_protection() {
    block_on(async {
        for (fast, fail_at) in [false, true]
            .into_iter()
            .flat_map(|fast| [0, 2].map(|addr| (fast, addr)))
        {
            let features = Features::WRSR_WREN | Features::AAI_WORD;
            let (master, ctx) = fixture(Unlock::Status, 0x3c, features, 8192);
            let state = master.0.clone();
            {
                let mut s = state.lock().unwrap();
                s.data.fill(0xff);
                s.fail_aai_at = Some(fail_at);
                s.bulk_aai = fast;
            }
            let mut device = if fast {
                ErasedFlashDevice::new(SpiFlashDevice::new(with_fast_paths(master), ctx))
            } else {
                ErasedFlashDevice::new(SpiFlashDevice::new(master, ctx))
            };
            assert_eq!(
                device.write(0, &[0x55; 4]).await,
                Err(Error::SpiTransferFailed)
            );
            let s = state.lock().unwrap();
            assert_eq!(s.aai_next, None);
            assert_eq!(s.sr1, 0x3c);
            let aai = s.commands.iter().rposition(|(op, _)| *op == 0xad).unwrap();
            let wrdi = s.commands.iter().rposition(|(op, _)| *op == 0x04).unwrap();
            let restore = s.commands.iter().rposition(|(op, _)| *op == 0x01).unwrap();
            assert!(aai < wrdi && wrdi < restore);
        }
    });
}

#[test]
fn aai_preflight_checks_only_opcodes_used_by_each_segment() {
    block_on(async {
        for (addr, data, denied, accepted) in [
            (0, vec![0x55], 0xad, true),
            (1, vec![0x55], 0xad, true),
            (1, vec![0x55; 2], 0xad, true),
            (0, vec![0x55; 2], 0x02, true),
            (0, vec![0x55; 3], 0x02, false),
            (1, vec![0x55; 3], 0xad, false),
            (1, vec![0x55; 4], 0x02, false),
        ] {
            let features = Features::WRSR_WREN | Features::AAI_WORD;
            let (master, ctx) = fixture(Unlock::Status, 0x3c, features, 8192);
            let state = master.0.clone();
            {
                let mut s = state.lock().unwrap();
                s.data.fill(0xff);
                s.deny_opcode = Some(denied);
            }
            let mut device = SpiFlashDevice::new(master, ctx);
            assert_eq!(
                device.write(addr, &data).await,
                if accepted {
                    Ok(())
                } else {
                    Err(Error::OpcodeNotSupported)
                }
            );
            let s = state.lock().unwrap();
            assert_eq!(s.sr1, 0x3c);
            if accepted {
                assert_eq!(&s.data[addr as usize..addr as usize + data.len()], data);
                assert!(!s.commands.iter().any(|(op, _)| *op == denied));
            } else {
                assert!(s.commands.is_empty());
            }
        }
    });
}

#[test]
fn erase_poll_error_waits_for_the_erase_budget_before_restoring_protection() {
    block_on(async {
        // Exercise sector, block, addressed die and addressless chip erase timing.
        for (opcode, block_size, count, busy_us) in [
            (0x20, 4096, 2, 6_000_000),
            (0x52, 8192, 2, 20_000_000),
            (0xc4, 512 * 1024, 2, 300_000_000),
            (0xc7, 8192, 1, 60_000_000),
        ] {
            let (master, mut ctx) = fixture(
                Unlock::Bp2Srwd,
                0x9c,
                Features::WRSR_WREN,
                (block_size * count) as usize,
            );
            ctx.chip.erase_blocks = vec![EraseBlock::with_count(opcode, block_size, count)];
            let state = master.0.clone();
            {
                let mut s = state.lock().unwrap();
                s.erase_busy_us = busy_us;
                s.fail_busy_read = true;
            }
            let mut device = SpiFlashDevice::new(master, ctx);
            assert_eq!(
                device.erase(0, block_size).await,
                Err(Error::SpiTransferFailed)
            );
            let s = state.lock().unwrap();
            assert_eq!(s.busy_us, 0);
            assert_eq!(s.sr1, 0x9c);
            assert!(s.data[..block_size as usize].iter().all(|b| *b == 0xff));
            assert!(s.data[block_size as usize..].iter().all(|b| *b == 0));
        }
    });
}

#[test]
fn restoration_failure_is_not_hidden_by_mutation_success_or_failure() {
    block_on(async {
        for fail_mutation in [false, true] {
            let (master, ctx) = fixture(Unlock::At2x, 0x80, Features::WRSR_WREN, 8192);
            {
                let mut s = master.0.lock().unwrap();
                s.fail_restore = true;
                s.fail_mutation = fail_mutation;
            }
            let mut device = SpiFlashDevice::new(master, ctx);
            assert_eq!(
                device.erase(0, 4096).await,
                Err(Error::ProtectionRestoreFailed)
            );
        }
    });
}

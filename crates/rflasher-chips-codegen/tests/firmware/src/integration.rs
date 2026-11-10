//! Exercise the real allocation-free core path, not just descriptor lookup.

use super::catalog::{CATALOG, CHIPS};
use core::{
    cell::Cell,
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};
use rflasher_core::{
    error::{Error, Result},
    flash::{FlashDevice, OptimalEraseOp, SpiFlashDevice, probe_catalog},
    programmer::{FlashAccess, SpiFeatures, SpiMaster},
    protocol,
    spi::SpiCommand,
};
use std::alloc::{GlobalAlloc, Layout, System};

std::thread_local! {
    static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
}

struct CountAllocations;
unsafe impl GlobalAlloc for CountAllocations {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| {
            if let Some(n) = count.get() {
                count.set(Some(n + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountAllocations = CountAllocations;

// The test master completes synchronously, as do firmware MMIO controllers.
fn run<F: Future>(future: F) -> F::Output {
    match pin!(future)
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("test SPI master unexpectedly suspended"),
    }
}

struct MemorySpi {
    flash: [u8; 8192],
    id: [u8; 4],
    status: u8,
    enabled: bool,
    busy: u8,
    stuck: bool,
    fail_program: bool,
    deny: bool,
    polls: usize,
    delays: usize,
    programs: usize,
    erases: usize,
    status_writes: usize,
}
impl MemorySpi {
    fn new() -> Self {
        Self {
            flash: [0; 8192],
            id: [0xbf, 0x25, 0x41, 0xff],
            status: 0x1c,
            enabled: false,
            busy: 0,
            stuck: false,
            fail_program: false,
            deny: false,
            polls: 0,
            delays: 0,
            programs: 0,
            erases: 0,
            status_writes: 0,
        }
    }
}
impl SpiMaster for MemorySpi {
    fn features(&self) -> SpiFeatures {
        SpiFeatures::empty()
    }
    fn max_read_len(&self) -> usize {
        64
    }
    fn max_write_len(&self) -> usize {
        64
    }
    fn probe_opcode(&self, opcode: u8) -> bool {
        // Deliberately no AAI/WRDI and no large erasers, like an Intel menu.
        matches!(
            opcode,
            0x9f | 0x05 | 0x06 | 0x50 | 0x01 | 0x20 | 0x02 | 0x03
        )
    }
    fn check_access(&self, access: FlashAccess, _addr: u32, _len: u32) -> Result<()> {
        if self.deny && access == FlashAccess::Modify {
            Err(Error::RegionProtected)
        } else {
            Ok(())
        }
    }
    async fn delay_us(&mut self, _us: u32) {
        self.delays += 1;
    }
    async fn execute(&mut self, command: &mut SpiCommand<'_>) -> Result<()> {
        match command.opcode {
            0x9f => command
                .read_buf
                .copy_from_slice(&self.id[..command.read_buf.len()]),
            0x05 => {
                self.polls += 1;
                let busy = self.stuck || self.busy > 0;
                self.busy = self.busy.saturating_sub(1);
                command.read_buf[0] = self.status | u8::from(busy) | u8::from(self.enabled) << 1;
            }
            0x06 | 0x50 => self.enabled = true,
            0x01 => {
                if !self.enabled {
                    return Err(Error::WriteProtected);
                }
                self.status = command.write_data[0] & !3;
                self.enabled = false;
                self.busy = 2;
                self.status_writes += 1;
            }
            0x03 => {
                let start = command.address.unwrap() as usize;
                command
                    .read_buf
                    .copy_from_slice(&self.flash[start..start + command.read_buf.len()]);
            }
            0x02 | 0x20 => {
                if !self.enabled || self.status & 0x3c != 0 {
                    return Err(Error::WriteProtected);
                }
                let start = command.address.unwrap() as usize;
                if command.opcode == 0x20 {
                    assert_eq!(start % 4096, 0);
                    self.flash[start..start + 4096].fill(0xff);
                    self.erases += 1;
                } else {
                    if self.fail_program {
                        return Err(Error::SpiTransferFailed);
                    }
                    // SST25VF016B requires byte program when AAI is unavailable.
                    assert_eq!(command.write_data.len(), 1);
                    self.flash[start] &= command.write_data[0];
                    self.programs += 1;
                }
                self.busy = 2;
                self.enabled = false;
            }
            _ => return Err(Error::OpcodeNotSupported),
        }
        Ok(())
    }
}

#[test]
fn identify_erase_program_read_and_restore_without_allocating() {
    let mut master = MemorySpi::new();
    ALLOCATIONS.with(|n| n.set(Some(0)));
    let context = run(probe_catalog(&mut master, &CATALOG)).unwrap();
    assert_eq!(context.chip.name(), "SST25VF016B");
    let mut device = SpiFlashDevice::new(master, context);
    run(device.erase(4096, 4096)).unwrap();
    run(device.write(4096, &[1, 2, 3, 4])).unwrap();
    let mut read = [0; 4];
    run(device.read(4096, &mut read)).unwrap();
    let allocations = ALLOCATIONS.with(|n| n.replace(None).unwrap());
    assert_eq!(allocations, 0);
    assert_eq!(read, [1, 2, 3, 4]);
    let (master, _) = device.into_parts();
    assert!(master.flash[..4096].iter().all(|&b| b == 0));
    assert!(master.flash[4100..].iter().all(|&b| b == 0xff));
    assert_eq!(master.status, 0x1c);
    assert_eq!(master.status_writes, 4); // Unprotect/restore once per operation.
    assert_eq!(master.erases, 1);
    assert_eq!(master.programs, 4);
    assert!(master.polls > 12);
    assert!(master.delays > 4);
}

#[test]
fn defaults_are_applied_by_the_actual_probe_path() {
    let mut master = MemorySpi::new();
    master.id = [0xef, 0x40, 0x18, 0xff];
    let context = run(probe_catalog(&mut master, &CATALOG)).unwrap();
    assert_eq!(context.chip.name(), "W25Q128FVJV-WP");
    master.id = [0xff; 4];
    assert_eq!(
        run(probe_catalog(&mut master, &CATALOG)).unwrap_err(),
        Error::ChipNotFound
    );
    assert_eq!(master.status_writes, 0);
    master.id = [0x7f, 0x37, 0x20, 0x21];
    assert_eq!(
        run(probe_catalog(&mut master, &CATALOG))
            .unwrap()
            .chip
            .name(),
        "A25L10PT"
    );
}

#[test]
fn failed_program_restores_protection() {
    let mut master = MemorySpi::new();
    master.fail_program = true;
    let context = run(probe_catalog(&mut master, &CATALOG)).unwrap();
    let mut device = SpiFlashDevice::new(master, context);
    assert_eq!(run(device.write(4096, &[1])), Err(Error::SpiTransferFailed));
    let (master, _) = device.into_parts();
    assert_eq!(master.status, 0x1c);
    assert_eq!(master.status_writes, 2);
}

#[test]
fn bad_geometry_and_protected_ranges_are_refused_before_unprotecting() {
    let mut master = MemorySpi::new();
    let context = run(probe_catalog(&mut master, &CATALOG)).unwrap();
    let mut device = SpiFlashDevice::new(master, context);
    assert_eq!(run(device.erase(2048, 4096)), Err(Error::InvalidAlignment));
    let forged = OptimalEraseOp {
        start: 4096,
        size: 4096,
        erase_block: CHIPS[4].erase_blocks()[2].clone(), // 64-KiB, not a 4-KiB eraser.
    };
    assert_eq!(
        run(device.erase_operation(&forged)),
        Err(Error::InvalidAlignment)
    );
    device.master().deny = true;
    assert_eq!(run(device.erase(4096, 4096)), Err(Error::RegionProtected));
    let (master, _) = device.into_parts();
    assert_eq!(master.status_writes, 0);
    assert_eq!(master.programs + master.erases, 0);
}

#[test]
fn busy_is_polled_and_a_stuck_chip_times_out() {
    let mut master = MemorySpi::new();
    master.busy = 2;
    run(protocol::wait_ready(&mut master, 100, 1000)).unwrap();
    assert_eq!(master.polls, 3);
    assert_eq!(master.delays, 2);
    master.stuck = true;
    assert_eq!(
        run(protocol::wait_ready(&mut master, 100, 300)),
        Err(Error::Timeout)
    );
    assert_eq!(master.polls, 6);
    assert_eq!(master.delays, 5);
}

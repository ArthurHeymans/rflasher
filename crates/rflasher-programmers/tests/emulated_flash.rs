//! End-to-end flash operations against the strict in-memory chip emulator.
//!
//! The emulator (`dummy`) behaves like a real SPI NOR chip where it matters:
//! page programs wrap inside their page, program/erase need WREN and are
//! silently ignored otherwise or under block protection, operations keep WIP
//! raised for a while, and the address width must match the opcode and mode.
//! Running the real erase/write planner against it therefore checks the
//! properties that count on hardware without needing any:
//!
//! - a smart write leaves exactly the requested bytes changed;
//! - data outside the region survives the erase-restore dance;
//! - 4-byte addressing reaches beyond 16 MiB and leaves the chip in 3-byte
//!   mode afterwards;
//! - a protected chip is reported instead of pretending to have erased.

#![cfg(feature = "dummy")]

use futures_lite::future::block_on;
use proptest::prelude::*;
use rflasher_core::chip::{
    ChipTestStatus, EraseBlock, EraseRegion, Features, FlashChip, WriteGranularity,
};
use rflasher_core::error::{EraseFailure, Error};
use rflasher_core::flash::unified::{self, NoProgress};
use rflasher_core::flash::{self, FlashContext, FlashDevice, SpiFlashDevice};
use rflasher_core::spi::opcodes;
use rflasher_programmers::dummy::{DummyConfig, DummyFlash};

/// Simulated durable storage for the in-memory emulator (not production I/O).
#[derive(Default)]
struct RecoveryImage(Vec<u8>);
impl rflasher_core::flash::RecoveryBackup for RecoveryImage {
    fn persist(&mut self, image: &[u8]) -> rflasher_core::Result<()> {
        self.0 = image.to_vec();
        Ok(())
    }
}

const KIB: u32 = 1024;
const MIB: u32 = 1024 * KIB;

/// A chip with 4 KiB / 32 KiB / 64 KiB erase blocks and a chip erase
fn chip(size: u32, features: Features, four_byte_erase: bool) -> FlashChip {
    let block = |opcode: u8, opcode_4b: u8, block_size: u32| {
        EraseBlock::with_regions_and_4b(
            opcode,
            four_byte_erase.then_some(opcode_4b),
            &[EraseRegion::new(block_size, size / block_size)],
        )
    };
    FlashChip {
        vendor: "Emulated".into(),
        name: "EMU".into(),
        jedec_manufacturer: 0xEF,
        jedec_device: 0x4018,
        total_size: size,
        page_size: 256,
        features: Features::WRSR_WREN | features,
        voltage_min_mv: 2700,
        voltage_max_mv: 3600,
        write_granularity: WriteGranularity::Page,
        erase_blocks: vec![
            block(opcodes::SE_20, opcodes::SE_21, 4 * KIB),
            block(opcodes::BE_52, opcodes::BE_5C, 32 * KIB),
            block(opcodes::BE_D8, opcodes::BE_DC, 64 * KIB),
            EraseBlock::new(opcodes::CE_C7, size),
        ],
        tested: ChipTestStatus::default(),
    }
}

fn emulator(size: u32, busy_polls: u32, initial: &[u8]) -> DummyFlash {
    DummyFlash::with_data(
        DummyConfig {
            size: size as usize,
            busy_polls,
            ..DummyConfig::default()
        },
        initial,
    )
}

/// Deterministic pseudo-random bytes (xorshift), so that a failing case is
/// fully described by its seed and proptest can shrink the parameters.
fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// An image where roughly a quarter of the bytes are erased (0xFF), like
/// firmware with padding, and the rest is noise.
fn firmware_like(seed: u64, len: usize) -> Vec<u8> {
    let picks = noise(seed ^ 0x5555, len);
    noise(seed, len)
        .into_iter()
        .zip(picks)
        .map(|(byte, pick)| if pick < 64 { 0xFF } else { byte })
        .collect()
}

/// `base` with about one byte in `1 / period` altered: the shape of a typical
/// firmware update, and what makes the planner skip, partially erase or
/// promote to bigger blocks.
fn mutate(base: &[u8], seed: u64, period: u8) -> Vec<u8> {
    let picks = noise(seed, base.len());
    let values = noise(seed ^ 0xAAAA, base.len());
    base.iter()
        .zip(picks.iter().zip(values))
        .map(
            |(&byte, (&pick, value))| {
                if pick % period == 0 { value } else { byte }
            },
        )
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// A smart write changes the region to exactly the requested data and
    /// touches nothing else, whatever the alignment, size, similarity to the
    /// current contents and busy time.
    #[test]
    fn smart_write_region_updates_only_its_region(
        seed in any::<u64>(),
        addr in 0u32..(128 * KIB),
        len in 1u32..(48 * KIB),
        period in 1u8..=255,
        busy_polls in 0u32..30,
    ) {
        const SIZE: u32 = 128 * KIB;
        let len = len.min(SIZE - addr);
        let (start, end) = (addr as usize, (addr + len) as usize);

        let initial = firmware_like(seed, SIZE as usize);
        let target = mutate(&initial[start..end], seed.rotate_left(17), period);

        let mut device = SpiFlashDevice::new(
            emulator(SIZE, busy_polls, &initial),
            FlashContext::new(chip(SIZE, Features::empty(), false)),
        );
        let mut backup = RecoveryImage::default();
        let mut policy = rflasher_core::flash::MutationPolicy { recovery: Some(&mut backup), ..Default::default() };
        block_on(unified::smart_write_region_with_policy(&mut device, addr, &target, &mut NoProgress, &mut policy))
            .expect("smart write");
        if !backup.0.is_empty() { prop_assert_eq!(&backup.0, &initial); }

        let flash = device.master().data();
        prop_assert_eq!(&flash[start..end], &target[..], "region contents");
        prop_assert_eq!(&flash[..start], &initial[..start], "data before the region");
        prop_assert_eq!(&flash[end..], &initial[end..], "data after the region");
    }

    /// The low-level write splits at page boundaries: unaligned writes of any
    /// length land exactly where asked, with nothing spilling into (or, via
    /// page wrap-around, back into) neighbouring bytes.
    #[test]
    fn page_program_splitting_is_exact(
        seed in any::<u64>(),
        addr in 0u32..(60 * KIB),
        len in 1usize..3000,
    ) {
        const SIZE: u32 = 64 * KIB;
        let ctx = FlashContext::new(chip(SIZE, Features::empty(), false));
        let mut flash = emulator(SIZE, 1, &[]);
        let data = noise(seed, len);

        block_on(flash::write(&mut flash, &ctx, addr, &data)).expect("write");

        let flash = flash.data();
        let (start, end) = (addr as usize, addr as usize + len);
        prop_assert_eq!(&flash[start..end], &data[..]);
        prop_assert!(flash[..start].iter().all(|&b| b == 0xFF), "bytes before");
        prop_assert!(flash[end..].iter().all(|&b| b == 0xFF), "bytes after");
    }
}

/// Smart-write across the 16 MiB boundary with each 4-byte addressing style.
fn four_byte_roundtrip(features: Features, four_byte_erase: bool) {
    const SIZE: u32 = 32 * MIB;
    let ctx = FlashContext::new(chip(SIZE, features, four_byte_erase));
    assert!(
        ctx.chip.requires_4byte_addr(),
        "a 32 MiB chip is only reachable with 4-byte addressing"
    );
    let mut device = SpiFlashDevice::new(emulator(SIZE, 2, &[]), ctx);

    // Straddles 16 MiB, unaligned, so partial-block preservation is exercised
    // on both sides of the boundary.
    let addr = 16 * MIB - 1500;
    let old = firmware_like(1, 6000);
    let mut backup = RecoveryImage::default();
    let mut policy = rflasher_core::flash::MutationPolicy {
        recovery: Some(&mut backup),
        ..Default::default()
    };
    block_on(unified::smart_write_region_with_policy(
        &mut device,
        addr,
        &old,
        &mut NoProgress,
        &mut policy,
    ))
    .expect("initial write");
    let new = mutate(&old, 2, 3);
    block_on(unified::smart_write_region_with_policy(
        &mut device,
        addr,
        &new,
        &mut NoProgress,
        &mut policy,
    ))
    .expect("update write");

    let start = addr as usize;
    assert_eq!(&device.master().data()[start..start + new.len()], &new[..]);
    block_on(unified::verify(&mut device, &new, addr)).expect("verify");
    assert!(
        !device.master().in_4byte_mode(),
        "chip must be returned to 3-byte mode for the next user"
    );
}

#[test]
fn four_byte_addressing_by_mode_switch() {
    four_byte_roundtrip(Features::FOUR_BYTE_ADDR | Features::FOUR_BYTE_ENTER, false);
}

#[test]
fn four_byte_addressing_with_native_opcodes() {
    four_byte_roundtrip(
        Features::FOUR_BYTE_ADDR
            | Features::FOUR_BYTE_NATIVE
            | Features::FOUR_BYTE_READ
            | Features::FOUR_BYTE_FAST_READ
            | Features::FOUR_BYTE_PROGRAM,
        true,
    );
}

/// Erasing a block-protected chip must fail loudly: the chip ignores the
/// command without any error, so only the read-back can tell.
#[test]
fn erase_of_a_protected_chip_is_reported() {
    const SIZE: u32 = 128 * KIB;
    let mut flash = emulator(SIZE, 2, &vec![0x00; SIZE as usize]);
    // BP = 7: the whole chip is protected.
    flash.set_block_protect(opcodes::SR1_BP0 | opcodes::SR1_BP1 | opcodes::SR1_BP2);
    let mut device = SpiFlashDevice::new(
        flash,
        FlashContext::new(chip(SIZE, Features::empty(), false)),
    );

    for (addr, len) in [(0, 4 * KIB), (64 * KIB, 64 * KIB), (0, SIZE)] {
        let err = block_on(device.erase(addr, len)).unwrap_err();
        assert!(
            matches!(
                err,
                Error::EraseError(EraseFailure::VerifyFailed { addr: at, found: 0x00 })
                    if at == addr
            ),
            "erase({addr:#x}, {len:#x}): {err:?}"
        );
    }
    assert!(device.master().data().iter().all(|&b| b == 0x00));
}

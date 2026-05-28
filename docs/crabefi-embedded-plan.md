# Plan: Make rflasher usable from CrabEFI

## Goal

Make rflasher's internal Intel/AMD SPI flash programming support reusable in two environments:

1. Linux userspace `rflasher` application.
2. CrabEFI / firmware `no_std` embedded environment.

The end state should keep one shared implementation of Intel ICH/PCH and AMD SPI100 flash controller logic, with thin host-specific adapters for Linux and CrabEFI.

## Current state

`rflasher-core` is already close to reusable:

- It is `no_std` capable.
- It has useful abstractions such as `SpiMaster`, `OpaqueMaster`, `FlashDevice`, SFDP parsing, flash layout handling, and erase planning.
- It supports synchronous mode via `maybe_async` / `is_sync`.

`rflasher-internal` is not directly usable by CrabEFI today:

- Intel/AMD controller implementations are gated on `#[cfg(all(feature = "std", target_os = "linux"))]`.
- It directly depends on Linux mechanisms:
  - `/sys/bus/pci/devices`
  - `/sys/.../config`
  - `/dev/mem` + `mmap`
  - `libc::iopl`
  - `std::thread::sleep`
- It uses userspace-oriented dynamic dispatch (`Box<dyn Controller>`) in the high-level internal programmer.
- `cargo check -p rflasher-internal --no-default-features --target x86_64-unknown-none` currently fails.

CrabEFI already has working embedded code for:

- PCI config access: `src/drivers/pci/access.rs`
- MMIO access: `src/drivers/mmio.rs`
- Intel SPI: `src/drivers/spi/intel.rs`
- AMD SPI100: `src/drivers/spi/amd.rs`
- Variable storage adapter: `src/efi/varstore/storage.rs`

But rflasher has more complete Intel chipset coverage and implemented logic that CrabEFI still has as TODOs, such as FRAP/FREG access permission checks and protected range reporting.

## Design direction

Separate chipset/controller logic from host access.

The controller code should not know whether PCI/MMIO access comes from Linux sysfs/devmem or from CrabEFI's direct firmware mappings. Linux and CrabEFI should both implement a small host access interface.

## Investigation notes

The high-level direction is sound, but the original version of this plan was too broad for one safe change. The first useful milestone is compile-time and unit-test coverage for the host abstraction boundary, not a full Intel/AMD controller rewrite.

Important corrections:

- `HostAccess` must include or inherit PCI configuration access. SPI BAR discovery, AMD ROM range discovery, and Intel BIOS write-enable logic all require PCI config reads/writes; MMIO and delays alone are insufficient.
- `Bdf` needs a PCI segment/domain. UEFI PCI access is segment-aware, and the current `PciDevice` already carries `domain`.
- `rflasher-internal` must expose an `is_sync` feature and have `embedded-host` imply it. The current controller/programmer implementations are synchronous while `rflasher-core` traits are async unless `is_sync` is enabled.
- Avoid introducing a new flash trait until there is a concrete gap in `rflasher-core`'s existing `SpiMaster`, `OpaqueMaster`, or `FlashDevice` abstractions.
- Scratch-buffer write APIs are still desirable for capsule updates, but they are a separate follow-up from the host/controller split.

Implemented first slice:

- Added `PciConfigAccess`, `MmioAccess`, `HostAccess`, and segment-aware `Bdf` abstractions.
- Added `linux-host`, `embedded-host`, `alloc`, and `is_sync` features to `rflasher-internal`.
- Added pure PCI-device-list chipset detection helpers for embedded callers.
- Added host-backed Intel SPI BAR discovery and AMD SPI100 BAR/ROM range discovery helpers.
- Added fake-host unit tests for the embedded abstraction boundary.
- Added CI checks for `x86_64-unknown-none` no-std builds.

## Proposed crate/feature layout

Keep the existing crate names, but split features more clearly.

### `rflasher-core`

Keep as protocol/chip/flash core.

Desired feature combinations:

- `default = []`
- `std`
- `alloc`
- `is_sync`
- `static-chips`

For CrabEFI, expected dependency mode:

```toml
rflasher-core = { path = "../rflasher/crates/rflasher-core", default-features = false, features = ["is_sync"] }
```

If embedded code needs allocation-backed helpers:

```toml
features = ["alloc", "is_sync"]
```

### `rflasher-internal`

Add host backend features:

```toml
[features]
default = ["std", "linux-host", "alloc", "is_sync"]
std = ["alloc", "is_sync", "rflasher-core/std", "dep:thiserror", "dep:libc"]
alloc = ["rflasher-core/alloc"]
is_sync = ["rflasher-core/is_sync"]
linux-host = ["std"]
embedded-host = ["is_sync"]
```

CrabEFI should be able to use:

```toml
rflasher-internal = { path = "../rflasher/crates/rflasher-internal", default-features = false, features = ["embedded-host"] }
```

## Step 1: Introduce host access traits

Create a module such as `crates/rflasher-internal/src/host.rs`.

Suggested traits:

```rust
pub trait PciConfigAccess {
    fn read8(&self, bdf: Bdf, offset: u16) -> Result<u8>;
    fn read16(&self, bdf: Bdf, offset: u16) -> Result<u16>;
    fn read32(&self, bdf: Bdf, offset: u16) -> Result<u32>;

    fn write8(&self, bdf: Bdf, offset: u16, value: u8) -> Result<()>;
    fn write16(&self, bdf: Bdf, offset: u16, value: u16) -> Result<()>;
    fn write32(&self, bdf: Bdf, offset: u16, value: u32) -> Result<()>;
}

pub trait MmioAccess {
    fn read8(&self, offset: usize) -> u8;
    fn read16(&self, offset: usize) -> u16;
    fn read32(&self, offset: usize) -> u32;

    fn write8(&self, offset: usize, value: u8);
    fn write16(&self, offset: usize, value: u16);
    fn write32(&self, offset: usize, value: u32);
}

pub trait HostAccess {
    type MmioRegion: MmioAccess;

    unsafe fn map_mmio(&self, phys_addr: u64, size: usize) -> Result<Self::MmioRegion>;
    fn delay_us(&self, us: u32);
}
```

Also add a small `Bdf` type independent of Linux sysfs naming:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bdf {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}
```

## Step 2: Move Linux-specific access behind a Linux backend

Create `crates/rflasher-internal/src/host/linux.rs`, gated by `linux-host`.

Move or wrap the current Linux-specific pieces there:

- sysfs PCI scanning and config file access
- direct I/O port config access for hidden devices
- `/dev/mem` mapping
- `std::thread::sleep`

The existing CLI should keep working by constructing a `LinuxHost` and passing it into the internal programmer.

## Step 3: Make controller implementations generic over host access

Refactor Intel and AMD controllers from Linux-specific structs into generic structs.

Example direction:

```rust
pub struct IchSpiController<H: HostAccess> {
    host: H,
    spibar: H::MmioRegion,
    generation: IchChipset,
    lpc_bdf: Bdf,
    // existing controller state
}
```

```rust
pub struct Spi100Controller<H: HostAccess> {
    host: H,
    spibar: H::MmioRegion,
    memory: Option<H::MmioRegion>,
    mapped_len: usize,
    // existing controller state
}
```

Do not put the controller implementation itself behind `std` or `target_os = "linux"`. Only Linux host construction should be Linux-gated.

## Step 4: Separate chipset detection from PCI enumeration

The current detection scans Linux sysfs. For embedded use, detection should accept an iterator/slice of PCI devices supplied by the host.

Add pure functions like:

```rust
pub fn find_intel_chipset_in_devices(devices: &[PciDevice]) -> Result<Option<DetectedChipset>>;
pub fn find_amd_chipset_in_devices(devices: &[PciDevice]) -> Result<Option<DetectedAmdChipset>>;
```

Linux can populate `Vec<PciDevice>` from sysfs.

CrabEFI can populate a fixed-capacity list from its PCI scanner.

For no-alloc mode, also provide iterator-based variants:

```rust
pub fn find_intel_chipset<I>(devices: I) -> Result<Option<DetectedChipset>>
where
    I: IntoIterator<Item = PciDevice>;
```

## Step 5: Remove direct `std` references from shared controller code

Replace:

- `std::fmt` with `core::fmt`
- `std::hint::spin_loop` with `core::hint::spin_loop`
- `std::thread::sleep` with `host.delay_us()` or `host.delay_ms()`
- `Box<dyn Controller>` with enum or generic forms for embedded paths

Userspace can still use dynamic dispatch in the CLI layer if desired.

## Step 6: Provide an embedded-friendly internal flash enum

Avoid requiring heap allocation or trait objects for CrabEFI.

Add an enum similar to CrabEFI's current `AnySpiController`:

```rust
pub enum InternalFlashController<H: HostAccess> {
    Intel(IchSpiController<H>),
    Amd(Spi100Controller<H>),
}
```

Implement a small common trait for it:

```rust
pub trait InternalFlash {
    fn name(&self) -> &'static str;
    fn size(&self) -> u32;
    fn bios_region(&self) -> Option<(u32, u32)>;
    fn is_locked(&self) -> bool;
    fn writes_enabled(&self) -> bool;
    fn enable_writes(&mut self) -> Result<()>;
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()>;
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<()>;
    fn erase(&mut self, addr: u32, len: u32) -> Result<()>;
}
```

CrabEFI's `SpiStorageBackend` can then wrap `InternalFlashController<CrabEfiHost>`.

## Step 7: Keep userspace CLI API stable

Keep `InternalProgrammer::new()` for the Linux CLI, but implement it using the new shared pieces:

```rust
impl InternalProgrammer {
    pub fn new() -> Result<Self> {
        let host = LinuxHost::new()?;
        let devices = host.scan_pci_bus()?;
        let controller = InternalFlashController::detect(host, devices)?;
        Ok(Self { controller })
    }
}
```

The CLI can continue implementing `SpiMaster`, `OpaqueMaster`, or `FlashDevice` on its userspace wrapper.

## Step 8: Add CrabEFI adapter outside rflasher first

Initially, avoid making rflasher depend on CrabEFI.

In CrabEFI, create an adapter module that implements rflasher's host traits for CrabEFI types:

- `CrabEfiHost`
- `CrabEfiMmioRegion`
- `PciConfigAccess` backed by `crate::drivers::pci::access`
- delays backed by `crate::time::delay_us`

This keeps dependencies one-way: CrabEFI depends on rflasher, not vice versa.

## Step 9: Add no_std CI checks

Add CI jobs in rflasher for:

```bash
cargo check -p rflasher-core --no-default-features --target x86_64-unknown-none
cargo check -p rflasher-core --no-default-features --features is_sync --target x86_64-unknown-none
cargo check -p rflasher-internal --no-default-features --features embedded-host --target x86_64-unknown-none
```

Also add a normal Linux userspace check:

```bash
cargo check -p rflasher-internal --features linux-host
cargo check --workspace
```

## Step 10: Add tests with fake host access

Create a fake host backend for unit tests:

- fake PCI config space
- fake MMIO register arrays
- deterministic delay no-op

Use it to test:

- Intel SPI BAR discovery
- AMD SPI BAR discovery
- FRAP/FREG parsing
- protected range handling
- BIOS region calculation
- command sequencing register writes

This allows most controller logic to be tested without root or hardware.

## Step 11: Make flash operations firmware-friendly

Do not require whole-flash `Vec` buffers for CrabEFI capsule updates.

Add APIs that operate on caller-provided scratch buffers:

```rust
pub fn smart_write_region_with_scratch<D: FlashDevice>(
    device: &mut D,
    addr: u32,
    desired: &[u8],
    scratch: &mut [u8],
) -> Result<WriteStats>;
```

Guidelines:

- Scratch buffer size should be one erase block or caller-selected chunk.
- Never require buffering a full 16 MiB/32 MiB/64 MiB flash image in firmware.
- Keep the existing allocation-backed userspace convenience APIs.

## Step 12: Integration path in CrabEFI

Once rflasher has embedded-host support:

1. Add `rflasher-core` and `rflasher-internal` dependencies to CrabEFI with `default-features = false`.
2. Implement `CrabEfiHost` in CrabEFI.
3. Replace or wrap `src/drivers/spi/{intel.rs,amd.rs}` with rflasher controllers.
4. Keep CrabEFI's `SpiStorageBackend` API stable initially.
5. Verify:
   - variable store load/write/delete
   - capsule region write path
   - QEMU pflash fallback remains separate or gets its own rflasher host/device adapter
6. Remove old duplicate CrabEFI SPI code only after parity is confirmed on Intel and AMD hardware.

## Migration strategy

Do this incrementally:

1. Port rflasher's chipset table and Intel permission checks into CrabEFI as a stopgap.
2. Refactor rflasher host access traits.
3. Make Linux use the new abstraction with no behavior change.
4. Add fake-host tests.
5. Add CrabEFI adapter and compile-only integration.
6. Test variable store and capsule writes on hardware.
7. Delete duplicated CrabEFI Intel/AMD controller logic.

## Acceptance criteria

- `rflasher` CLI still works on Linux with the internal programmer.
- `rflasher-internal` builds for `x86_64-unknown-none` with `embedded-host`.
- Shared Intel/AMD controller logic has no direct dependency on Linux sysfs, `/dev/mem`, libc, or `std`.
- CrabEFI can construct an internal flash controller without heap allocation.
- CrabEFI can read, erase, and write the SMMSTORE region through rflasher-backed code.
- Capsule writes can operate region-by-region without allocating a full flash-size buffer.
- Intel access permissions and protected ranges are handled at least as well as current rflasher userspace code.

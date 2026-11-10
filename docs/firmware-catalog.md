# Board-selected flash chips for firmware

The firmware path is:

**board strings → static catalog → identity/default resolution → core SPI operations → controller**

RON parsing and name matching run on the host. The firmware contains only the
selected descriptors. Identification, protection handling, geometry validation,
programming and erase/status polling are shared with the native SPI core—not
reimplemented in each board driver.

## Dependencies and build script

Use the same rflasher revision for every dependency. The target only needs:

```toml
[dependencies]
rflasher-core = { version = "0.1", default-features = false }
rflasher-internal = { version = "0.1", default-features = false }

[build-dependencies]
rflasher-chips = { version = "0.1", default-features = false }
rflasher-chips-codegen = "0.1"
```

In `build.rs`:

```rust
use std::{env, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let data = rflasher_chips::bundled_data_dir();
    println!("cargo::rerun-if-changed={}", data.display());
    let db = rflasher_chips_codegen::ChipDatabase::load_from_dir(data)?;
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("flash_chips.rs");
    db.select(&["W25Q128FVJV-WP", "MX25L6405D", "SST25VF016B"])?
        .with_defaults(&["W25Q128FVJV-WP"])?
        .with_crate_path("rflasher_core::chip")?
        .write(&output)?;
    Ok(())
}
```

Names ignore ASCII case. Exact canonical names take precedence over catalog
aliases. An explicit profile must be named exactly, not through an alias
pattern. Unknown or ambiguous selections fail the build; ambiguity diagnostics
list exact `Vendor::CanonicalEntryName` choices, including same-vendor matches.
Repeated entries are deduplicated while preserving input order.

`with_crate_path` supports core's re-exports or renamed dependencies. Its default
is `rflasher_chip_types`. Generated imports stay in a private module, avoiding
collisions with the consumer's own imports. A custom RON directory can replace
the bundled data.

## Identification and defaults

The output exports `CHIPS` and `CATALOG`. Use **`CATALOG.resolve`** for an
identity you already read, or **`probe_catalog`** to read and resolve it.

The build script reduces the selection to one operation profile per chip
identity, so the firmware only looks an identity up. Whatever the selection
leaves undecided fails the build instead of surfacing as an ambiguous chip in
the field:

- entries that share an identity and operate alike are aliases; one is kept;
- entries that share an identity but operate differently need a default, which
  becomes that identity's profile. The others are not shipped;
- conflicting defaults for one identity are an error;
- an explicit profile must be a default. Selecting it is not approval;
- identities read through different protocols (standard RDID, audited banked
  identities, RES2) never compete.

Every default must already be selected. An empty selection or default list is
valid. Defaults assert board knowledge (for example a verified flash marking
when several models share RDID); RDID alone cannot establish those differences.
A default is never a fallback: an identity outside the catalog stays unknown.

A hand-written `ChipCatalog::new(&chips)` follows the same rule at run time:
all its entries are approved, and an identity whose entries disagree is
refused as ambiguous rather than answered by list order.

## Use existing core behaviour

```rust
mod flash_chips {
    include!(concat!(env!("OUT_DIR"), "/flash_chips.rs"));
}

use rflasher_core::flash::{probe_catalog, SpiFlashDevice};
use rflasher_core::error::Result;
use rflasher_internal::{HostAccess, IchSpiController};

async fn open_flash<H: HostAccess>(
    mut controller: IchSpiController<H>,
) -> Result<SpiFlashDevice<IchSpiController<H>>> {
    let context = probe_catalog(&mut controller, &flash_chips::CATALOG).await?;
    Ok(SpiFlashDevice::new(controller, context))
}
```

The concrete Intel controller implements the existing `SpiMaster`; no `Box` or
firmware-specific protocol trait is needed. Use `FlashDevice::{read, write,
erase}` on the result. Core chooses supported erasers from the chip's physical
map, preflights the complete footprint, polls completion, and handles/restores
chip protection. Unsupported AAI falls back to the documented byte-program
path. Firmware need not supply raw `(opcode, block_size)` pairs.

Identification also tells the controller which chip it drives. Intel software
sequencing only sends opcodes it understands, and some depend on the chip:
0x35 reads status register 2 on Winbond parts, whose unprotect procedure needs
it, but enters QPI on others. An unlocked opcode menu gains the identified
chip's status reads; a locked one is used as the firmware left it.

`SpiFlashDevice::check_writable` reports, without sending a command, whether
the controller can program and erase the identified chip at all. Use it when
opening the device to decide whether to offer a writable store, rather than
finding out from the first failing write.

`probe_catalog` is available without `alloc` or `std`. It reads standard RDID
and audited bank-one PMC/AMIC identities, with no SFDP synthesis or fallback to
legacy identities. A caller explicitly using a legacy protocol can pass its
observed `ChipIdentity::Res2` to `CATALOG.resolve_identity` instead. The richer
native SFDP/probing API remains separate.

For fstart, keep the existing RAM-execution/XIP requirements and linked mutable
`FlashExtent` checks: core knows chip capacity and controller restrictions, not
the board's reserved firmware-data extents. Async operations must be driven to
completion, including error restoration. Synchronous firmware can drive these
MMIO futures with a no-op-waker poll loop at its service boundary; the Intel
controller does not suspend. Do not cancel an in-flight mutation.

Chipset configuration (including owned BIOS write-enable changes) is restored
when the controller is dropped. Scope that controller/session inside the
RAM-only operation, or retain fstart's existing per-call BIOSWE guard if keeping
a controller alive between calls. Restoring a chip's BP bits is not the same as
restoring chipset BIOSWE. Do not relax fstart's lock/SMM-only refusal policy.

## One model, with or without allocation

`FlashChip`, `EraseBlock` and `FlashContext` no longer switch between different
owned and static APIs. `ChipName::borrowed` and `ChipSlice::borrowed` are const
constructors in every feature configuration. Generated maps are complete,
compact static slices—not capacity-eight inline vectors.

An eraser's single region of equal blocks is held inline. `EraseBlock::new`
and `with_count` are therefore const and need neither a heap nor a static
table, so geometry found at run time (a controller's block size, for one)
can be described without `alloc`. Only maps of several regions borrow a
table or own their storage.

With `alloc`, storage uses standard `Cow`: runtime strings/vectors convert via
`.into()` or `.collect()`, and `to_mut()` copies borrowed maps before modifying
them. Cloning a generated descriptor or performing SPI operations with it does
not allocate even when `alloc` is enabled. Both full and selected compiled
databases use this same representation; there is no lazy per-chip allocation.
Runtime RON loading continues to own its data, with the same serialized format.

## Regression fixture

CI checks the real generic controller/core path on `x86_64-unknown-none`, with
and without `alloc`, and runs the same host tests in both configurations:

```sh
cargo check --manifest-path crates/rflasher-chips-codegen/tests/firmware/Cargo.toml --target x86_64-unknown-none
cargo check --manifest-path crates/rflasher-chips-codegen/tests/firmware/Cargo.toml --target x86_64-unknown-none --features alloc
cargo test --manifest-path crates/rflasher-chips-codegen/tests/firmware/Cargo.toml
cargo test --manifest-path crates/rflasher-chips-codegen/tests/firmware/Cargo.toml --features alloc
```

The fixture checks identity/default policy, unknown and conflicting identities,
non-uniform/native-four-byte maps, protection restoration on success/failure,
busy polling/timeouts, geometry/access rejection before mutation, and a complete
probe/erase/program/read cycle with an allocation-counting allocator.

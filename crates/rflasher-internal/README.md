# rflasher-internal

Internal chipset SPI controller support: Intel ICH7 through 500 Series PCH
and AMD FCH (SPI100). Used by the rflasher CLI's `internal` programmer on
Linux (via PCI sysfs and `/dev/mem`, usually requiring root), and reusable in
`no_std` firmware such as fstart and CrabEFI.

## Firmware reuse

For a `no_std` integration, depend on the crates without default features:

```toml
rflasher-core = { version = "0.1", default-features = false }
rflasher-internal = { version = "0.1", default-features = false }
```

The crate is structured so firmware does not duplicate controller logic.
Embedded callers provide the platform-specific access layer by implementing
`rflasher_internal::HostAccess`:

- `PciConfigAccess` methods for PCI configuration reads/writes,
- `map_mmio` for controller register and optional flash memory windows,
- `delay_us` for short controller polling delays.

The core `SpiMaster`/`FlashDevice` operations are `async fn`; platform
`HostAccess` remains synchronous. Firmware without an executor can drive the
SPI operations to completion with its own minimal `block_on` — a poll loop with a
no-op waker is enough, since the controllers never actually suspend. (`std`
executors like `futures_lite::future::block_on` are not available in `no_std`
builds; embedded executors such as Embassy also work.)

Firmware can pass its own PCI scan results to
`find_intel_chipset_in_devices` / `find_amd_chipset_in_devices`, then
construct controllers with `IchSpiController::new_with_host(...)` or
`AmdSpi100Info::create_controller_with_host(...)`.

Both concrete generic controllers implement `SpiMaster`, without `Box` or
`std`. For a [board-selected catalog](../../docs/firmware-catalog.md), use
`flash::probe_catalog` and wrap the controller and resulting `FlashContext`
in `SpiFlashDevice`. Core then owns chip geometry, protection and polling;
firmware should not reissue raw erase/status/unlock sequences itself.

An Intel controller in hardware sequencing sends no raw SPI commands, so it
needs no chip at all: wrap it in `HwseqFlash::with_size(controller, size)`,
which is the same `FlashDevice` with the controller's erase block size. That
is the path for flash beyond the 16 MiB software sequencing can address.

For native probing, the same adapter uses the richer SFDP-aware context.
AMD SPI100 reads use the checked memory window where the chip's size and addressing allow it, and native SPI commands otherwise;
writes, erases and status access use the chip-aware SPI path.

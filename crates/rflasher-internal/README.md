# rflasher-internal

Internal chipset SPI controller support: Intel ICH7 through 500 Series PCH
and AMD FCH (SPI100). Used by the rflasher CLI's `internal` programmer on
Linux (via PCI sysfs and `/dev/mem`, usually requiring root), and reusable in
`no_std` firmware such as CrabEFI.

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

All I/O trait methods are `async fn`. A firmware without an executor can
drive them to completion with its own minimal `block_on` — a poll loop with a
no-op waker is enough, since the controllers never actually suspend. (`std`
executors like `futures_lite::future::block_on` are not available in `no_std`
builds; embedded executors such as Embassy also work.)

Firmware can pass its own PCI scan results to
`find_intel_chipset_in_devices` / `find_amd_chipset_in_devices`, then
construct controllers with `IchSpiController::new_with_host(...)` or
`AmdSpi100Info::create_controller_with_host(...)`.

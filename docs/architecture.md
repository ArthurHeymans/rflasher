# Architecture

rflasher is a Cargo workspace with a strict dependency direction: the chip
data model and flash protocol live in `no_std` core crates, programmer
backends sit behind cargo features, and frontends (CLI, WASM, REPL) depend on
those layers without the core knowing about them.

| Crate | Purpose |
|---|---|
| `rflasher-chip-types` | Shared `no_std` SPI NOR chip data model and provider trait |
| `rflasher-core` | `no_std` SPI protocol, probing, and flash operations (async, runtime-neutral) |
| `rflasher-chips` | Runtime RON loading and optional compiled chip database provider, with chip type re-exports |
| `rflasher-chips-codegen` | Build-time code generator for the compiled chip database |
| `rflasher-programmers` | Feature-gated external programmer backends plus the native high-level registry and `FlashHandle` |
| `rflasher-internal` | Internal chipset SPI controller support, kept separate so firmware can use it with `default-features = false` and no `std` |
| `rflasher-pci` | Small `no_std` PCI configuration-space abstraction used by the internal programmer |
| `rflasher-repl` | Steel Scheme scripting support for native applications |
| `rflasher-wasm` | Browser-based web interface using egui, WebSerial, and WebUSB |

## Async, executor-independent core

All operational APIs (`SpiMaster`, `OpaqueMaster`, `FlashDevice`, probing,
flash operations) are async on every target, and the core requires no runtime:

- **Native CLI** blocks exactly once, in `main`, with
  `futures_lite::future::block_on` around the async command handlers.
  Genuinely blocking backends (Linux spidev/MTD/GPIO, serial serprog) perform
  their blocking calls inside async methods.
- **WASM**: the browser event loop drives the same async operations over
  WebUSB/WebSerial.

Because `async fn` traits are not object-safe, runtime programmer selection
goes through object-erasure adapters (`ErasedFlashDevice`,
`ErasedSpiMaster`) in `rflasher-programmers`. The one boxed future per
operation is negligible next to USB and flash latency.

## Firmware reuse of the internal programmer

The Intel ICH/PCH and AMD SPI100 controller code in `rflasher-internal` is
structured for `no_std` firmware reuse. Embedded callers provide the
platform-specific access layer by implementing `rflasher_internal::HostAccess`:

- `PciConfigAccess` methods for PCI configuration reads/writes,
- `map_mmio` for controller register and optional flash memory windows,
- `delay_us` for short controller polling delays.

See [crates/rflasher-internal/README.md](../crates/rflasher-internal/README.md)
for details.

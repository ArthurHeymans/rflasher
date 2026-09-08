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

## Prepare / read-op pipeline (multi-IO sessions)

Multi-IO reads are a per-session negotiation, modeled on flashprog's
`spi_prepare_io` / `spi_finish_io` (`spi25_prepare.c`). The flow is:

1. **Probe** yields a `FlashContext` (chip + address mode).
2. **`prepare_io`** (`rflasher-core/src/flash/prepare.rs`) assembles a
   `PreparedState` and establishes it on the chip:
   - *Quad Enable*: set **volatile** (EWSR-prefixed write, lost on power
     cycle) and only on chips advertising volatile status-register writes
     (`WRSR_EWSR`); the bit is re-read to confirm, otherwise quad is
     disabled for the session. Chips with no QE register keep their flags
     (factory-set assumption), matching flashprog.
   - *QPI entry* is deliberately skipped: QPI rewires every command to
     4-4-4 framing and the non-read paths are still single-IO.
   - *Read-op selection* (`select_read_op`, mirroring
     `select_multi_io_fast_read` with 4BA-native preference) picks the
     fastest op both chip and programmer support; if nothing is selectable
     it degrades to single-I/O 0x03. Only genuinely unaddressable
     combinations (4-byte addressing required but unsupported) fail the
     open — everything else degrades, never corrupts.
3. **Devices** carry the `PreparedState`: `SpiFlashDevice` issues the cached
   op through `SpiMaster::execute`; `HybridFlashDevice` (Dediprog, sunxi
   FEL) pushes it to `OpaqueMaster::set_read_op` for the bulk path while
   erase/status stay on `SpiMaster`. If `prepare()` was never called, the
   first bulk read/write/erase runs it lazily, so the opaque path never
   sees a stale read op or 3-byte address on a >16 MiB chip.
4. **Erase verification** (`check_erased_range`) always reads back through
   the single-IO slow path (`operations::read`), mirroring flashprog's
   `dediprog_slow_read` pinning `spi_fast_read` to NULL: the verify path
   must work where no QE is established and where the generic command path
   is single-IO-only.
5. **`finish_io`** undoes session side-effects: exit QPI if entered, and
   clear a volatile QE bit *this session set* (pre-existing sticky QE is
   left untouched). Compatibility 4BA mode is left active, like flashprog.
   Teardown is wired in, not just available: the CLI runs `finish()` after
   every flash command (including error paths), the WASM app runs it after
   every op (devices are discarded per-op there), and `prepare_io` itself
   restores QE if address setup fails after the bit was set — mirroring
   flashprog's `finish_access` coverage.

The chip database (`crates/rflasher-chips/data/vendors/*.ron`) is derived
from flashprog's `flashchips.c`. Multi-IO capability is stored as
fine-grained per-JEDEC-mode flags plus `qe_method` and `wrsr_ewsr`; entries
the reference does not know are left alone.

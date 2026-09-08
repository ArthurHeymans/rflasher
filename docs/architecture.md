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

## Operation-scoped SPI I/O

`flash/io.rs` selects small immutable `ReadPlan`, `WritePlan`, and `ErasePlan`
values using chip capabilities and pure backend representability checks. Program
addressing is independent of read addressing. Native four-byte opcodes,
compatibility EN4B, and EAR are explicit alternatives; residual commands share
the selected address strategy. Selection rejects unrepresentable dummy timing
and unsupported/unknown QE sequences without speculative register writes. Per-chip
`dummy_cycles_*` overrides and `DummyCycleOverrides` use `Option<u8>`: `None`
selects the JEDEC default, while `Some(0)` means exactly zero clocks. RON may
omit these fields or supply `Some(n)`; SFDP stores supported descriptor totals
as `Some(mode + wait)`. Resolved command timing remains a plain `u8`. QPI
entry remains disabled.

`SpiFlashDevice` and `HybridFlashDevice` retain only a binary recovery-required
latch. One shared async bracket owns temporary addressing and (reads only) QE
around the **entire requested operation**, including every bulk chunk and
unaligned head/tail. Undo obligations are recorded before mutation submission.
Normal errors still reach cleanup: wait for idle, disable writes, restore owned
QE, restore EAR or exit owned EN4B. Independent cleanup is attempted even when
one restoration fails. Pre-existing QE and unrelated status bits are preserved.
A cleanup failure takes precedence over the operation error (both are logged).
Uncertain firmware failures bypass all ordinary SPI cleanup, including RDSR:
without a firmware quiescence barrier, even an idle flash may receive further
firmware commands. The latch stays armed and explicit recovery owns restoration.
Persistent WP uses the same lifecycle but never enters a read-QE scope. Modern
persistent status writes use WREN; mandatory legacy EWSR remains distinct from
optional volatile writes.

Hybrid paths use explicit `OpaqueMaster::*_planned` calls. Defaults report no
support and fail before I/O, never silently discard a plan. Dediprog retains
aligned firmware READ/bulk IN and WRITE/padded bulk OUT, with explicitly planned
single-I/O residuals and bank-boundary splitting. V2 uses native four-byte packet
framing, not an inferred address width; compatibility-four-byte program is V3
only. V2 bulk `0x13` is not silently substituted with `0x0c`: selection must
explicitly authorize a native fast-read command. When no bulk read plan is
representable, Hybrid selects a separate single-I/O SPI plan before setup;
failed bulk reads are never retried through SPI. Writes require backend plan
support, including V2's native-PP/EAR restriction. Sunxi retains firmware bulk
read, batched program, and erase with on-SoC busy polling. Firmware erase versus
SPI fallback is chosen **before** hardware I/O; a firmware failure never triggers
a SPI retry. Erase completes and cleans up before a fresh single-I/O verification
scope. Raw opaque APIs remain available on genuinely opaque programmers; the
SPI-backed Dediprog/Sunxi paths require chip plans instead of guessed metadata.

### Baseline and cancellation

Adapter construction and probing assume the caller has established an idle,
ordinary-SPI, three-byte command baseline. JEDEC identification does **not**
establish that baseline; there is no universally safe generic chip reset.
EAR is read and restored for operations that use it. Callers must ensure bank
zero for ordinary three-byte operations. Connection to a previously used flash
must not be mistaken for a reset.

The adapter's latch is armed before the first hardware await. Dropping a polled
future performs no async cleanup and leaves the latch armed. Every adapter I/O
and WP entry point rejects subsequent access until explicit **hardware recovery
and reprobe**; there is no transparent latch-reset/reprepare method. Opaque
firmware failures also require recovery because a status read alone cannot prove
the firmware command engine has stopped. `master()` and `into_parts()` are
low-level escapes whose callers inherit this responsibility. The low-level free
SPI functions likewise leave cancellation recovery to their caller.

The CLI and registry need no preparation or temporary-state teardown plumbing.
The browser discards an adapter's master after failed cleanup rather than
reconstructing a healthy adapter on uncertain hardware. Disconnect/reconnect
alone is **not** a promise of chip recovery. Unrelated programmer resource
shutdown remains backend-owned.

Chip capabilities and QE metadata live in `rflasher-chip-types` and the vendor
RON database, with conservative SFDP conversion. Erased SPI dispatch forwards
exact dummy-clock representability as well as opcode and I/O-mode capabilities.

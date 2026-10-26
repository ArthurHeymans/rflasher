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

All operational APIs (`SpiMaster`, `FlashDevice`, probing,
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

## SPI and opaque access

`FlashDevice` is the address-level interface used by every high-level
operation. Every programmer provides it in one of two ways:

- `SpiMaster` executes SPI transactions; the core owns chip protocols.
  `SpiFlashDevice` combines a `SpiMaster` with the probed `FlashContext`.
  USB bulk transport or a memory-mapped window does not make a programmer
  opaque.
- Programmers without raw SPI access run the protocol themselves and
  implement `FlashDevice` directly, including their own size and erase
  geometry (`HwseqFlash` for Intel hardware sequencing, `LinuxMtd`).

Programmers with a faster route to the flash override the provided
`SpiMaster::read_flash` and `SpiMaster::program_flash` methods. Their
defaults are the generic protocol (`flash::spi_read`, `flash::spi_program`),
and an override receives the chip context on every call, so it decides per
chip and per range, and falls back by calling the generic function. No
configuration step is needed after probing, and erasure through
`ErasedSpiMaster` keeps the overrides.

The core keeps everything around the fast path: bounds checks, the
preflight that the generic protocol could perform the write, temporary
unprotection and its restoration, erase, status registers and write
protection. A fast path may only be faster than the generic protocol, never
more capable.

| Backend | Access |
|---|---|
| AMD SPI100 | SPI; reads through the checked memory window where the chip allows it |
| Dediprog | SPI; CMD_READ/CMD_WRITE bulk transfers for aligned blocks below 16 MiB |
| sunxi FEL | SPI; payload bulk reads and page programs for compatible chips |
| Intel software sequencing | SPI |
| Intel hardware sequencing, Linux MTD | Opaque |
| CH341A/CH347, FTDI/FT4222, Raiden, serprog, spidev/GPIO, ATI SPI, dummy | SPI |

Fallback is selected from known capabilities, never by retrying a failed fast
transfer as a generic one.

## Firmware reuse of the internal programmer

The Intel ICH/PCH and AMD SPI100 controller code in `rflasher-internal` is
structured for `no_std` firmware reuse. Embedded callers provide the
platform-specific access layer by implementing `rflasher_internal::HostAccess`:

- `PciConfigAccess` methods for PCI configuration reads/writes,
- `map_mmio` for controller register and optional flash memory windows,
- `delay_us` for short controller polling delays.

See [crates/rflasher-internal/README.md](../crates/rflasher-internal/README.md)
for details.

## Programmer option catalog

Every programmer's connection options are described once, in
`rflasher-programmers::catalog`, and consumed by both frontends:

- Each backend keeps its typed config and `parse_options` as the semantic
  source of truth. It also declares an `OptionSpec` table next to the parser
  (in a backend-local `schema` module) describing each option's key, label,
  help, value domain, default, and platform scope, plus a `validate` entry
  point that runs that same parser. `catalog` only aggregates the backend
  schemas for listing and validation, so the table and the parser it describes
  are read and reviewed together.
- The CLI renders `-p <name>:key=value,...`, and `list-programmers` prints the
  option syntax generated from the catalog rather than hand-written prose.
- The web frontend renders widgets from the schema and hands the same raw
  `(key, value)` pairs to `validate`, so both surfaces validate the same
  web-visible options. Native-only selection uses the browser device picker.

The catalog is compiled on every `std` target, including `wasm`. Programmers
are listed by compile-time feature; individual options that only make sense on
native targets (device paths, serial numbers) are marked `Scope::NativeOnly`
and are hidden by the web frontend, which uses the browser device picker
instead.

Schema and parsers are kept in sync by tests in `catalog` that feed every
declared key, every choice value, and every declared default back through the
real parser, and assert that unknown keys are rejected. Parsers therefore error
on unrecognised options instead of logging a warning and silently ignoring
them.

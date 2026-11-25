# rflasher-internal

Internal chipset SPI controller support: Intel ICH7 through 500 Series PCH,
AMD SB600, SB7x0/SP5100 and SB8x0/SB9x0 southbridges (SB600-family
controller; the Hudson to Promontory FCHs are detected but refused as
untested) and AMD FCH (SPI100). Used by the rflasher CLI's `internal` programmer on
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
`AmdSpi100Info::create_controller_with_host(...)`. The SB600 family is
described below.

The concrete generic controllers implement `SpiMaster`, without `Box` or
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
the SB600 family reads through its 8-byte command FIFO (as flashprog does);
writes, erases and status access use the chip-aware SPI path.

## AMD SB600 family

`amd_sb600spi` drives the SPI ROM controller of the SB600, SB7x0/SP5100 and
SB8x0/SB9x0 (matching flashprog's `sb600spi.c`). The Hudson, Bolton, Yangtze
and Promontory FCHs are detected and refused as untested.

Discovery (`enable_amd_sb600_with_host`) is read-only and refuses before
touching SPI registers when:

- the chipset is not an SB600-family chipset, or the LPC bridge (D14F3) and
  SMBus controller (D14F0) do not match a documented revision (SB7xx
  `0x39..=0x3d`, SB8xx/SB9xx `0x40..=0x42`);
- `SPI_BASE_ADDR` is zero or (on SB7xx and later) `SpiRomEnable` is clear;
- the SPI pins are configured as GPIOs (SMBus `0x83`/`0xA7`/`0xAB` on
  SB600/SB7xx, IoMux GPIO162-165 on SB8xx/SB9xx);
- an SB8xx/SB9xx southbridge is not strapped to SPI ROM (checking
  `PM_Reg C8h` `UseBypassRom`, enabled `StrapOverride`, then `StrapStatus`
  through ACPI MMIO at `Sb600BoardConfig::acpi_mmio_base`);
- the IMC is strapped on, reports itself running (`LPC 0x40[7]`), or — on
  SB7xx where the strap is not readable from PCI config space — the board
  or CLI (`sb7xx_imc=off`) has not asserted that it is strapped off.

Controller behavior:

- Refuses to open if `SpiAccessMacRomEn` or `SpiHostAccessRomEn` is clear,
  or if the command engine is already busy.
- Single-I/O, 3-byte addressing (`SpiFeatures::NO_4BA_MODES`), 8-byte FIFO
  ring buffer (max 7 transmit / 8 receive bytes per command, with the
  controller's off-by-one receive count handled when `writecnt == 0`).
- Enforces the controller's hardware restricted-opcode registers
  (`SPI_RestrictedCmd`, `SPI_RestrictedCmd2`, and `SPI_Cntrl1[31:16]`).
- Enforces write-once `RomProtect 0..3` ranges (LPC `0x50..=0x5C`) on both
  high-level `check_access`/read paths and raw `send_command`/`execute`
  calls whenever any slot is active, mapping top-16 MiB host ranges via
  `address & (chip_size - 1)` and treating non-top ranges as full-chip
  protection.
- Configures `NormSpeed` (default 16.5 MHz; 22 and 33 MHz selectable) and
  disables `fastReadEnable` on SB8xx/SB9xx during operation, restoring both
  on `Drop`.
- Poisons the handle after a polling timeout until the engine goes idle.

### Firmware use

```rust,ignore
use rflasher_internal::{amd_pci, Sb600BoardConfig, enable_amd_sb600_with_host};

let enable = amd_pci::find_chipset(0x1002, 0x439d, 0).unwrap(); // SB9x0 LPC
let board = Sb600BoardConfig::default(); // ACPI MMIO at 0xFED8_0000
// SAFETY: `host` reaches this chipset, and its AcpiMMioAddr is the default.
let info = unsafe { enable_amd_sb600_with_host(&host, enable, lpc_bdf, &board)? };
// SAFETY: `info.spibar_addr` was discovered on this host, unmodified.
let controller = unsafe { info.create_controller_with_host(host)? };
let device = SpiFlashDevice::new(controller, ctx); // after probing
```

Firmware that already knows its board can construct
`Sb600SpiController::new_with_host(host, spi_base, config)` directly from
`Sb600SpiConfig::new(Sb600Generation::Sb89xx)` with `imc` and `rom_protect`
populated. On Linux, `open_amd_sb600` discovers and opens in one safe call.

# EM100 / DediBridge hardware smoke sweep

`scripts/em100_smoke.py` tests rflasher against an isolated EM100 emulator through
a Pico running DediBridge. It is **not** a real-chip qualification or a test for
an in-system motherboard flash bus. Every rflasher operation uses **24 MHz,
single I/O**; there are no lower-speed retries.

## Requirements

- EM100 wired only to DediBridge's SPI pins with common ground and checked 3.3 V
  signaling. No competing SPI master or tied power supplies.
- A `../rem100` checkout and a `../dedibridge` checkout alongside rflasher.
- Python 3.11+, Rust and the rem100 build environment (including xz).
- rflasher with the Dediprog backend, and `dedibridgectl` with `recover-spi`.
- Explicit EM100 serial, Pico USB serial and Dediprog EEPROM selector. The last
  two are **different identifiers**. For example, the tested rig's Pico serial
  is `152A3F44E920C459`, while its EEPROM selector is `SF2147417`.

Build the narrow USB helper (not Pico firmware):

```sh
cargo build --locked --manifest-path scripts/em100-smoke-helper/Cargo.toml \
  --target-dir ../rem100/target
```

The helper links the sibling rem100 library and is intentionally outside the
rflasher Cargo workspace. On NixOS, use rem100's development shell if needed.
The helper binary defaults to `../rem100/target/debug/em100-smoke-helper`.

## Run

Inventory only; does not open USB devices:

```sh
python3 scripts/em100_smoke.py \
  --output .local/em100-smoke/run-1 \
  --em100-serial EM138573 --pico-serial 152A3F44E920C459 \
  --programmer-id SF2147417
```

Add `--execute --phase read` for the full-chip read sweep. Add
`--execute --phase all` for reads followed by bounded erase/program tests.
**Both modes overwrite emulator SDRAM**; `all` also issues SPI mutations.
`--chips M25P80,25VF016B,W25Q64FV,W25Q256FV,MX25L51245G` runs a small pilot.
`--rflasher`, `--helper`, `--recover-tool` and `--configs` override tool paths.

The first run takes two matching 64 MiB SDRAM backups. It locks the selected
EM100 against another instance of this harness and checks the connected Pico
identity before commands. Normal completion, exceptions and SIGTERM attempt
explicit SPI recovery if necessary, restore the original SDRAM with exact
verification, and leave emulation stopped. Repeated SIGINT/SIGTERM are deferred
while stopping a child or restoring SDRAM; already-exited process groups are
still reaped without turning cancellation into `ProcessLookupError`. SIGKILL,
unplugging or loss of power
cannot guarantee restoration. No Pico or EM100 firmware is flashed. The last
configured chip profile remains selected; restoring SDRAM is **not** restoring
the initial profile configuration or emulated register state.

To resume, repeat the same command and output directory. Completed outcomes,
including failures, are not rerun or overwritten. Interrupted attempts retain
logs; their next attempt uses a new directory. Binary, script, configuration
archive and device fingerprints must match. Use a new output directory when
changing them. Do not rebuild/replace binaries during a running sweep.

For a background run, redirect stdout/stderr to `runner.log` and retain its PID.
Use SIGTERM, not SIGKILL, to request a stop with restoration.

## Coverage and evidence

- Inventory is keyed by configuration **filename**, not the internal chip name:
  the cached database contains two different configurations with the same name.
- Mapping uses name patterns plus JEDEC ID/size, or a unique ID/size definition.
  ID-only ambiguities are not arbitrarily assigned a mutation profile. Mapping
  provenance and candidate names remain in `inventory.json`.
- A complete profile-supplied SFDP density that disagrees with Dcfg capacity is
  marked `unresolved` before configuration. This can be a bad profile or an
  unsupported stacked-die layout, not necessarily a chip/driver failure. A
  disagreement between detected and declared geometry is also unresolved;
  neither case authorizes an override or mutation.
- Each read loads a per-profile SHAKE256 pattern, verifies it through rem100,
  records automatic probing and explicit selection separately, checks geometry,
  and compares a complete rflasher read byte for byte. Full reads cover the last
  address and both sides of 16 MiB where applicable. No `--force` is used.
- Each mutation requires a passing read and an explicit mapped definition. A
  separately seeded non-erased pattern is preloaded. The harness erases a 64 KiB
  window, and another at 16 MiB on larger parts. Parts without a documented erase
  block no larger than 64 KiB are excluded from this bounded mutation pass.
- Independent EM100 uploads check the entire chip after erase and program,
  including bytes outside the selected windows. Programming writes a 1,027-byte
  region prefix, spanning pages and an odd-byte tail; rflasher verification and
  a final full-chip rflasher read are also required. This is not exhaustive
  programming of all addresses, all erase opcodes or all program procedures.
- Profiles requesting a supply other than 3.3 V are marked `emulator_limited`
  **before configuration**, because rem100 can otherwise switch voltage while
  loading a profile. Profiles above the library's checked 64 MiB SDRAM API limit
  are also marked limited. No hardware-capacity claim is inferred for G2.
- Before loading each profile, the helper overwrites the emulator's 256-byte
  SFDP table with FF. Profiles with SFDP replace it with their own table; legacy
  profiles cannot inherit the preceding chip's SFDP geometry. Merely writing
  zero to register C9 did not invalidate stale SFDP on the tested EM100.
- Chip-level failures are recorded and the sweep continues. Unrecoverable USB,
  identity or preload failures stop the sweep. Interrupted transfers require
  explicit recovery, not an MCU reboot or a lower frequency.

`results.jsonl` is the durable per-profile/per-phase evidence; `results.csv` is a
compact matrix. `status.json` reports progress and becomes `complete` only after
verified restoration. Each attempt clears stale cleanup success and assigns a
new `run_id` shared by `status.json` and `restoration.json`. **Check matching
run IDs and `restored: true` in both files** before considering cleanup
successful. If execution and cleanup both fail, both errors are recorded and
the original execution failure remains primary. Each phase has its
command logs; failures retain images and independent captures. Passing runs
retain comparison hashes and logs rather than large binary images.

Real-chip timing, voltage compatibility, analog margins, endurance, OTP,
irreversible locks and complete protection-register semantics are not qualified.
The present rflasher Dediprog backend does not implement dual/quad transfers.

## Host checks

```sh
python3 -m unittest discover -s scripts -p test_em100_smoke.py -v
cargo clippy --locked --manifest-path scripts/em100-smoke-helper/Cargo.toml \
  --target-dir ../rem100/target -- -D warnings
```

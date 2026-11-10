# SPI identities and erase-path selection

The probing design is informed by flashprog's method-aware bus probing, checked
against upstream `bbe7393f16b7` (fetched from `main` on 2026-11-10).
`libflashprog/chips.c` separates collecting responses from matching definitions;
`spi.c` registers distinct RDID, REMS, RES and AT25F methods.

## A chip has alternative value-carrying identities

Identification methods own their values, and one chip can advertise several:

```ron
(
    name: "SST25VF040B",
    ids: [Rdid(id: 0xBF258D), Rems(id: 0xBF8D)],
    // operational size, erase and programming metadata omitted
)
```

`ChipIdentity` is an enum, not a method flag paired with a separately interpreted
number. REMS, RES2 and AT25F carry a full `u16` manufacturer/device pair. RES1
carries a `u8`. RDID and RES3 carry complete wire-order values, including a
continuation prefix where present: e.g. `Rdid(id: 0x7F372020)`.

A response is matched by exact enum value; equal numeric values in different
methods are different identities. A repeated trailing byte is not part of a
shorter ID. `Rems(id: 0xBF48)` is never inferred by truncating RDID `BF 48 BF`.

Explicit `ids` replace the legacy identity fields for lookup. The old
`device_id` and `at25f_id`/`res2_id`/`rdid_continuation` metadata remains a
compatibility fallback only when `ids` is empty. With explicit IDs, `device_id`
may be omitted; canonical manufacturer/device metadata for operational APIs is
derived from an RDID alternative if present, otherwise the first identity.
Runtime loading and compiled catalogs use the same `ChipSlice<ChipIdentity>`
model, borrowing static IDs even when allocation is enabled. Selected firmware
catalogs resolve overlaps per exact identity at build time; a default overriding
one alternative does not discard the chip's other IDs or reintroduce ambiguity.
Duplicate identities, invalid framing, overflow and conflicting explicit/legacy
probe declarations fail catalog validation.

`ChipProvider` exposes typed identity lookup with defaults adapting existing
RDID, banked-RDID and RES2 providers. `ProbeResult.identity` and
`ChipInfo.identity` record the response that actually matched, rather than
assuming the first listed alternative was used. The CLI reports that identity
separately from its raw three-byte RDID response.

SST definitions now carry audited REMS/RES values rather than synthetic RDID
values. In particular, true RDID `BF 25 8D` no longer selects the incompatible
non-B byte-program profile. The main SST25VF040B definition accepts RDID and
REMS; the old `.REMS` catalog name remains for compatibility. SST25LF040A and
SST25LF080(A) advertise both documented REMS and RES2 identities.

## Probe policy

- Preserve normal RDID/SFDP selection and geometry checks. Cache the decoded
  RDID identity so explicit-name resolution does not repeat its bus query.
- If RDID has no usable definition and SFDP is definitively absent, collect
  replies from audited legacy methods. Each method runs at most once regardless
  of catalog size. Unsupported opcodes are skipped; transport errors abort.
- Match each response in its own namespace, then intersect its positive catalog
  candidate set with the other matching responses. Multiple methods agreeing on
  one chip corroborate it and can disambiguate aliases; they do not create a
  conflict just because the methods differ. Disjoint positive matches require
  explicit disambiguation and cannot form an invented common profile.
- A recognized RDID chip cannot be replaced by a weaker legacy reply or a
  requested name. Unresolved four-byte AMIC framing is not converted to a
  legacy match. These retain rflasher's stricter safety policy rather than
  copying every flashprog selection behavior.
- One-byte RES is weakest: suppress it when RDID or REMS actually replied, even
  when that response is unknown to the catalog.

No identity authorizes guessed protection-register writes, OTP or irreversible
locks. Definitions still need documented operational procedures.

## Macronix erase paths

The combined `MX66L51235F/MX25L51245G` definition now includes documented B7/E9
mode entry, matching flashprog's `FEATURE_4BA` and the other entry for this ID.
Its real native erase capabilities remain in the catalog.

For addressed erases on a large chip, select a mode-entered legacy opcode when
the chip documents entry and the programmer supports the complete entry, erase
and exit sequence. Otherwise retain native four-byte opcodes, including for
controllers that forbid mode switching or cannot send the legacy opcode.
Preflight and execution share the selector. Transfer/verification failure does
**not** cause a retry using another erase procedure; exit is attempted on
failure as well as success.

This reproduces the useful behavior of flashprog's separate legacy/native
erasers without deleting real silicon capabilities or adding destructive retry.

## Hardware evidence and limits

All tests used the isolated Pico DediBridge / EM100Pro rig at **24 MHz,
single I/O**. No Pico/EM100 firmware or original-sweep binaries were changed.

- Initial iteration (`fix-validation-v2`) passed full reads and bounded
  erase/program for six SST profiles: 25VF512, 25VF512A, 25VF010, 25VF010A,
  25VF020 and 25LF020A. MX25L51245G and W25Q256FV passed too.
- Final multi-ID iteration (`fix-validation-v3`) passed full reads and bounded
  erase/program for 25VF512, 25LF040A, 25VF040B and MX25L51245G.
- After rebasing onto current master and fixing the USB runtime feature,
  `fix-validation-v4` repeated those four profiles with a Dediprog-only build
  (`--no-default-features --features dediprog`). All full reads and bounded
  erase/program comparisons passed. This run used the hardened harness and
  freshly verified cleanup, with matching run IDs in status/restoration records.
- MX25L51245G's complete 64 MiB read passed. Mutation windows at 64 KiB and
  16 MiB passed; complete independent EM100 uploads after erase and program
  matched, including every byte outside the windows. Final full-chip rflasher
  reads also matched. Original SDRAM was restored and verified after each run.
- Multiple alternatives made a real difference for 25LF040A: EM100 returns
  invalid RES2 `44 44`, but valid REMS `BF 44`. The explicit REMS alternative
  now identifies and programs it without forcing or inventing an ID.
- AT25F2048 still fails identification with both rflasher and freshly built
  upstream flashprog. The AT25F method has mock-protocol coverage, not a hardware
  pass on this emulator profile.

Evidence is retained in the original workspace under
`.local/em100-smoke-20261110/fix-validation-v2/`, `fix-validation-v3/`,
`fix-validation-v4/` and `fix-id-diagnostics/`. The first run (`fix-validation-v1`) was invalid: a minimal
Dediprog-only build exposed a pre-existing missing nusb async-runtime feature
and panicked before SPI access. V2/V3 used the normal default CLI build; V4
qualifies the corrected standalone backend. Failed-attempt logs were preserved.

A subsequent raw-SPI investigation bypassed rflasher entirely. Linux USB control
transfers directly requested `DC 00 01 00 00` and `DC 01 00 00 00` at 24 MHz.
EM100's own SPI trace decoded both native erase commands with the correct
addresses. Nevertheless, MX25L51245G's entire SDRAM remained unchanged and WEL
remained set, both with and without prior B7 mode entry. The W25Q256FV emulator
profile showed the same native-erase failure. B7 followed by D8 at those addresses
cleared WEL and erased exactly the requested windows; independent full-SDRAM
readback verified every neighboring byte. Original SDRAM was restored and verified.

This localizes the observed problem to native-erase handling in this EM100 stack,
not rflasher's address serialization or a Macronix-only command path. It does not
identify the responsible EM100 firmware/profile component or qualify real silicon.
Do not remove documented native opcodes or add a mutation retry to compensate.
Raw commands, SPI traces, readbacks and restoration evidence are retained under
`.local/em100-smoke-20261110/pr-investigation-v1/` in the original workspace.
No Pico or EM100 firmware was flashed. All evidence remains at 24 MHz.

These qualify the tested stack against emulation, not real-silicon timing,
analog margins or complete protection behavior. The frozen original sweep and
fixed-binary qualification remain separate datasets.

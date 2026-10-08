# Chip-specific software protection

SPI erase/program operations, including hybrid programmers' bulk writes, use
explicit `Unlock` metadata. The chip catalog records the documented procedure;
it is not inferred from the manufacturer or from an SFDP BP-field guess.
JEDEC aliases with different procedures are not interchangeable.

## Mutation lifecycle

1. Validate bounds, erase geometry and command/addressing capabilities before
   changing protection. High-level mutation policy and recovery-backup preflight
   still run before the first destructive command.
2. Read protection. If absent, do not write status registers. Otherwise save it,
   check available unlock **and restore** commands, remove software register
   locks where allowed, then unprotect and verify the readback.
3. Erase/program. Check documented SR1 erase/program failure flags, and retain
   the existing array readback verification. WIP clearing alone does not prove
   an erase was accepted.
4. Exit AAI programming mode with best-effort WRDI, including after transfer
   failures, before issuing restorative status writes. If an erase may still be
   running, wait with its sector/block/die/chip readiness budget before restoring
   saved protection on success and on command/verification failure.
   Restoration failure is explicitly reported as `ProtectionRestoreFailed`,
   with the original operation result logged. Do not continue flashing until
   protection has been inspected.

Each primitive mutation is responsible for its own restoration, so the free SPI
helpers, `FlashDevice` adapters and type-erased devices all retain this behavior.

Dediprog bulk USB completion is not a flash-worker readiness barrier. In
particular, Dedibridge can still be programming buffered pages or settling when
USB delivery completes, and rejects control SPI access with STALL until the bus
is released. The Dediprog backend retries only an exact one-byte RDSR (`05`)
on a typed control-transfer STALL, polling every 10 ms for at most five seconds.
This lets result checks and restoration wait for ownership before reading flash
WIP; it does not treat USB completion or STALL as proof of a successful mutation.
Other transport errors and short responses are reported immediately. Erase,
program, WREN and status-write commands are never automatically replayed.
Futures must be driven to completion: cancellation, process termination, loss of
power or a disconnected programmer can prevent cleanup. Software protection may
be temporarily removed outside the selected data region, but data mutation
policy and verification still apply to the requested region.

## Covered procedures

Catalog assignments follow flashprog's exact chip identity (manufacturer ID,
device ID and capacity); consistent aliases share the same procedure. Legacy
Atmel F parts also have explicit, name-specific assignments. Their legacy
opcode-`15` identities are marked `AT25F_ID` and excluded from RDID (`9f`)
candidate lookup; legacy probing is not yet implemented. AT25F512B uses RDID
and remains independently detectable. Unmatched and conflicting identities remain `Unknown` rather than receiving a vendor default.

| Procedure | SR1 protection / lock / WP indication / preserved bits |
|---|---|
| Ordinary status BP clear | `3c / 00 / 00 / ff` |
| BP1 + SRWD; AT25F | `0c / 80 / 00 / ff` |
| BP2 + SRWD | `1c / 80 / 00 / ff` |
| BP3 + SRWD | `3c / 80 / 00 / ff` |
| BP4 + SRWD; AT25FS040 | `7c / 80 / 00 / ff` |
| Atmel DF/DL/DQ/AT26 global unprotect | `0c / 80 / 10 / 00` |
| AT25F512A | `04 / 80 / 00 / ff` |
| AT25F512B | `04 / 80 / 10 / ff` |
| AT25FS010 | `6c / 80 / 00 / ff` |
| Micron N25Q/MT25Q | `5c / 80 / 00 / ff` |

These cover catalog entries across Atmel, Micron, SST25, Sanyo, AMIC, Boya,
ESI/ESMT, Eon, GigaDevice, ISSI, Macronix, PMC, Spansion and Winbond, among others.
WREN and persistent/volatile EWSR capabilities come from chip metadata; missing
capabilities do not trigger flashprog's historical guessed-EWSR fallback.
Combined WRSR preserves SR2 where the catalog specifies that protocol.

Complement protection is checked before the BP-zero fast path: CMP=1 with BP=0
can protect the entire array, and clearing BP with CMP=1 can protect a previously
writable region. `WP_CMP_SR2` explicitly identifies CMP at bit 6 of the register
read with `35`; CMP or bit 0 (SRL on these layouts) being set is refused before
any status or array mutation. This check does not enable combined status writes
or a WP range decoder. Catalog assignments use flashprog's per-chip register
maps, not vendor defaults. Known CMP parts without this audited layout (including
Fudan variants with CMP in SR1 or SR2 bit 4, and GigaDevice variants with CMP in
SR3) carry only `WP_CMP` and are refused with `ChipNotSupported`. Aliases with
different CMP checks cannot synthesize a common mutation profile.

**AT25DF321 (`1f:4700`, 4 MiB):** unprotection writes SR1 `00`, including zeroes
in bits 4 and 5. Clearing only bits 2 and 3 is insufficient. SPRL is cleared in a
separate write that leaves sector protection alone. Fully protected chips can
be restored with global protect. Mixed protection is saved through individual
sector-protection registers, sampled every 4 KiB, then restored and checked
before SPRL is restored. The bounded no-allocation snapshot supports the
bundled Atmel chips through 8 MiB. This does not remove security lockdown.

**SST26VF016B(A)/032B(A)/064B(A):** save the 6/10/18-byte BPR, send WREN +
ULBPR `98`, verify write locks cleared, and restore with WBPR `42` and RBPR `72`.
Read-locked parameter blocks and non-removable lockdown are refused. The older
SST26VF016/032 datasheet documents SQI protection commands, not this single-SPI
ULBPR procedure: those definitions are explicitly unsupported for mutation.
SST26VF080A instead uses SR1 BP/BPL and must not receive ULBPR.

AT26DF041 has a read-only status register and needs no status unlock. Intel S33
uses BP2/SRWD with separate SR1 failure flags, not Winbond TB/SEC fields.

## Separate WP configuration support

Unlock masks are not range decoders. `wp` operations require `WP_WINBOND`, an
explicit assertion of the supported SR1/SR2 map, SPI25 range decoder and combined
status-write procedure. Currently the bundled W25Q128.V profile carries this
assertion; other layouts are refused rather than silently treated as Winbond.
CMP/SRL and independent protection modes are not automatically cleared.
Micron sector locks, Atmel security locks and PMC safeguards remain unresolved;
array verification still detects ignored mutations.

## References

- flashprog `flashchips.c` and `spi25_statusreg.c`: per-chip assignments, masks,
  register-lock ordering, enable procedures and 100 ms WRSR settling delay.
- [AT25DF321A datasheet](https://www.renesas.com/en/document/dst/at25df321a-datasheet?language=en),
  sections 9.3–9.7: sector registers and global protect/unprotect.
- [SST26VF016B datasheet](https://ww1.microchip.com/downloads/en/DeviceDoc/20005262D.pdf):
  BPR, RBPR/WBPR and ULBPR.
- [SST26VF080A datasheet](https://ww1.microchip.com/downloads/aemDocuments/documents/MPD/ProductDocuments/DataSheets/20006203B.pdf):
  SR1 BP/BPL and one/two-byte WRSR.
- [Legacy SST26VF016/032 datasheet](https://ww1.microchip.com/downloads/en/DeviceDoc/Serial-Quad-IO-%28SQI%29-Flash-Memory-SST26VF016-SST26VF032-20005017B.pdf).

Stateful emulator tests exercise a 4 MiB AT25DF321-shaped BIOS update through
SPI and hybrid bulk writes, including dynamic dispatch, preservation of data
outside the region, mixed protection, ignored erases, locks, enable methods,
error flags and cleanup failures. They do not replace validation with physical
flash and programmer hardware.

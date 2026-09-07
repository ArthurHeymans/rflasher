#!/usr/bin/env python3
"""
Multi-IO chip database audit script.

Reads flashprog's flashchips.c, derives the expected fine-grained multi-IO
flags (fast_read_dout, fast_read_dio, fast_read_qout, fast_read_qio,
qpi_35_f5, qpi_38_ff, set_read_params, fast_read_qpi4b), qe_method, and
wrsr_ewsr for each entry, then rewrites our chips/vendors/*.ron files to
match.

Unlike the original add-only version, this script synchronises TWO-WAY:
flags the reference does not encode are REMOVED (over-claimed quad/QPI is a
correctness bug: it makes prepare() set QE and issue multi-IO reads the chip
may not support). The only add-only field is wrsr_ewsr (presence-gated
volatile-write capability; never stripped).

QE write-method disambiguation (the old script left STATUS2/bit-1 unmapped,
which silently skipped ~half the database):
  - no .qe in flashchips            -> qe_method removed (no QE -> no quad)
  - STATUS1 bit 6                   -> Sr1Bit6
  - STATUS2 bit 7                   -> Sr2Bit7
  - STATUS2 bit 1 + FEATURE_WRSR2   -> Sr2Bit1WriteSr2 (dedicated 0x31)
  - STATUS2 bit 1 + FEATURE_WRSR_EXT2 (no WRSR2)
                                    -> Sr2Bit1WriteSr (combined 0x01 + 2B)
  - STATUS2 bit 1 + neither bit     -> Sr2Bit1WriteSr2 (fallback; matches the
                                       pre-migration database and Winbond-style
                                       0x31 support. flashprog itself cannot
                                       write SR2 for these entries.)
This mirrors flashprog's spi_write_register(), which prefers the dedicated
WRSR2 command and falls back to the combined WRSR.

Name matching: flashprog entries often cover several chips in one
`.name` ("A/B/C") or carry parenthetical variants ("GD25Q16(B)"). The
reference index is built per individual name; a RON entry matches when it
equals any of them, or equals one stripped of a parenthetical suffix.

Usage:
    ./util/multiio_audit.py          # Dry run (prints summary)
    ./util/multiio_audit.py --apply  # Apply in place

A clean tree must report zero updates; CI enforces this.
"""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FLASHPROG = ROOT.parent / "flashprog" / "flashchips.c"
VENDORS = ROOT / "crates" / "rflasher-chips" / "data" / "vendors"

# ----------------------------------------------------------------------
# Feature-bit bundles from flashprog/include/flash.h
# ----------------------------------------------------------------------

FEAT_BUNDLES = {
    "FEATURE_DIO": {"fast_read", "fast_read_dout", "fast_read_dio"},
    "FEATURE_QIO": {
        "fast_read",
        "fast_read_dout",
        "fast_read_dio",
        "fast_read_qout",
        "fast_read_qio",
    },
    "FEATURE_FAST_READ": {"fast_read"},
    "FEATURE_FAST_READ_DOUT": {"fast_read_dout"},
    "FEATURE_FAST_READ_DIO": {"fast_read_dio"},
    "FEATURE_FAST_READ_QOUT": {"fast_read_qout"},
    "FEATURE_FAST_READ_QIO": {"fast_read_qio"},
    "FEATURE_FAST_READ_QPI4B": {"fast_read_qpi4b"},
    "FEATURE_QPI_35_F5": {"qpi_35_f5"},
    "FEATURE_QPI_38_FF": {"qpi_38_ff"},
    "FEATURE_SET_READ_PARAMS": {"set_read_params"},
    "FEATURE_QPI_35": {
        "fast_read",
        "fast_read_dout",
        "fast_read_dio",
        "fast_read_qout",
        "fast_read_qio",
        "qpi_35_f5",
    },
    "FEATURE_QPI_38": {
        "fast_read",
        "fast_read_dout",
        "fast_read_dio",
        "fast_read_qout",
        "fast_read_qio",
        "qpi_38_ff",
    },
    "FEATURE_QPI_SRP": {
        "fast_read",
        "fast_read_dout",
        "fast_read_dio",
        "fast_read_qout",
        "fast_read_qio",
        "qpi_38_ff",
        "set_read_params",
    },
    # Non-multi-IO bundles — ignored for our purposes.
    "FEATURE_WRSR_WREN": set(),
    "FEATURE_WRSR_EWSR": set(),
    "FEATURE_WRSR_EITHER": set(),
    "FEATURE_WRSR2": set(),
    "FEATURE_WRSR3": set(),
    "FEATURE_WRSR_EXT2": set(),
    "FEATURE_WRSR_EXT3": set(),
    "FEATURE_OTP": set(),
    "FEATURE_4BA_ENTER": set(),
    "FEATURE_4BA_ENTER_WREN": set(),
    "FEATURE_4BA_ENTER_EAR7": set(),
    "FEATURE_4BA_EAR_C5C8": set(),
    "FEATURE_4BA_EAR_1716": set(),
    "FEATURE_4BA_READ": set(),
    "FEATURE_4BA_FAST_READ": set(),
    "FEATURE_4BA_WRITE": set(),
    "FEATURE_4BA": set(),
    "FEATURE_4BA_WREN": set(),
    "FEATURE_4BA_EAR7": set(),
    "FEATURE_4BA_EAR_ANY": set(),
    "FEATURE_4BA_NATIVE": set(),
    "FEATURE_ERASED_ZERO": set(),
    "FEATURE_NO_ERASE": set(),
    "FEATURE_LONG_RESET": set(),
    "FEATURE_SHORT_RESET": set(),
    "FEATURE_EITHER_RESET": set(),
    "FEATURE_ADDR_FULL": set(),
    "FEATURE_ADDR_2AA": set(),
    "FEATURE_ADDR_AAA": set(),
    "FEATURE_ADDR_SHIFTED": set(),
    "FEATURE_ANY_DUAL": {"fast_read_dout", "fast_read_dio"},
    "FEATURE_ANY_QUAD": {"fast_read_qout", "fast_read_qio"},
}

# The nine multi-IO flags this script owns (two-way sync).
MULTI_IO_FLAGS = {
    "fast_read",
    "fast_read_dout",
    "fast_read_dio",
    "fast_read_qout",
    "fast_read_qio",
    "fast_read_qpi4b",
    "qpi_35_f5",
    "qpi_38_ff",
    "set_read_params",
}

WRSR_TOKENS = {
    "FEATURE_WRSR_WREN",
    "FEATURE_WRSR_EWSR",
    "FEATURE_WRSR_EITHER",
    "FEATURE_WRSR2",
    "FEATURE_WRSR3",
    "FEATURE_WRSR_EXT2",
    "FEATURE_WRSR_EXT3",
}

# Our vendor name -> flashprog vendor names (in lookup order).
VENDOR_ALIASES = {
    "Boya": ["Boya/BoHong Microelectronics", "Boya Microelectronics", "Boya"],
    "Eon": ["Eon", "EON"],
    "Micron": ["Micron", "Micron/Numonyx/ST"],
    "XTX": ["XTX Technology", "XTX"],
    "Spansion": ["Spansion", "Cypress"],
    "Zetta": ["Zetta Device", "Zetta"],
    "GigaDevice": ["GigaDevice"],
    "Fudan": ["Fudan"],
    "XMC": ["XMC"],
    "Puya": ["Puya"],
    "ESMT": ["ESMT"],
    "ISSI": ["ISSI"],
    "Atmel": ["Atmel"],
    "AMIC": ["AMIC"],
    "Macronix": ["Macronix"],
    "Winbond": ["Winbond"],
    "SST": ["SST"],
}


def parse_feature_expr(expr: str) -> set[str]:
    """Evaluate a C feature_bits expression into a set of RON flag names."""
    expr = re.sub(r"/\*.*?\*/", "", expr, flags=re.DOTALL)
    expr = re.sub(r"//.*", "", expr)
    expr = " ".join(expr.split())

    added: set[str] = set()
    removed: set[str] = set()

    for part in expr.split("|"):
        part = part.strip("() ")
        sub_parts = [s.strip() for s in part.split("&")]
        if not sub_parts:
            continue
        first = sub_parts[0]
        for tok in re.findall(r"FEATURE_[A-Z0-9_]+", first):
            added |= FEAT_BUNDLES.get(tok, set())
        for sp in sub_parts[1:]:
            if sp.startswith("~"):
                for tok in re.findall(r"FEATURE_[A-Z0-9_]+", sp):
                    removed |= FEAT_BUNDLES.get(tok, set())
    return added - removed


def parse_wrsr_caps(feature_expr: str) -> set[str]:
    """Extract effective WRSR capability tokens, honouring ~ negation."""
    expr = re.sub(r"/\*.*?\*/", "", feature_expr, flags=re.DOTALL)
    # NOTE: WRSR2/WRSR3 carry no underscore after WRSR.
    present = set(re.findall(r"FEATURE_WRSR_?[A-Z0-9]+", expr))
    negated = set(re.findall(r"~\s*(FEATURE_WRSR_?[A-Z0-9]+)", expr))
    return {t for t in present if t in WRSR_TOKENS} - negated


# ----------------------------------------------------------------------
# Load flashprog entries, indexed per individual chip name
# ----------------------------------------------------------------------

CHIP_HEADER_RE = re.compile(r'\.vendor\s*=\s*"([^"]+)",\s*\.name\s*=\s*"([^"]+)",')


def split_names(name: str) -> list[str]:
    """Split a flashprog multi-chip name entry into individual names."""
    return [n.strip() for n in name.split("/") if n.strip()]


def expand_names(name: str) -> list[str]:
    """Expand parenthesised alternations, then split multi-names.

    "MX25U3235(E/F)" -> ["MX25U3235E", "MX25U3235F"];
    "EN25Q32(/A/B)" -> ["EN25Q32", "EN25Q32A", "EN25Q32B"];
    "MX25L1005(C)" -> ["MX25L1005", "MX25L1005C"].
    Applied to both reference and RON names so either side's punctuation
    matches (slash-splitting alone mangles "EN25Q32(/A/B)").
    """
    options = [name]
    while True:
        expanded: list[str] = []
        changed = False
        for candidate in options:
            m = re.search(r"\(([^()]*)\)", candidate)
            if not m:
                expanded.append(candidate)
                continue
            changed = True
            for alt in m.group(1).split("/"):
                expanded.append(candidate[: m.start()] + alt + candidate[m.end():])
        options = expanded
        if not changed:
            break
    out: list[str] = []
    for candidate in options:
        out.extend(n.strip() for n in candidate.split("/") if n.strip())
    return out


def strip_paren(name: str) -> str:
    """Strip a parenthetical variant suffix: 'GD25Q16(B)' -> 'GD25Q16'."""
    return re.sub(r"\([^()]*\)$", "", name)


def expected_qe_method(
    qe: tuple[str, str] | None, wrsr: set[str]
) -> str | None:
    """Derive the RON qe_method from flashprog .qe and WRSR capabilities."""
    if qe is None:
        return None
    reg, bit = qe
    if reg == "STATUS1" and bit == "6":
        return "Sr1Bit6"
    if reg == "STATUS2" and bit == "7":
        return "Sr2Bit7"
    if reg == "STATUS2" and bit == "1":
        # Mirror spi_write_register(): dedicated WRSR2 wins over combined WRSR.
        if "FEATURE_WRSR2" in wrsr:
            return "Sr2Bit1WriteSr2"
        if "FEATURE_WRSR_EXT2" in wrsr:
            return "Sr2Bit1WriteSr"
        # Neither bit (e.g. W25Qxx.V era): flashprog cannot write SR2 at
        # all; keep the historical 0x31 method (Winbond-style 0x31 support).
        return "Sr2Bit1WriteSr2"
    print(f"warning: unmapped QE location {reg} bit {bit}; dropping", file=sys.stderr)
    return None


def load_flashprog() -> dict[tuple[str, str], dict]:
    text = FLASHPROG.read_text()
    headers = [
        (m.group(1), m.group(2), m.start()) for m in CHIP_HEADER_RE.finditer(text)
    ]
    out: dict[tuple[str, str], dict] = {}
    for i, (vendor, name, start) in enumerate(headers):
        end = headers[i + 1][2] if i + 1 < len(headers) else len(text)
        body = text[start:end]
        fm = re.search(
            r"\.feature_bits\s*=\s*(.*?)(?:,\s*\.[a-z_]+\s*=)", body, re.DOTALL
        )
        feat_expr = fm.group(1) if fm else ""
        flags = parse_feature_expr(feat_expr) if fm else set()
        wrsr = parse_wrsr_caps(feat_expr)
        # NOTE on layout: each entry spans [.vendor, next .vendor), and its
        # own reg_bits block (with .qe) sits at the END of that span, after
        # .block_erasers. Searching *backward* from .vendor would grab the
        # previous entry's .qe (they are only ~12 lines apart across the
        # `},` / `{` boundary) — a systematic misattribution. Always search
        # the entry's own forward span.
        qe: tuple[str, str] | None = None
        qm = re.search(
            r"\.qe\s*=\s*\{\s*STATUS(\d)\s*,\s*(\d)(?:\s*,\s*(?:RO|RW))?\s*\}",
            body,
        )
        if qm:
            qe = (f"STATUS{qm.group(1)}", qm.group(2))
        entry = {
            "flags": flags & MULTI_IO_FLAGS,
            "qe_method": expected_qe_method(qe, wrsr),
            "ewsr": bool(wrsr & {"FEATURE_WRSR_EWSR", "FEATURE_WRSR_EITHER"}),
            "ref_name": name,
        }
        # Index under both the literal and the expanded names: the literal
        # keeps exact-match semantics ("GD25Q64(B)"), the expansions bridge
        # punctuation differences on either side.
        for individual in dict.fromkeys([name, *expand_names(name)]):
            out[(vendor, individual)] = entry
    return out


# ----------------------------------------------------------------------
# Update RON files (two-way sync of the owned flags)
# ----------------------------------------------------------------------

FEATURES_LINE_RE = re.compile(r"^(?P<lead>\s*)features:\s*\((?P<inner>.*)\),\s*$")
QE_LINE_RE = re.compile(r"^(?P<lead>\s*)qe_method:\s*(?P<val>\w+),\s*$")
# Lines that terminate the qe_method search window after a features line.
ENTRY_FIELD_RE = re.compile(
    r"^\s*(erase_blocks|tested|voltage|total_size|page_size|device_id|name)\s*:"
)


def parse_inner(inner: str) -> list[tuple[str, bool]]:
    return [
        (pair.group(1), pair.group(2) == "true")
        for pair in re.finditer(r"(\w+)\s*:\s*(true|false)", inner)
    ]


def build_inner(pairs: list[tuple[str, bool]]) -> str:
    return ", ".join(f"{k}: {'true' if v else 'false'}" for k, v in pairs if v)


def lookup_entry(flashprog_map: dict, our_vendor: str, name: str):
    """Look up a chip entry, trying aliased vendors and variant spellings.

    Both sides may use slash-separated multi-names ("A/B") and
    parenthetical variants ("GD25Q16(B)", "MX25L1005(C)"); every part is
    tried. Returns (entry, refkey) on unanimous match, (None, None) when
    nothing matches, and raises _Conflict when parts disagree.
    """
    candidates = VENDOR_ALIASES.get(our_vendor, [our_vendor])
    hits: dict[tuple[str, str], dict] = {}
    parts = expand_names(name)
    if (our_vendor, name) in NAME_ALIASES:
        fv, fn = NAME_ALIASES[(our_vendor, name)]
        entry = flashprog_map.get((fv, fn))
        if entry is not None:
            return entry, (fv, fn)
        print(f"warning: alias {(our_vendor, name)} -> {(fv, fn)} missed", file=sys.stderr)
    for part in parts:
        for variant in (part, strip_paren(part)):
            found = False
            for v in candidates:
                entry = flashprog_map.get((v, variant))
                if entry is not None:
                    hits[(v, variant)] = entry
                    found = True
                    break
            if found:
                break
        else:
            # Last resort: any flashprog name whose paren-stripped form
            # equals ours, e.g. RON "GD25Q16" vs flashprog "GD25Q16(B)".
            for (fv, fn), entry in flashprog_map.items():
                if fv in candidates and strip_paren(fn) == part:
                    hits[(fv, fn)] = entry
                    break
    if not hits:
        return None, None
    distinct = {id(e) for e in hits.values()}
    if len(distinct) > 1:
        raise _Conflict(name, sorted(k[1] for k in hits))
    refkey = sorted(hits)[0]
    return hits[refkey], refkey


# Explicit RON-name -> flashprog-name aliases for entries whose naming
# differs beyond slash/paren variants (same die, different punctuation).
NAME_ALIASES = {
    ("Winbond", "W25Q32JV_M"): ("Winbond", "W25Q32JV-.M"),
    ("Winbond", "W25Q64JV_M"): ("Winbond", "W25Q64JV-.M"),
    ("Winbond", "W25Q128JV_M"): ("Winbond", "W25Q128.V..M"),
    # Family entries: the alias target is the conservative base die, and the
    # derived combined-write QE method works family-wide (every member
    # carries WRSR_EXT2), while QPI is only claimed where the base die has
    # it. This strips e.g. the fabricated qpi_38_ff on W25Q32.W/W25Q64.W.
    ("Winbond", "W25Q32.V"): ("Winbond", "W25Q32BV"),
    ("Winbond", "W25Q64.V"): ("Winbond", "W25Q64BV"),
    ("Winbond", "W25Q32.W"): ("Winbond", "W25Q32BW"),
    ("Winbond", "W25Q64.W"): ("Winbond", "W25Q64FW"),
    # Bare Micron names are the 3V variant of the dotted same-die entries
    # (voltages match); the reference encodes no QE and no multi-IO for any
    # of them (Micron QE lives in the NV config register, factory-set).
    ("Micron", "N25Q032"): ("Micron/Numonyx/ST", "N25Q032..3E"),
    ("Micron", "N25Q064"): ("Micron/Numonyx/ST", "N25Q064..3E"),
    ("Micron", "N25Q128"): ("Micron/Numonyx/ST", "N25Q128..3E"),
    ("Micron", "N25Q256"): ("Micron/Numonyx/ST", "N25Q256..3E"),
    ("Micron", "N25Q512"): ("Micron/Numonyx/ST", "N25Q512..3G"),
}


class _Conflict(Exception):
    def __init__(self, name: str, refs: list[str]):
        super().__init__(name)
        self.refs = refs


def process_vendor_file(
    path: Path, flashprog_map: dict, apply: bool, stats: dict
) -> None:
    text = path.read_text()
    vendor_match = re.search(r'vendor:\s*"([^"]+)"', text)
    vendor = vendor_match.group(1) if vendor_match else None

    lines = text.splitlines(keepends=True)
    out_lines: list[str] = []
    last_name: str | None = None

    i = 0
    while i < len(lines):
        line = lines[i]
        name_match = re.search(r'^\s*name:\s*"([^"]+)",', line)
        if name_match:
            last_name = name_match.group(1)

        fm = FEATURES_LINE_RE.match(line)
        if fm and last_name is not None and vendor is not None:
            try:
                entry, refkey = lookup_entry(flashprog_map, vendor, last_name)
            except _Conflict as c:
                stats["conflicts"].append(f"{path.name}:{last_name} -> {c.refs}")
                last_name = None
                out_lines.append(line)
                i += 1
                continue
            if entry is None:
                stats["unmatched"].append(f"{path.name}:{last_name}")
            else:
                stats["matched"] += 1
                pairs = parse_inner(fm.group("inner"))
                current = {k: v for k, v in pairs}
                want = entry["flags"]

                # Two-way sync of owned flags; preserve existing order,
                # append newly-added flags sorted for determinism.
                new_pairs = [
                    (k, v)
                    for k, v in pairs
                    if not (k in MULTI_IO_FLAGS and k not in want)
                ]
                have_keys = {k for k, _ in new_pairs}
                for k in sorted(want - have_keys):
                    new_pairs.append((k, True))
                    stats["flags_added"] += 1
                removed = [
                    k
                    for k in current
                    if k in MULTI_IO_FLAGS and k not in want and current[k]
                ]
                stats["flags_removed"] += len(removed)

                # wrsr_ewsr is add-only: volatile-write capability.
                if entry["ewsr"] and not current.get("wrsr_ewsr"):
                    new_pairs.append(("wrsr_ewsr", True))
                    stats["ewsr_added"] += 1

                if len(new_pairs) != len(pairs) or any(
                    a != b for a, b in zip(new_pairs, pairs)
                ):
                    line = f"{fm.group('lead')}features: ({build_inner(new_pairs)}),\n"
                    stats["entries_touched"] += 1

                # qe_method handling within this entry's window.
                want_qe = entry["qe_method"]
                j = i + 1
                qe_idx: int | None = None
                while j < len(lines):
                    if QE_LINE_RE.match(lines[j]):
                        qe_idx = j
                        break
                    if ENTRY_FIELD_RE.match(lines[j]):
                        break
                    j += 1
                if want_qe is None:
                    if qe_idx is not None:
                        del lines[qe_idx]
                        # lines shifted: current line already appended below,
                        # continue scanning after it.
                        stats["qe_removed"] += 1
                        stats["entries_touched"] += 1
                        # Adjust: we deleted a later line; rebuild indexing.
                        # Simplest: operate on a mutable list copy.
                        out_lines.append(line)
                        i += 1
                        last_name = None
                        continue
                else:
                    indent = fm.group("lead")
                    if qe_idx is not None:
                        m = QE_LINE_RE.match(lines[qe_idx])
                        if m is None:  # pragma: no cover - regex just matched
                            raise RuntimeError(f"qe line vanished in {path}:{last_name}")
                        if m.group("val") != want_qe:
                            lines[qe_idx] = f"{m.group('lead')}qe_method: {want_qe},\n"
                            stats["qe_changed"] += 1
                            stats["entries_touched"] += 1
                    else:
                        # Insert after the features line. `lines` is being
                        # consumed via index i; insert into it directly.
                        lines.insert(i + 1, f"{indent}qe_method: {want_qe},\n")
                        stats["qe_added"] += 1
                        stats["entries_touched"] += 1
            last_name = None

        out_lines.append(line)
        i += 1

    if apply and "".join(out_lines) != text:
        path.write_text("".join(out_lines))
        print(f"Updated {path.name}")


# Pinned derivations guarding against systematic misattribution (e.g. a
# search-direction bug makes every entry inherit its neighbour's QE while the
# dry run still converges). (flashprog vendor, name) -> expected qe_method.
QE_SELF_TEST = {
    ("GigaDevice", "GD25Q64(B)"): "Sr2Bit1WriteSr2",  # WRSR2
    ("GigaDevice", "GD25Q16(B)"): "Sr2Bit1WriteSr",  # EXT2 only
    ("GigaDevice", "GD55B01GE"): None,  # no .qe at all
    ("Macronix", "MX25L1605A"): None,  # no .qe at all
    ("Micron", "MT25QL128"): None,  # no .qe; QE in NV config register
    ("Macronix", "MX25L25635F"): "Sr1Bit6",
    ("Atmel", "AT25SL128A"): "Sr2Bit1WriteSr2",  # WRSR2, no EXT2
    ("Fudan", "FM25Q02"): "Sr2Bit1WriteSr2",  # EXT2+WRSR2: dedicated wins
    ("XMC", "XM25QH128C"): "Sr2Bit1WriteSr2",  # WRSR2
}


def run_self_test(flashprog_map: dict) -> None:
    failures = []
    for key, want in QE_SELF_TEST.items():
        entry = flashprog_map.get(key)
        if entry is None:
            failures.append(f"{key}: reference entry vanished")
        elif entry["qe_method"] != want:
            failures.append(f"{key}: got {entry['qe_method']}, want {want}")
    if failures:
        for f in failures:
            print(f"SELF-TEST FAIL: {f}", file=sys.stderr)
        sys.exit("audit self-test failed: .qe attribution is wrong")
    print(
        f"self-test: {len(QE_SELF_TEST)} pinned QE derivations ok",
        file=sys.stderr,
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--apply", action="store_true", help="Write changes in place")
    args = parser.parse_args()

    if not FLASHPROG.exists():
        sys.exit(f"flashchips.c not found at {FLASHPROG}")

    flashprog_map = load_flashprog()
    print(f"Loaded {len(flashprog_map)} chip names from flashchips.c", file=sys.stderr)

    run_self_test(flashprog_map)

    stats = {
        "matched": 0,
        "unmatched": [],
        "entries_touched": 0,
        "flags_added": 0,
        "flags_removed": 0,
        "qe_added": 0,
        "qe_changed": 0,
        "qe_removed": 0,
        "ewsr_added": 0,
        "conflicts": [],
    }
    for ron in sorted(VENDORS.glob("*.ron")):
        process_vendor_file(ron, flashprog_map, apply=args.apply, stats=stats)

    action = "Applied" if args.apply else "Would apply"
    print(
        f"\n{action}: {stats['entries_touched']} entries "
        f"({stats['matched']} matched reference)"
    )
    print(
        f"  flags +{stats['flags_added']}/-{stats['flags_removed']}, "
        f"qe +{stats['qe_added']}/~{stats['qe_changed']}/-{stats['qe_removed']}, "
        f"ewsr +{stats['ewsr_added']}"
    )
    if stats["conflicts"]:
        print(f"\nConflicting RON entries ({len(stats['conflicts'])}) — left alone:")
        for u in stats["conflicts"]:
            print(f"  {u}")
    if stats["unmatched"]:
        print(f"\nUnmatched RON entries ({len(stats['unmatched'])}) — left alone:")
        for u in stats["unmatched"]:
            print(f"  {u}")


if __name__ == "__main__":
    main()

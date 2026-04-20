#!/usr/bin/env python3
"""
Multi-IO chip database audit script.

Reads flashprog's flashchips.c, extracts feature_bits for each SPI flash
entry, then rewrites our chips/vendors/*.ron files to set fine-grained
multi-IO flags (fast_read_dout, fast_read_dio, fast_read_qout, fast_read_qio,
qpi_35_f5, qpi_38_ff, set_read_params, fast_read_qpi4b) and qe_method.

The script is conservative: it only ADDS flags inside the `features: (...)`
tuple; it never removes existing flags or reorders other fields. Chips that
don't have a matching entry in flashchips.c are left alone.

This implementation is line-based and matches one chip entry at a time via
a simple state machine — avoids the pitfalls of multi-line regex across
adjacent chips.

Usage:
    ./util/multiio_audit.py          # Dry run (prints summary)
    ./util/multiio_audit.py --apply  # Apply in place
"""

import argparse
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FLASHPROG = ROOT.parent / "flashprog" / "flashchips.c"
VENDORS = ROOT / "chips" / "vendors"

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


QE_WRITE_MAP = {
    ("STATUS1", "6"): "Sr1Bit6",
    # bit=1, STATUS2: most chips now use the dedicated 0x31 write, but some
    # still use combined WRSR 0x01 (Sr2Bit1WriteSr). Without parsing flashprog's
    # `write` callback we can't tell, so leave it to vendor default.
    ("STATUS2", "7"): "Sr2Bit7",
}

# Our vendor name → accepted flashprog vendor names.
VENDOR_ALIASES = {
    "Eon": ["Eon", "EON"],
    "Micron": ["Micron", "Micron/Numonyx/ST"],
    "XTX": ["XTX Technology", "XTX"],
    "Boya": ["Boya/BoHong Microelectronics", "Boya"],
    "Spansion": ["Spansion", "Cypress"],
    "Zetta": ["Zetta Device", "Zetta"],
}


# ----------------------------------------------------------------------
# Load flashprog entries
# ----------------------------------------------------------------------

CHIP_HEADER_RE = re.compile(r'\.vendor\s*=\s*"([^"]+)",\s*\.name\s*=\s*"([^"]+)",')


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
        flags: set[str] = set()
        if fm:
            flags = parse_feature_expr(fm.group(1))
        qe_method = None
        qm = re.search(r"\.qe\s*=\s*\{\s*STATUS(\d)\s*,\s*(\d)\s*\}", body)
        if qm:
            qe_method = QE_WRITE_MAP.get((f"STATUS{qm.group(1)}", qm.group(2)))
        out[(vendor, name)] = {"flags": flags, "qe_method": qe_method}
    return out


# ----------------------------------------------------------------------
# Update RON files
# ----------------------------------------------------------------------

FEATURES_LINE_RE = re.compile(r"^(?P<lead>\s*)features:\s*\((?P<inner>.*)\),\s*$")


def parse_inner(inner: str) -> dict[str, bool]:
    result = {}
    # Walk key: value pairs, stripping whitespace/commas.
    for pair in re.finditer(r"(\w+)\s*:\s*(true|false)", inner):
        result[pair.group(1)] = pair.group(2) == "true"
    return result


def build_inner(flags: dict[str, bool]) -> str:
    # Preserve original insertion order where possible by not sorting.
    parts = [f"{k}: {'true' if v else 'false'}" for k, v in flags.items() if v]
    return ", ".join(parts)


def lookup_entry(flashprog_map: dict, our_vendor: str, name: str):
    """Look up a chip entry, trying aliased vendor names."""
    candidates = VENDOR_ALIASES.get(our_vendor, [our_vendor])
    for v in candidates:
        entry = flashprog_map.get((v, name))
        if entry is not None:
            return entry
    return None


def process_vendor_file(
    path: Path, flashprog_map: dict, apply: bool
) -> tuple[int, int]:
    """Return (chips_modified, qe_method_added)."""
    text = path.read_text()
    vendor_match = re.search(r'vendor:\s*"([^"]+)"', text)
    vendor = vendor_match.group(1) if vendor_match else None

    lines = text.splitlines(keepends=True)
    out_lines: list[str] = []
    modified = 0
    qe_added = 0

    i = 0
    last_name = None
    while i < len(lines):
        line = lines[i]
        name_match = re.search(r'^\s*name:\s*"([^"]+)",', line)
        if name_match:
            last_name = name_match.group(1)

        fm = FEATURES_LINE_RE.match(line)
        if fm and last_name is not None and vendor is not None:
            entry = lookup_entry(flashprog_map, vendor, last_name)
            if entry is not None:
                current = parse_inner(fm.group("inner"))
                new_flags = entry["flags"] & {
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
                to_add = {k for k in new_flags if not current.get(k)}
                if to_add:
                    for k in to_add:
                        current[k] = True
                    new_inner = build_inner(current)
                    line = f"{fm.group('lead')}features: ({new_inner}),\n"
                    modified += 1

                # Check next few lines for existing qe_method; if absent and
                # we have a quad flag, insert it.
                has_quad = any(
                    current.get(f)
                    for f in (
                        "fast_read_qout",
                        "fast_read_qio",
                        "qpi_35_f5",
                        "qpi_38_ff",
                        "set_read_params",
                        "fast_read_qpi4b",
                    )
                )
                if has_quad and entry["qe_method"]:
                    # Look at the next 10 lines for qe_method:
                    window = "".join(lines[i : i + 10])
                    if "qe_method:" not in window:
                        out_lines.append(line)
                        indent = fm.group("lead")
                        out_lines.append(f"{indent}qe_method: {entry['qe_method']},\n")
                        qe_added += 1
                        i += 1
                        continue

            last_name = None  # Consume the name so we don't re-apply to other blocks.

        out_lines.append(line)
        i += 1

    if apply and (modified or qe_added):
        path.write_text("".join(out_lines))
    return modified, qe_added


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--apply", action="store_true", help="Write changes in place")
    args = parser.parse_args()

    if not FLASHPROG.exists():
        sys.exit(f"flashchips.c not found at {FLASHPROG}")

    flashprog_map = load_flashprog()
    print(f"Loaded {len(flashprog_map)} chips from flashchips.c", file=sys.stderr)

    total_mod = 0
    total_qe = 0
    for ron in sorted(VENDORS.glob("*.ron")):
        mod, qe = process_vendor_file(ron, flashprog_map, apply=args.apply)
        if mod or qe:
            action = "Updated" if args.apply else "Would update"
            print(f"{action} {ron.name}: {mod} chips, {qe} qe_method")
            total_mod += mod
            total_qe += qe

    print(f"\nTotal: {total_mod} chip entries updated, {total_qe} qe_method added")


if __name__ == "__main__":
    main()

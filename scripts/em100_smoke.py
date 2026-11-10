#!/usr/bin/env python3
"""Resumable, emulator-only EM100 / DediBridge smoke tests at 24 MHz.

Inventory is the default. --execute loads EM100 SDRAM; --phase all additionally
runs bounded SPI erase/program tests. Never use this on an in-system flash bus.
See docs/em100-smoke.md for wiring, building, coverage and limitations.
"""

from __future__ import annotations

import argparse
import collections
import contextlib
import csv
import fcntl
import hashlib
import json
import os
import re
import shlex
import signal
import struct
import subprocess
import sys
import tarfile
import time
from pathlib import Path
from typing import Any

CAPACITY = 64 * 1024 * 1024  # rem100's checked SDRAM API limit, not a guessed G2 limit


def digest(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def parse_json(text):
    try:
        return json.loads(text)
    except json.JSONDecodeError as error:
        raise RuntimeError("corrupt run metadata; refusing to resume") from error


def atomic_json(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def alternatives(pattern):
    depth, start = 0, 0
    for i, char in enumerate(pattern):
        if char == "(":
            depth += 1
        elif char == ")":
            depth = max(0, depth - 1)
        elif char == "/" and depth == 0:
            yield pattern[start:i]
            start = i + 1
    yield pattern[start:]


def name_regex(pattern):
    """Mirror rflasher-chip-types/src/name.rs, including optional groups."""
    choices = list(alternatives(pattern))
    if len(choices) > 1:
        return "(?:" + "|".join(name_regex(c) for c in choices) + ")"
    result, i = "", 0
    while i < len(pattern):
        char = pattern[i]
        if char == "(":
            depth, end = 1, i + 1
            while end < len(pattern) and depth:
                if pattern[end] == "(":
                    depth += 1
                elif pattern[end] == ")":
                    depth -= 1
                end += 1
            if depth == 0:
                inner = pattern[i + 1 : end - 1]
                optional = len(list(alternatives(inner))) == 1
                result += "(?:" + name_regex(inner) + ")" + ("?" if optional else "")
                i = end
                continue
        result += "." if char == "." else re.escape(char)
        i += 1
    return result


def name_matches(pattern, wanted):
    return (
        re.fullmatch(name_regex(pattern), wanted, re.IGNORECASE | re.ASCII) is not None
    )


def profile_sfdp_size(data):
    """Read only a complete, version-one BFPT density from a Dcfg SFDP block.

    A difference is unresolved metadata, not permission to override geometry:
    stacked parts can describe one die rather than the whole package.
    """
    if data[176:180] != b"SFDP":
        return None
    table = data[180:]
    if len(table) < 16 or table[:4] != b"SFDP" or table[5] != 1:
        return None
    for index in range(table[6] + 1):
        header = table[8 + index * 8 : 16 + index * 8]
        if len(header) != 8:
            return None
        if header[0] != 0 or header[7] != 0xFF or header[2] != 1 or header[3] < 2:
            continue
        pointer = int.from_bytes(header[4:7], "little")
        if pointer + 8 > len(table):
            return None
        density = struct.unpack_from("<I", table, pointer + 4)[0]
        if density & 0x80000000:
            exponent = density & 0x7FFFFFFF
            return (1 << exponent) // 8 if 3 <= exponent <= 63 else None
        return (density + 1) // 8
    return None


@contextlib.contextmanager
def cleanup_signals():
    """Let orderly cleanup finish despite a repeated SIGINT or SIGTERM."""
    previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        for sig in previous:
            signal.signal(sig, signal.SIG_IGN)
        yield
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


def stop_process_group(process):
    """Terminate/reap the command even if it exits between wait and killpg."""
    with contextlib.suppress(ProcessLookupError):
        os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        with contextlib.suppress(ProcessLookupError):
            os.killpg(process.pid, signal.SIGKILL)
        process.wait()


def profiles(archive):
    result = []
    with tarfile.open(archive) as source:
        for member in source:
            if not member.isfile() or not member.name.endswith(".cfg"):
                continue
            entry = source.extractfile(member)
            if entry is None:
                raise ValueError(f"unreadable profile {member.name}")
            data = entry.read()
            if len(data) < 176 or data[:4] != b"Dcfg":
                raise ValueError(f"invalid profile {member.name}")
            offset, size, vendor_offset, name_offset = struct.unpack_from(
                "<IIII", data, 8
            )

            def text(off, content=data):
                return content[off:].split(b"\0", 1)[0].decode("ascii")

            registers, bank = {}, 0x2300
            for pos in range(offset, 176, 4):
                value, register = struct.unpack_from("<HH", data, pos)
                if value == register == 0xFFFF:
                    bank = 0x1100
                else:
                    registers[bank + register] = value
            identity = registers.get(0x2342, 0)
            device = registers.get(0x2340, 0)
            result.append(
                {
                    "vendor": text(vendor_offset),
                    "name": Path(member.name).stem,
                    "reported_name": text(name_offset),
                    "size": size,
                    "config_hex": data.hex(),
                    "voltage_mv": registers.get(0x1104),
                    "manufacturer": identity >> 8,
                    "device_id": ((identity & 255) << 8) | (device >> 8),
                    "config_sha256": hashlib.sha256(data).hexdigest(),
                    "sfdp": b"SFDP" in data[176:],
                    "sfdp_size": profile_sfdp_size(data),
                }
            )
    names = [p["name"] for p in result]
    if len(set(names)) != len(names):
        raise ValueError("duplicate emulator profile names")
    return sorted(result, key=lambda p: (p["size"], p["name"]))


def chip_listing(output):
    result = []
    for line in output.splitlines():
        match = re.fullmatch(
            r"(\S+)\s+(.*?)\s+(\d+)\s+(KiB|MiB)\s+([0-9a-fA-F]{2,6})\s+([0-9a-fA-F]{4})\s*",
            line,
        )
        if match:
            vendor, name, count, unit, manufacturer, device = match.groups()
            result.append(
                {
                    "vendor": vendor,
                    "name": name,
                    "size": int(count) * (1024 if unit == "KiB" else 1024**2),
                    "manufacturer": int(manufacturer, 16),
                    "device_id": int(device, 16),
                }
            )
    if not result:
        raise ValueError("could not parse rflasher list-chips")
    return result


def map_profile(profile, chips):
    same_id = [
        c
        for c in chips
        if all(c[k] == profile[k] for k in ("size", "manufacturer", "device_id"))
    ]
    names = [profile["name"], profile.get("reported_name", profile["name"])]
    if profile["vendor"] == "Microchip/SST":
        names.extend("SST" + name for name in names[:])
    matches = [
        c
        for c in same_id
        if any(
            c["name"].lower() == n.lower() or name_matches(c["name"], n) for n in names
        )
    ]
    if matches:
        matches.sort(
            key=lambda c: (
                c["name"].lower() != profile["name"].lower(),
                -len(c["name"]),
            )
        )
        return matches[0]["name"], "name_and_id", [c["name"] for c in matches]
    if len(same_id) == 1:
        return same_id[0]["name"], "unique_id_and_size", [same_id[0]["name"]]
    return (
        None,
        "unmapped" if not same_id else "ambiguous_id",
        [c["name"] for c in same_id],
    )


def compare(expected, actual) -> dict[str, Any]:
    if len(expected) != len(actual):
        return {
            "outcome": "fail",
            "reason": "length_mismatch",
            "expected_size": len(expected),
            "actual_size": len(actual),
        }
    if expected != actual:
        first = next(
            i for i, (a, b) in enumerate(zip(expected, actual, strict=True)) if a != b
        )
        return {
            "outcome": "fail",
            "reason": "data_mismatch",
            "first_mismatch": first,
            "expected_hex": expected[first : first + 32].hex(),
            "actual_hex": actual[first : first + 32].hex(),
        }
    return {
        "outcome": "pass",
        "sha256": hashlib.sha256(actual).hexdigest(),
        "bytes": len(actual),
    }


def regions(size):
    # An aligned interior erase window preserves sentinels on both sides where
    # capacity allows. Large parts also exercise programming above 16 MiB.
    start = 65536 if size >= 3 * 65536 else 0
    return [(start, 65536)] + (
        [(16 * 1024 * 1024, 65536)] if size > 16 * 1024 * 1024 else []
    )


class Harness:
    def __init__(self, args):
        self.args = args
        self.output = args.output.resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        self.needs_recovery = False
        self.programmer = f"dediprog:id={args.programmer_id},spispeed=24M"
        self.results = []
        path = self.output / "results.jsonl"
        if path.exists():
            self.results = [parse_json(line) for line in path.read_text().splitlines()]

    def check_pico(self):
        found = []
        for device in Path("/sys/bus/usb/devices").iterdir():
            try:
                if (device / "idVendor").read_text().strip() == "0483" and (
                    device / "idProduct"
                ).read_text().strip() == "dada":
                    found.append((device / "serial").read_text().strip())
            except FileNotFoundError:
                continue
        if found != [self.args.pico_serial]:
            raise RuntimeError(
                f"expected only Pico {self.args.pico_serial}; found {found}"
            )

    def command(self, directory, name, argv, timeout=240):
        self.check_pico()
        log = directory / (name + ".log")
        if log.exists():
            raise RuntimeError(f"refusing to overwrite prior command evidence: {log}")
        argv = [str(a) for a in argv]
        started = time.monotonic()
        with log.open("x") as stream:
            stream.write("$ " + shlex.join(argv) + "\n")
            stream.flush()
            process = subprocess.Popen(
                argv, stdout=stream, stderr=subprocess.STDOUT, start_new_session=True
            )
            rc = "unknown"
            try:
                rc = process.wait(timeout=timeout)
            except (subprocess.TimeoutExpired, KeyboardInterrupt) as error:
                rc = 130 if isinstance(error, KeyboardInterrupt) else 124
                if argv[0] == self.args.rflasher:
                    self.needs_recovery = True
                with cleanup_signals():
                    stop_process_group(process)
                if isinstance(error, KeyboardInterrupt):
                    raise
            finally:
                stream.write(f"\nexit={rc} seconds={time.monotonic() - started:.3f}\n")
        text = log.read_text()
        if (
            argv[0] == self.args.rflasher
            and rc
            and re.search(
                r"(?i)(stall|pipe|bulk.*(?:fail|error)|transfer.*(?:fail|error))", text
            )
        ):
            self.needs_recovery = True
        return rc, text

    def helper(self, directory, name, command, *args):
        return self.command(
            directory, name, [self.args.helper, command, self.args.em100_serial, *args]
        )

    def flash(self, directory, name, operation, *args):
        return self.command(
            directory,
            name,
            [
                self.args.rflasher,
                operation,
                "--non-interactive",
                "-p",
                self.programmer,
                *args,
            ],
        )

    def recover(self, directory):
        if self.needs_recovery:
            rc, _ = self.command(
                directory,
                f"recovery-{time.time_ns()}",
                [
                    self.args.recover_tool,
                    "recover-spi",
                    "--serial",
                    self.args.pico_serial,
                ],
            )
            if rc:
                raise RuntimeError(
                    "explicit SPI recovery failed; stopping instead of rebooting or hiding the failure"
                )
            self.needs_recovery = False

    def record(self, profile, phase, **result):
        row = dict(
            name=profile["name"],
            vendor=profile["vendor"],
            size=profile["size"],
            phase=phase,
            timestamp=time.time(),
            **result,
        )
        with (self.output / "results.jsonl").open("a") as stream:
            stream.write(json.dumps(row) + "\n")
            stream.flush()
            os.fsync(stream.fileno())
        self.results.append(row)
        fields = [
            "name",
            "vendor",
            "size",
            "phase",
            "outcome",
            "reason",
            "selected_chip",
            "first_mismatch",
        ]
        with (self.output / "results.csv").open("w") as stream:
            writer = csv.DictWriter(stream, fieldnames=fields, extrasaction="ignore")
            writer.writeheader()
            writer.writerows(self.results)
        print(
            f"{phase:6} {profile['name']:24} {result['outcome']:18} {result.get('reason', '')}",
            flush=True,
        )

    def read_profile(self, profile, directory):
        size = profile["size"]
        data = hashlib.shake_256((profile["name"] + ":read-v1").encode()).digest(size)
        expected = directory / "preload.bin"
        expected.write_bytes(data)
        configuration = directory / "profile.cfg"
        configuration.write_bytes(bytes.fromhex(profile["config_hex"]))
        rc, _ = self.helper(
            directory, "configure", "configure", configuration, expected
        )
        if rc:
            return {
                "outcome": "infrastructure_error",
                "reason": "configure_or_preload_failed",
            }
        rc, auto = self.flash(directory, "auto-probe", "probe")
        autodetection = {
            "exit": rc,
            "ambiguous": "aliases" in auto.lower() or "ambig" in auto.lower(),
        }
        chosen = profile["selected_chip"]
        selection = ["-c", chosen] if chosen else []
        rc, info = self.flash(
            directory, "explicit-info" if chosen else "auto-info", "info", *selection
        )
        if rc:
            return {
                "outcome": "fail" if chosen else "unsupported",
                "reason": "explicit_selection_failed"
                if chosen
                else "no_usable_definition",
                "autodetection": autodetection,
                "selected_chip": chosen,
            }
        detected_size = re.search(r"^Size:\s+(\d+) bytes", info, re.MULTILINE)
        if not detected_size:
            return {
                "outcome": "infrastructure_error",
                "reason": "unexpected_info_format",
            }
        if int(detected_size[1]) != size:
            return {
                "outcome": "unresolved",
                "reason": "profile_and_detection_geometry_disagree",
                "profile_size": size,
                "detected_size": int(detected_size[1]),
                "autodetection": autodetection,
                "selected_chip": chosen,
            }
        actual = directory / "readback.bin"
        rc, _ = self.flash(directory, "full-read", "read", *selection, actual)
        if rc:
            return {
                "outcome": "fail",
                "reason": "read_command_failed",
                "autodetection": autodetection,
                "selected_chip": chosen,
            }
        result = compare(data, actual.read_bytes())
        result.update(
            selected_chip=chosen,
            autodetection=autodetection,
            sfdp="SFDP:            Not detected" not in info,
        )
        return result

    def mutate_profile(self, profile, directory):
        read = next(
            (
                r
                for r in self.results
                if r["name"] == profile["name"] and r["phase"] == "read"
            ),
            None,
        )
        if not read or read["outcome"] != "pass":
            return {"outcome": "not_run", "reason": "read_did_not_pass"}
        chosen = profile["selected_chip"]
        if not chosen:
            return {"outcome": "not_run", "reason": "no_explicit_chip_mapping"}
        info = (Path(read["directory"]) / "explicit-info.log").read_text()
        sizes = [
            int(n) * (1024 if unit == "KiB" else 1024**2)
            for n, unit in re.findall(
                r"Opcode 0x[0-9A-Fa-f]+:\s+(\d+)\s+(KiB|MiB)", info
            )
        ]
        if not sizes or min(sizes) > 65536:
            return {"outcome": "not_run", "reason": "no_bounded_erase_geometry"}
        size = profile["size"]
        expected = bytearray(
            hashlib.shake_256((profile["name"] + ":mutate-v1").encode()).digest(size)
        )
        preload = directory / "preload.bin"
        preload.write_bytes(expected)
        configuration = directory / "profile.cfg"
        configuration.write_bytes(bytes.fromhex(profile["config_hex"]))
        rc, _ = self.helper(directory, "configure", "configure", configuration, preload)
        if rc:
            return {
                "outcome": "infrastructure_error",
                "reason": "configure_or_preload_failed",
            }
        windows = regions(size)
        layout = directory / "layout.toml"
        layout.write_text(
            '[layout]\nname = "EM100 disposable smoke"\nchip_size = "'
            + str(size)
            + '"\n'
            + "".join(
                f'\n[[region]]\nname = "test{i}"\nstart = {start}\nend = {start + length - 1}\n'
                for i, (start, length) in enumerate(windows)
            )
        )
        selection = ["-c", chosen, "--layout", layout]
        includes = ["--include", ",".join(f"test{i}" for i in range(len(windows)))]
        rc, _ = self.flash(directory, "erase", "erase", *selection, *includes)
        if rc:
            self.recover(directory)
            self.helper(
                directory,
                "failed-erase-capture",
                "capture",
                size,
                directory / "failed-erase.bin",
            )
            return {
                "outcome": "fail",
                "reason": "erase_command_failed",
                "selected_chip": chosen,
            }
        for start, length in windows:
            expected[start : start + length] = b"\xff" * length
        erased = directory / "erased.bin"
        rc, _ = self.helper(
            directory, "erase-independent-upload", "capture", size, erased
        )
        if rc:
            return {
                "outcome": "infrastructure_error",
                "reason": "erase_independent_upload_failed",
            }
        checked = compare(expected, erased.read_bytes())
        if checked["outcome"] != "pass":
            return dict(
                checked, stage="erase_independent_readback", selected_chip=chosen
            )
        files = []
        # A 1027-byte prefix spans several page boundaries and leaves the rest
        # of each erased sector unchanged; the complete chip is checked below.
        for i, (start, _) in enumerate(windows):
            patch = hashlib.shake_256(
                (profile["name"] + f":patch-{i}-v1").encode()
            ).digest(1027)
            path = directory / f"patch-{i}.bin"
            path.write_bytes(patch)
            files.append(f"test{i}:{path}")
            expected[start : start + len(patch)] = patch
        rc, _ = self.flash(
            directory, "program", "write", *selection, "--include", ",".join(files)
        )
        if rc:
            self.recover(directory)
            self.helper(
                directory,
                "failed-program-capture",
                "capture",
                size,
                directory / "failed-program.bin",
            )
            return {
                "outcome": "fail",
                "reason": "program_command_failed",
                "selected_chip": chosen,
            }
        actual = directory / "programmed.bin"
        rc, _ = self.helper(
            directory, "program-independent-upload", "capture", size, actual
        )
        if rc:
            return {
                "outcome": "infrastructure_error",
                "reason": "program_independent_upload_failed",
            }
        checked = compare(expected, actual.read_bytes())
        checked.update(
            selected_chip=chosen,
            windows=windows,
            programmed_bytes=1027 * len(windows),
            independent_erase=checked["outcome"] == "pass",
        )
        if checked["outcome"] == "pass":
            final = directory / "rflasher-programmed.bin"
            rc, _ = self.flash(
                directory, "program-full-read", "read", "-c", chosen, final
            )
            if rc:
                return {
                    "outcome": "fail",
                    "reason": "program_full_read_command_failed",
                    "selected_chip": chosen,
                }
            checked = dict(
                compare(expected, final.read_bytes()),
                selected_chip=chosen,
                windows=windows,
                programmed_bytes=1027 * len(windows),
                independent_erase=True,
                independent_program=True,
            )
        return checked

    def run_hardware(self, inventory):
        lock_directory = Path.home() / ".cache/rflasher-hardware-locks"
        lock_directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        lock_path = lock_directory / f"em100-{self.args.em100_serial}.lock"
        with lock_path.open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            run_id = time.time_ns()
            status = self.output / "status.json"
            atomic_json(
                status, {"state": "preparing", "run_id": run_id, "restored": False}
            )
            atomic_json(
                self.output / "restoration.json",
                {
                    "state": "pending",
                    "run_id": run_id,
                    "restored": False,
                },
            )
            self.check_pico()
            backup = self.output / "original-sdram.bin"
            if not backup.exists():
                rc, _ = self.helper(
                    self.output, "backup-first", "backup", CAPACITY, backup
                )
                if rc:
                    raise RuntimeError(
                        "initial backup failed; no configuration changes attempted"
                    )
                second = self.output / "original-sdram-second.bin"
                rc, _ = self.helper(
                    self.output, "backup-second", "backup", CAPACITY, second
                )
                if (
                    rc
                    or backup.stat().st_size != CAPACITY
                    or digest(backup) != digest(second)
                ):
                    raise RuntimeError("initial SDRAM backups are not identical")
                atomic_json(
                    self.output / "backup.json",
                    {"size": CAPACITY, "sha256": digest(backup)},
                )
            reference = parse_json((self.output / "backup.json").read_text())
            if (
                backup.stat().st_size != CAPACITY
                or digest(backup) != reference["sha256"]
            ):
                raise RuntimeError("recovery backup is damaged")
            execution_error = None
            try:
                phases = (
                    ["read", "mutate"]
                    if self.args.phase == "all"
                    else [self.args.phase]
                )
                done = {(r["name"], r["phase"]) for r in self.results}
                for phase in phases:
                    for profile in inventory:
                        if self.args.chips and profile["name"] not in self.args.chips:
                            continue
                        if (profile["name"], phase) in done:
                            continue
                        atomic_json(
                            status,
                            {
                                "state": "running",
                                "run_id": run_id,
                                "restored": False,
                                "pid": os.getpid(),
                                "phase": phase,
                                "name": profile["name"],
                                "completed": len(self.results),
                            },
                        )
                        if profile["voltage_mv"] != 3300:
                            self.record(
                                profile,
                                phase,
                                outcome="emulator_limited",
                                reason="requires_non_3v3_profile",
                            )
                            continue
                        if profile["size"] > CAPACITY:
                            self.record(
                                profile,
                                phase,
                                outcome="emulator_limited",
                                reason="exceeds_rem100_64mib_limit",
                            )
                            continue
                        if profile.get("sfdp_size") not in (None, profile["size"]):
                            self.record(
                                profile,
                                phase,
                                outcome="unresolved",
                                reason="profile_sfdp_density_differs",
                                sfdp_size=profile["sfdp_size"],
                            )
                            continue
                        directory = self.output / profile["name"] / phase
                        if directory.exists() and any(directory.iterdir()):
                            directory = directory / f"attempt-{time.time_ns()}"
                        directory.mkdir(parents=True, exist_ok=True)
                        self.recover(directory)
                        result = (
                            self.read_profile
                            if phase == "read"
                            else self.mutate_profile
                        )(profile, directory)
                        self.record(profile, phase, directory=str(directory), **result)
                        # Preserve failed images; passing runs retain hashes/logs.
                        if result["outcome"] == "pass":
                            for path in directory.glob("*.bin"):
                                path.unlink()
                        if result["outcome"] == "infrastructure_error":
                            raise RuntimeError(
                                f"infrastructure failure on {profile['name']}; refusing to continue"
                            )
            except BaseException as error:
                execution_error = error
                raise
            finally:
                with cleanup_signals():
                    restoration = {
                        "restored": False,
                        "state": "restoring",
                        "run_id": run_id,
                    }
                    atomic_json(self.output / "restoration.json", restoration)
                    atomic_json(
                        status,
                        {"state": "restoring", "run_id": run_id, "restored": False},
                    )
                    try:
                        self.recover(self.output)
                        rc, text = self.helper(
                            self.output, f"restore-{time.time_ns()}", "restore", backup
                        )
                        restoration.update(exit=rc, message=text[-1000:])
                        if rc:
                            raise RuntimeError("original SDRAM restoration failed")
                        restoration["restored"] = True
                    except BaseException as error:
                        restoration["error"] = repr(error)
                        if execution_error is None:
                            raise
                        # Do not replace the first failure with a cleanup error.
                        print(f"Cleanup also failed: {error!r}", file=sys.stderr)
                    finally:
                        restoration["state"] = (
                            "restored" if restoration["restored"] else "failed"
                        )
                        restoration["timestamp"] = time.time()
                        atomic_json(self.output / "restoration.json", restoration)
                        final = {
                            "state": "interrupted"
                            if execution_error is not None
                            else (
                                "complete"
                                if restoration["restored"]
                                else "cleanup_failed"
                            ),
                            "run_id": run_id,
                            "restored": restoration["restored"],
                            "completed": len(self.results),
                        }
                        if execution_error is not None:
                            final["error"] = repr(execution_error)
                        if "error" in restoration:
                            final["cleanup_error"] = restoration["error"]
                        atomic_json(status, final)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--configs", type=Path, default=Path.home() / ".em100/configs.tar.xz"
    )
    parser.add_argument("--rflasher", default="/run/current-system/sw/bin/rflasher")
    parser.add_argument(
        "--helper",
        default=str(
            Path(__file__).resolve().parents[2]
            / "rem100/target/debug/em100-smoke-helper"
        ),
    )
    parser.add_argument(
        "--recover-tool",
        default=str(
            Path(__file__).resolve().parents[2]
            / "dedibridge/target/debug/dedibridgectl"
        ),
    )
    parser.add_argument("--em100-serial", required=True)
    parser.add_argument("--pico-serial", required=True)
    parser.add_argument(
        "--programmer-id",
        required=True,
        help="Dediprog EEPROM selector, e.g. SF2147417, not USB serial",
    )
    parser.add_argument("--phase", choices=["read", "mutate", "all"], default="read")
    parser.add_argument("--chips", type=lambda s: s.split(","))
    parser.add_argument(
        "--execute",
        action="store_true",
        help="authorize mutations of this isolated emulator's SDRAM/SPI contents",
    )
    args = parser.parse_args()
    for key in ("em100_serial", "pico_serial", "programmer_id"):
        if not re.fullmatch(r"[A-Za-z0-9]+", getattr(args, key)):
            parser.error(f"invalid {key}")
    args.helper = str(Path(args.helper).resolve())
    args.rflasher = str(Path(args.rflasher).resolve())
    args.recover_tool = str(Path(args.recover_tool).resolve())
    harness = Harness(args)
    listing = subprocess.run(
        [args.rflasher, "list-chips"], capture_output=True, text=True, check=True
    ).stdout
    inventory, chips = profiles(args.configs), chip_listing(listing)
    for profile in inventory:
        profile["selected_chip"], profile["mapping"], profile["candidates"] = (
            map_profile(profile, chips)
        )
    if args.chips and set(args.chips) - {p["name"] for p in inventory}:
        parser.error("unknown chip in --chips")
    fingerprint = {
        "configs": digest(args.configs),
        "rflasher": digest(args.rflasher),
        "helper": digest(args.helper),
        "script": digest(__file__),
        "em100": args.em100_serial,
        "pico": args.pico_serial,
        "programmer_id": args.programmer_id,
        "spi_mhz": 24,
    }
    manifest = harness.output / "manifest.json"
    if manifest.exists() and parse_json(manifest.read_text()) != fingerprint:
        parser.error(
            "run identity/binaries changed; use a new output directory instead of mixing evidence"
        )
    atomic_json(manifest, fingerprint)
    atomic_json(harness.output / "inventory.json", inventory)
    (harness.output / "rflasher-chips.txt").write_text(listing)
    print(
        f"Profiles={len(inventory)}, mappings={dict(collections.Counter(p['mapping'] for p in inventory))}",
        flush=True,
    )
    if args.execute:

        def interrupt(*_):
            raise KeyboardInterrupt

        signal.signal(signal.SIGTERM, interrupt)
        harness.run_hardware(inventory)


if __name__ == "__main__":
    main()

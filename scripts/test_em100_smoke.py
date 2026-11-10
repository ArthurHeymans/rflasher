"""Host-only tests; no USB devices are opened."""

import argparse
import hashlib
import importlib.util
import io
import signal
import struct
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, call, patch

SPEC = importlib.util.spec_from_file_location(
    "em100_smoke", Path(__file__).with_name("em100_smoke.py")
)
assert SPEC is not None and SPEC.loader is not None
smoke = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(smoke)


class SmokeTests(unittest.TestCase):
    def test_patterns(self):
        cases = [
            ("W25Q64BV/W25Q64CV/W25Q64FV", "w25q64fv", True),
            ("W25Q64.V", "W25Q64FV", True),
            ("W25Q64.V", "W25Q64FFV", False),
            ("GD25Q64(B)", "GD25Q64", True),
            ("GD25Q64(B)", "GD25Q64B", True),
            ("GD25Q64(B)", "GD25Q64C", False),
            ("MX25L4005(A/C)", "MX25L4005", False),
            ("MX25L4005(A/C)", "MX25L4005C", True),
            ("MX25L4005(/A/C)", "MX25L4005", True),
            ("ABC(", "ABC(", True),
            ("ABC(", "ABC", False),
            ("", "", True),
        ]
        for pattern, name, expected in cases:
            with self.subTest(pattern=pattern, name=name):
                self.assertEqual(smoke.name_matches(pattern, name), expected)

    def test_listing_and_mapping(self):
        chips = smoke.chip_listing(
            "Winbond      W25Q64BV/W25Q64CV/W25Q64FV  8 MiB  EF 4017\n"
        )
        profile = {
            "vendor": "Winbond",
            "name": "W25Q64FV",
            "size": 8388608,
            "manufacturer": 239,
            "device_id": 0x4017,
        }
        chosen, method, _ = smoke.map_profile(profile, chips)
        self.assertEqual(chosen, "W25Q64BV/W25Q64CV/W25Q64FV")
        self.assertEqual(method, "name_and_id")
        profile["size"] //= 2
        self.assertIsNone(smoke.map_profile(profile, chips)[0])

    def test_unknown_listing_is_not_empty_success(self):
        with self.assertRaises(ValueError):
            smoke.chip_listing("new unexpected CLI format")

    def test_ambiguous_ids_are_not_arbitrarily_mapped(self):
        chips = smoke.chip_listing(
            "Winbond W25Q64BV 8 MiB EF 4017\nWinbond W25Q64FV 8 MiB EF 4017\n"
        )
        profile = {
            "vendor": "Winbond",
            "name": "unknown",
            "size": 8388608,
            "manufacturer": 239,
            "device_id": 0x4017,
        }
        self.assertEqual(smoke.map_profile(profile, chips)[1], "ambiguous_id")
        self.assertIsNone(smoke.map_profile(profile, chips)[0])

    def test_sst_prefix_and_unique_id_mapping(self):
        chips = smoke.chip_listing("SST SST25VF016B 2 MiB BF 2541\n")
        profile = {
            "vendor": "Microchip/SST",
            "name": "25VF016B",
            "size": 2097152,
            "manufacturer": 191,
            "device_id": 0x2541,
        }
        self.assertEqual(smoke.map_profile(profile, chips)[1], "name_and_id")
        profile["name"] = "renamed"
        self.assertEqual(smoke.map_profile(profile, chips)[1], "unique_id_and_size")

    def test_full_byte_comparison_including_neighbors(self):
        self.assertEqual(smoke.compare(b"abc", b"abc")["outcome"], "pass")
        self.assertEqual(smoke.compare(b"abc", b"ab")["reason"], "length_mismatch")
        self.assertEqual(smoke.compare(b"abc", b"abd")["first_mismatch"], 2)

    def test_large_chip_mutations_cross_address_boundary(self):
        self.assertEqual(smoke.regions(65536), [(0, 65536)])
        self.assertEqual(len(smoke.regions(16 * 1024**2)), 1)
        self.assertEqual(smoke.regions(32 * 1024**2)[1], (16 * 1024**2, 65536))

    def test_profile_filename_preserves_duplicate_internal_names(self):
        data = bytearray(192)
        data[:4] = b"Dcfg"
        struct.pack_into("<IIII", data, 8, 176, 8388608, 32, 48)
        data[32:39] = b"Vendor\0"
        data[48:58] = b"duplicate\0"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "configs.tar.xz"
            with tarfile.open(path, "w:xz") as archive:
                for name in ("first", "second"):
                    entry = tarfile.TarInfo(f"configs/{name}.cfg")
                    entry.size = len(data)
                    archive.addfile(entry, io.BytesIO(data))
            profiles = smoke.profiles(path)
            self.assertEqual({p["name"] for p in profiles}, {"first", "second"})
            self.assertEqual({p["reported_name"] for p in profiles}, {"duplicate"})

    def test_cli_programmer_is_always_24mhz(self):
        with tempfile.TemporaryDirectory() as directory:
            args = argparse.Namespace(
                output=Path(directory), programmer_id="SF123", rflasher="rflasher"
            )
            harness = smoke.Harness(args)
            with patch.object(harness, "command", return_value=(0, "")) as command:
                harness.flash(Path(directory), "probe", "probe")
            self.assertIn("dediprog:id=SF123,spispeed=24M", command.call_args.args[2])

    def test_failed_read_blocks_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            args = argparse.Namespace(output=Path(directory), programmer_id="SF123")
            harness = smoke.Harness(args)
            harness.results = [{"name": "part", "phase": "read", "outcome": "fail"}]
            with patch.object(harness, "helper") as helper:
                result = harness.mutate_profile({"name": "part"}, Path(directory))
            self.assertEqual(result["outcome"], "not_run")
            helper.assert_not_called()

    def test_sfdp_density_is_metadata_not_a_geometry_override(self):
        data = bytearray(436)
        data[176:184] = b"SFDPSFDP"
        data[184:188] = bytes([6, 1, 0, 0xFF])
        data[188:196] = bytes([0, 6, 1, 2, 32, 0, 0, 0xFF])
        for density in (0x03FFFFFF, 0x8000001A):
            struct.pack_into("<I", data, 216, density)
            self.assertEqual(smoke.profile_sfdp_size(data), 8 * 1024**2)
        data[192:195] = b"\xff\xff\xff"
        self.assertIsNone(smoke.profile_sfdp_size(data))
        self.assertIsNone(smoke.profile_sfdp_size(data[:190]))
        self.assertIsNone(smoke.profile_sfdp_size(b""))

    def test_process_exit_race_is_reaped(self):
        process = Mock(pid=1234)
        with patch.object(smoke.os, "killpg", side_effect=ProcessLookupError):
            smoke.stop_process_group(process)
        process.wait.assert_called_once_with(timeout=5)

    def test_kill_escalation_also_tolerates_exit_race(self):
        process = Mock(pid=1234)
        process.wait.side_effect = [subprocess.TimeoutExpired("test", 5), 0]
        with patch.object(
            smoke.os, "killpg", side_effect=[None, ProcessLookupError]
        ) as kill:
            smoke.stop_process_group(process)
        self.assertEqual(
            kill.call_args_list,
            [call(1234, signal.SIGTERM), call(1234, signal.SIGKILL)],
        )
        self.assertEqual(process.wait.call_args_list, [call(timeout=5), call()])

    def test_command_interrupt_and_timeout_keep_logs_and_recovery(self):
        for error, code in [
            (KeyboardInterrupt(), 130),
            (subprocess.TimeoutExpired("test", 1), 124),
        ]:
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                args = argparse.Namespace(
                    output=Path(directory), programmer_id="SF123", rflasher="rflasher"
                )
                harness = smoke.Harness(args)
                process = Mock(pid=1234)
                process.wait.side_effect = [error, 0]
                with (
                    patch.object(harness, "check_pico"),
                    patch.object(smoke.subprocess, "Popen", return_value=process),
                    patch.object(smoke.os, "killpg", side_effect=ProcessLookupError),
                ):
                    if code == 130:
                        with self.assertRaises(KeyboardInterrupt):
                            harness.command(Path(directory), "operation", ["rflasher"])
                    else:
                        rc, _ = harness.command(
                            Path(directory), "operation", ["rflasher"]
                        )
                        self.assertEqual(rc, 124)
                self.assertTrue(harness.needs_recovery)
                self.assertIn(
                    f"exit={code}", (Path(directory) / "operation.log").read_text()
                )
                process.wait.assert_has_calls([call(timeout=240), call(timeout=5)])

    def test_cleanup_signal_handlers_are_restored(self):
        previous = {
            sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)
        }
        with (
            self.assertRaisesRegex(RuntimeError, "cleanup error"),
            smoke.cleanup_signals(),
        ):
            for sig in previous:
                self.assertEqual(signal.getsignal(sig), signal.SIG_IGN)
            raise RuntimeError("cleanup error")
        for sig, handler in previous.items():
            self.assertEqual(signal.getsignal(sig), handler)

    def hardware_harness(self, directory):
        args = argparse.Namespace(
            output=Path(directory) / "run",
            programmer_id="SF123",
            em100_serial="TEST",
            phase="read",
            chips=None,
        )
        harness = smoke.Harness(args)
        backup = harness.output / "original-sdram.bin"
        backup.write_bytes(b"x" * 32)
        smoke.atomic_json(
            harness.output / "backup.json", {"sha256": smoke.digest(backup)}
        )
        smoke.atomic_json(
            harness.output / "restoration.json", {"restored": True, "run_id": "stale"}
        )
        harness.check_pico = Mock()
        harness.recover = Mock()
        harness.helper = Mock(return_value=(0, "restored and verified"))
        return harness

    def test_completion_is_published_only_after_fresh_restoration(self):
        with tempfile.TemporaryDirectory() as directory:
            harness = self.hardware_harness(directory)

            def restore(*_):
                state = smoke.parse_json((harness.output / "status.json").read_text())
                cleanup = smoke.parse_json(
                    (harness.output / "restoration.json").read_text()
                )
                self.assertEqual(state["state"], "restoring")
                self.assertFalse(cleanup["restored"])
                self.assertNotEqual(cleanup["run_id"], "stale")
                return 0, "verified"

            harness.helper.side_effect = restore
            with (
                patch.object(Path, "home", return_value=Path(directory)),
                patch.object(smoke, "CAPACITY", 32),
            ):
                harness.run_hardware([])
            state = smoke.parse_json((harness.output / "status.json").read_text())
            cleanup = smoke.parse_json(
                (harness.output / "restoration.json").read_text()
            )
            self.assertEqual(state["state"], "complete")
            self.assertTrue(state["restored"])
            self.assertEqual(state["run_id"], cleanup["run_id"])

    def test_cleanup_failure_cannot_leave_stale_success(self):
        with tempfile.TemporaryDirectory() as directory:
            harness = self.hardware_harness(directory)
            harness.helper.return_value = (1, "device not found")
            with (
                patch.object(Path, "home", return_value=Path(directory)),
                patch.object(smoke, "CAPACITY", 32),
                self.assertRaisesRegex(RuntimeError, "restoration failed"),
            ):
                harness.run_hardware([])
            state = smoke.parse_json((harness.output / "status.json").read_text())
            cleanup = smoke.parse_json(
                (harness.output / "restoration.json").read_text()
            )
            self.assertEqual(state["state"], "cleanup_failed")
            self.assertFalse(state["restored"])
            self.assertFalse(cleanup["restored"])
            self.assertIn("restoration failed", cleanup["error"])

    def test_first_failure_survives_a_second_cleanup_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            harness = self.hardware_harness(directory)
            harness.read_profile = Mock(side_effect=ValueError("first failure"))
            harness.helper.side_effect = RuntimeError("cleanup USB failure")
            profile = {
                "name": "part",
                "vendor": "vendor",
                "size": 32,
                "voltage_mv": 3300,
            }
            with (
                patch.object(Path, "home", return_value=Path(directory)),
                patch.object(smoke, "CAPACITY", 32),
                self.assertRaisesRegex(ValueError, "first failure"),
                patch.object(smoke.sys, "stderr", io.StringIO()),
            ):
                harness.run_hardware([profile])
            state = smoke.parse_json((harness.output / "status.json").read_text())
            self.assertEqual(state["state"], "interrupted")
            self.assertIn("first failure", state["error"])
            self.assertIn("cleanup USB failure", state["cleanup_error"])
            self.assertFalse(state["restored"])

    def test_profile_sfdp_conflict_never_reaches_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            harness = self.hardware_harness(directory)
            harness.read_profile = Mock()
            profile = {
                "name": "part",
                "vendor": "vendor",
                "size": 32,
                "sfdp_size": 16,
                "voltage_mv": 3300,
            }
            with (
                patch.object(Path, "home", return_value=Path(directory)),
                patch.object(smoke, "CAPACITY", 32),
            ):
                harness.run_hardware([profile])
            harness.read_profile.assert_not_called()
            self.assertEqual(harness.results[0]["outcome"], "unresolved")
            self.assertEqual(
                harness.results[0]["reason"], "profile_sfdp_density_differs"
            )

    def test_detected_geometry_conflict_is_unresolved_not_data_corruption(self):
        with tempfile.TemporaryDirectory() as directory:
            harness = self.hardware_harness(directory)
            harness.flash = Mock(side_effect=[(0, "probe"), (0, "Size: 64 bytes")])
            profile = {
                "name": "part",
                "size": 32,
                "selected_chip": None,
                "config_hex": "",
            }
            result = harness.read_profile(profile, harness.output)
            self.assertEqual(result["outcome"], "unresolved")
            self.assertEqual(result["detected_size"], 64)
            self.assertEqual(harness.flash.call_count, 2)

    def test_hash_is_full_file(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "data"
            path.write_bytes(b"known bytes")
            self.assertEqual(
                smoke.digest(path), hashlib.sha256(b"known bytes").hexdigest()
            )


if __name__ == "__main__":
    unittest.main()

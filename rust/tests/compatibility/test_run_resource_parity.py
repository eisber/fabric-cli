# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Fail-closed runner guards, independent of existing source expectations."""

from __future__ import annotations

import importlib.util
import io
import os
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path
from typing import Any
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "resource_parity_runner", Path(__file__).with_name("run_resource_parity.py")
)
if SPEC is None or SPEC.loader is None:
    raise AssertionError("Resource parity runner is unavailable")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


def archive(name: str = RUNNER.PAYLOADS + "/template.json") -> bytes:
    """Provide a private synthetic archive; product templates are not replaced."""
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as payloads:
        payloads.writestr(name, b"{}")
    return output.getvalue()


class ResourceParityRunnerTests(unittest.TestCase):
    """Every released comparison and actual probe is mandatory."""

    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.probes = self.root / "native probes"
        self.probes.mkdir()
        self.artifacts = self.root / "observations"
        suffix = ".exe" if os.name == "nt" else ""
        for _, _, name, _ in RUNNER.COMPARISONS:
            (self.probes / f"{name}{suffix}").touch()

    def process(self, arguments: list[str], **options: Any) -> subprocess.CompletedProcess:
        if arguments[0] == "git":
            return subprocess.CompletedProcess(arguments, 0, stdout=archive())
        if "--source-export" in arguments:
            exported = Path(arguments[-1])
            self.assertEqual(
                (exported / RUNNER.PAYLOADS / "template.json").read_bytes(), b"{}"
            )
        return subprocess.CompletedProcess(arguments, 0)

    def test_all_fifteen_comparisons_use_exact_source_and_native_probes(self) -> None:
        with patch.object(RUNNER.subprocess, "run", side_effect=self.process) as invoked:
            RUNNER.run(self.probes, self.artifacts)
        calls = invoked.call_args_list
        self.assertEqual(len(calls), 16)
        self.assertEqual(
            calls[0].args[0],
            ["git", "archive", "--format=zip", RUNNER.SOURCE, RUNNER.PAYLOADS],
        )
        expected = {
            "python_reference.py", "operation_reference.py", "error_reference.py",
            "onelake_reference.py", "onelake_write_reference.py", "shortcut_reference.py",
            "removal_reference.py", "public_set_reference.py", "copy_preflight_reference.py",
            "metadata_set_reference.py", "creation_reference.py", "dynamic_creation_reference.py",
            "creation_policy_reference.py", "creation_value_reference.py",
            "continuation_reference.py",
        }
        self.assertEqual({Path(call.args[0][2]).name for call in calls[1:]}, expected)
        self.assertEqual(
            sum("--source-export" in call.args[0] for call in calls[1:]), 4
        )
        for call in calls:
            self.assertTrue(call.kwargs["check"])
            self.assertEqual(call.kwargs["timeout"], RUNNER.TIMEOUT_SECONDS)
            self.assertEqual(call.kwargs["env"]["PYTHONUTF8"], "1")
        self.assertEqual(calls[0].kwargs["cwd"], RUNNER.REPOSITORY)
        for call in calls[1:]:
            self.assertEqual(call.kwargs["cwd"], RUNNER.WORKSPACE)
            probe = Path(call.args[0][4])
            self.assertEqual(probe.parent, self.probes.resolve())
        for call, (package, reference, _, _) in zip(calls[1:], RUNNER.COMPARISONS):
            self.assertEqual(
                Path(call.args[0][2]),
                RUNNER.REFERENCE_DIRECTORIES[package] / "tests" / reference,
            )
            self.assertTrue(Path(call.args[0][2]).is_file())
        self.assertEqual(list(self.artifacts.iterdir()), [])

    def test_every_failed_stage_stops_without_fallback(self) -> None:
        for failed in range(16):
            with self.subTest(stage=failed):
                count = 0

                def process(arguments: list[str], **options: Any) -> subprocess.CompletedProcess:
                    nonlocal count
                    index = count
                    count += 1
                    if index == failed:
                        raise subprocess.CalledProcessError(9, arguments)
                    return self.process(arguments, **options)

                with patch.object(RUNNER.subprocess, "run", side_effect=process):
                    with self.assertRaises(subprocess.CalledProcessError):
                        RUNNER.run(self.probes, self.artifacts)
                self.assertEqual(count, failed + 1)

    def test_missing_probe_prevents_source_export_or_comparison(self) -> None:
        next(self.probes.iterdir()).unlink()
        with patch.object(RUNNER.subprocess, "run") as invoked:
            with self.assertRaises(FileNotFoundError):
                RUNNER.run(self.probes, self.artifacts)
            invoked.assert_not_called()

    def test_timeout_and_invalid_archive_are_failures_not_skipped_stages(self) -> None:
        failures = [
            subprocess.TimeoutExpired(["git"], RUNNER.TIMEOUT_SECONDS),
            subprocess.CompletedProcess(["git"], 0, stdout=b"invalid ZIP"),
        ]
        for failure in failures:
            with self.subTest(failure=type(failure).__name__):
                with patch.object(RUNNER.subprocess, "run", side_effect=[failure]) as invoked:
                    with self.assertRaises((subprocess.TimeoutExpired, zipfile.BadZipFile)):
                        RUNNER.run(self.probes, self.artifacts)
                    self.assertEqual(invoked.call_count, 1)

    def test_source_archive_cannot_escape_its_pinned_payload_directory(self) -> None:
        for name in ("../escape", "/absolute", "src/unrelated", "src\\escape"):
            with self.subTest(name=name):
                result = subprocess.CompletedProcess(["git"], 0, stdout=archive(name))
                with patch.object(RUNNER.subprocess, "run", return_value=result) as invoked:
                    with self.assertRaises(ValueError):
                        RUNNER.run(self.probes, self.artifacts)
                    self.assertEqual(invoked.call_count, 1)


if __name__ == "__main__":
    unittest.main()

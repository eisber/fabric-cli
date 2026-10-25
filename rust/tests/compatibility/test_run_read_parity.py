# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Guard fail-closed orchestration separately from immutable oracle expectations."""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "read_parity_runner", Path(__file__).with_name("run_read_parity.py")
)
if SPEC is None or SPEC.loader is None:
    raise AssertionError("Read parity runner is unavailable")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class ReadParityRunnerTests(unittest.TestCase):
    """Require native observations, all guards and explicit process failures."""

    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.read_probe = self.root / "read probe.exe"
        self.query_probe = self.root / "query probe.exe"
        self.read_probe.touch()
        self.query_probe.touch()
        self.artifacts = self.root / "observations"

    def execute(self) -> None:
        RUNNER.run(self.read_probe, self.query_probe, self.artifacts)

    def test_all_current_comparisons_and_guards_are_required(self) -> None:
        def process(arguments: list[str], **options: Any) -> None:
            if arguments == [str(self.query_probe)]:
                options["stdout"].write(b"[]")

        with patch.object(RUNNER.subprocess, "run", side_effect=process) as invoked:
            self.execute()
        calls = invoked.call_args_list
        self.assertEqual(len(calls), 6)
        self.assertEqual(
            calls[0].args[0],
            [
                sys.executable,
                "-B",
                str(RUNNER.HERE / "commands" / "reference.py"),
                "--rust-probe",
                str(self.read_probe),
            ],
        )
        self.assertIn("metadata_reference.py", calls[1].args[0][2])
        self.assertEqual(calls[2].args[0][-3:], ["-p", "test_*.py", "-v"])
        self.assertEqual(calls[3].args[0], [str(self.query_probe)])
        self.assertIn("reference.py", calls[4].args[0][2])
        self.assertEqual(calls[5].args[0][-3:], ["-p", "test_reference.py", "-v"])
        for call in calls:
            self.assertTrue(call.kwargs["check"])
            self.assertEqual(call.kwargs["timeout"], RUNNER.TIMEOUT_SECONDS)
            self.assertEqual(call.kwargs["cwd"], RUNNER.WORKSPACE)
            self.assertEqual(call.kwargs["env"]["PYTHONUTF8"], "1")
            self.assertNotIn("--get-mode", call.args[0])
        self.assertEqual(
            (self.artifacts / "query-observations.json").read_bytes(), b"[]"
        )

    def test_every_failed_stage_stops_without_a_fallback(self) -> None:
        for failed_stage in range(6):
            with self.subTest(stage=failed_stage):
                outcomes = [None] * failed_stage + [
                    subprocess.CalledProcessError(9, ["failed-stage"])
                ]
                with patch.object(
                    RUNNER.subprocess, "run", side_effect=outcomes
                ) as invoked:
                    with self.assertRaises(subprocess.CalledProcessError) as error:
                        self.execute()
                self.assertEqual(error.exception.returncode, 9)
                self.assertEqual(invoked.call_count, failed_stage + 1)

    def test_timeout_is_not_a_skipped_or_successful_comparison(self) -> None:
        with patch.object(
            RUNNER.subprocess,
            "run",
            side_effect=subprocess.TimeoutExpired(["probe"], RUNNER.TIMEOUT_SECONDS),
        ) as invoked:
            with self.assertRaises(subprocess.TimeoutExpired):
                self.execute()
        self.assertEqual(invoked.call_count, 1)

    def test_missing_probe_is_rejected_before_running_comparisons(self) -> None:
        self.read_probe.unlink()
        with patch.object(RUNNER.subprocess, "run") as invoked:
            with self.assertRaises(FileNotFoundError):
                self.execute()
        invoked.assert_not_called()

    def test_artifact_write_failure_does_not_allow_query_comparison(self) -> None:
        with patch.object(RUNNER.subprocess, "run") as invoked:
            with patch.object(Path, "open", side_effect=OSError("write failure")):
                with self.assertRaisesRegex(OSError, "write failure"):
                    self.execute()
        self.assertEqual(invoked.call_count, 3)


if __name__ == "__main__":
    unittest.main()

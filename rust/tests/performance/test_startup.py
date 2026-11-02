# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Small synthetic contract tests, not Rust/Python performance measurements."""

from __future__ import annotations

from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from typing import Any

import startup


class StartupContracts(unittest.TestCase):
    """Keep process failures, measurement units, and evidence boundaries explicit."""

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.template = json.loads(
            Path(__file__)
            .with_name("measurement-request.json")
            .read_text(encoding="utf-8")
        )
        self.target = startup.load_request(self.template).targets[0]
        self.target = replace(
            self.target,
            executable=sys.executable,
            contracts={
                flag: startup.OutputContract(hashlib.sha256(b"ok\n").hexdigest(), 3)
                for flag in startup.FLAGS
            },
        )

    def observe_code(
        self, code: str, *, timeout: float = 5, memory: bool = False
    ) -> startup.Observation:
        """Use a tiny real subprocess, never product or credential discovery."""
        popen = subprocess.Popen

        def spawn(command: list[str], **kwargs: Any) -> subprocess.Popen[bytes]:
            code_with_lf = "import sys; sys.stdout.reconfigure(newline='\\n'); " + code
            return popen([sys.executable, "-c", code_with_lf], **kwargs)

        with patch.object(startup.subprocess, "Popen", side_effect=spawn):
            return startup.observe(
                self.target,
                "--help",
                "latency",
                0,
                timeout,
                self.directory,
                startup.isolated_environment(self.directory),
                memory=memory,
            )

    def test_pending_template_validates_but_cannot_execute(self) -> None:
        request = startup.load_request(self.template)
        self.assertEqual(request.samples, 30)
        with self.assertRaisesRegex(startup.BenchmarkError, "pending"):
            startup.preflight(request)

    def test_ready_template_rejects_placeholder_evidence(self) -> None:
        self.template["status"] = "ready"
        with self.assertRaisesRegex(startup.BenchmarkError, "pending evidence"):
            startup.load_request(self.template)

    def test_counts_time_bounds_and_schema_reject_invalid_values(self) -> None:
        for field, value in (
            ("samples", 29),
            ("samples", True),
            ("samples", 30.0),
            ("warmups", 0),
            ("warmups", 1),
            ("warmups", 2),
            ("warmups", 11),
            ("job_timeout_seconds", 601),
            ("samples", 10**400),
            ("process_timeout_seconds", float("nan")),
            ("process_timeout_seconds", 31),
            ("schema_version", True),
            ("endpoint_host", "https://example.invalid"),
        ):
            with self.subTest(field=field, value=value):
                data = dict(self.template, **{field: value})
                with self.assertRaises(startup.BenchmarkError):
                    startup.load_request(data)
        with self.assertRaises(startup.BenchmarkError):
            startup.load_request(dict(self.template, command=["auth", "login"]))

    def test_rejects_duplicate_targets_and_unpinned_contracts(self) -> None:
        self.template["targets"][1]["name"] = "rust"
        with self.assertRaises(startup.BenchmarkError):
            startup.load_request(self.template)
        self.template["targets"][1]["name"] = "python"
        self.template["targets"][0]["contracts"]["--help"]["sha256"] = "not-a-hash"
        with self.assertRaises(startup.BenchmarkError):
            startup.load_request(self.template)

    def test_source_pins_must_match(self) -> None:
        self.template["source_sha"] = "f" * 40
        with self.assertRaisesRegex(startup.BenchmarkError, "source"):
            startup.load_request(self.template)

    def test_summary_nearest_rank_p95_and_all_outliers(self) -> None:
        values = [float(value) for value in range(1, 31)]
        values[-1] = 10000
        result = startup.summary(values)
        self.assertEqual(
            result,
            {"samples": 30, "median": 15.5, "p95": 29.0, "min": 1.0, "max": 10000},
        )
        for invalid in ([], [-1.0], [float("inf")], [float("nan")]):
            with self.assertRaises(startup.BenchmarkError):
                startup.summary(invalid)

    def test_time_parser_uses_rss_not_vsz_and_rejects_missing_or_duplicate(
        self,
    ) -> None:
        text = "\tMaximum resident set size (kbytes): 12345\n"
        self.assertEqual(startup.parse_peak_rss(text), 12345)
        for invalid in (
            "",
            "Virtual memory: 12345",
            text + text,
            text.replace("12345", "0"),
        ):
            with self.assertRaises(startup.BenchmarkError):
                startup.parse_peak_rss(invalid)

    def test_new_process_succeeds_and_wall_unit_is_milliseconds(self) -> None:
        with patch.object(startup.time, "perf_counter_ns", side_effect=[1000, 2501000]):
            row = self.observe_code("print('ok')")
        self.assertTrue(row.valid)
        self.assertEqual(row.exit_code, 0)
        self.assertEqual(row.stdout_bytes, 3)
        self.assertEqual(row.wall_ms, 2.5)
        self.assertIsNone(row.peak_rss_kib)

    def test_nonzero_exit_is_not_a_successful_sample(self) -> None:
        row = self.observe_code("print('ok'); raise SystemExit(7)")
        self.assertFalse(row.valid)
        self.assertEqual(row.exit_code, 7)
        self.assertEqual(row.failure, "nonzero_exit")

    def test_wrong_bytes_or_stderr_fail(self) -> None:
        for code, failure in (
            ("print('no')", "invalid_stdout"),
            ("print('longer')", "invalid_stdout"),
            ("import sys; print('ok'); sys.stderr.write('error')", "unexpected_stderr"),
        ):
            with self.subTest(failure=failure, code=code):
                self.assertEqual(self.observe_code(code).failure, failure)

    def test_timeout_is_recorded_and_child_is_reaped(self) -> None:
        row = self.observe_code("import time; time.sleep(30)", timeout=0.1)
        self.assertFalse(row.valid)
        self.assertEqual(row.failure, "timeout")
        self.assertIsNotNone(row.exit_code)
        self.assertLess(row.wall_ms, 10000)

    def test_launch_failure_is_recorded_without_raw_os_error(self) -> None:
        with patch.object(startup.subprocess, "Popen", side_effect=OSError("private")):
            row = self.observe_code("print('ok')")
        self.assertEqual(row.failure, "process_start_failed")
        self.assertIsNone(row.exit_code)

    def test_live_or_mutating_flags_are_never_dispatched(self) -> None:
        with patch.object(startup.subprocess, "Popen") as process:
            with self.assertRaises(startup.BenchmarkError):
                startup.observe(
                    self.target, "auth", "latency", 0, 1, self.directory, {}
                )
            process.assert_not_called()

    def test_missing_memory_report_cannot_be_a_success(self) -> None:
        row = self.observe_code("print('ok')", memory=True)
        self.assertEqual(row.failure, "invalid_peak_rss")

    def test_memory_wrapper_is_separate_and_report_resets_per_child(self) -> None:
        commands = []
        popen = subprocess.Popen

        def spawn(command: list[str], **kwargs: Any) -> subprocess.Popen[bytes]:
            commands.append(command)
            report = Path(command[command.index("-o") + 1])
            self.assertFalse(report.exists())
            report.write_text(
                "\tMaximum resident set size (kbytes): 2048\n", encoding="utf-8"
            )
            return popen(
                [sys.executable, "-c", "import sys; sys.stdout.buffer.write(b'ok\\n')"],
                **kwargs,
            )

        for index in range(2):
            with patch.object(startup.subprocess, "Popen", side_effect=spawn):
                row = startup.observe(
                    self.target,
                    "--version",
                    "memory",
                    index,
                    5,
                    self.directory,
                    startup.isolated_environment(self.directory),
                    memory=True,
                )
            self.assertTrue(row.valid)
            self.assertEqual(row.peak_rss_kib, 2048)
        self.assertEqual(commands[0][:2], ["/usr/bin/time", "-v"])
        self.assertEqual(commands[0][-2:], [sys.executable, "--version"])

    def test_preflight_checks_hash_release_package_mode_and_direct_entrypoint(
        self,
    ) -> None:
        request = startup.load_request(self.template)
        targets = []
        for target in request.targets:
            executable = self.directory / ("fab-rs" if target.name == "rust" else "fab")
            executable.write_bytes(b"synthetic executable")
            targets.append(
                replace(
                    target,
                    executable=str(executable),
                    executable_sha256=startup.file_sha256(executable),
                )
            )
        request = replace(request, status="ready", targets=targets)
        with (
            patch.object(startup.sys, "platform", "linux"),
            patch.object(startup.Path, "is_file", return_value=True),
            patch.object(startup.os, "access", return_value=True),
        ):
            startup.preflight(request)
            rust = next(target for target in targets if target.name == "rust")
            python = next(target for target in targets if target.name == "python")
            for invalid in (
                replace(rust, package_mode="cargo-debug"),
                replace(python, package_mode="editable"),
                replace(rust, executable_sha256="f" * 64),
                replace(python, executable=str(self.directory / "python")),
            ):
                with self.subTest(target=invalid):
                    changed = [
                        invalid if target.name == invalid.name else target
                        for target in targets
                    ]
                    with self.assertRaises(startup.BenchmarkError):
                        startup.preflight(replace(request, targets=changed))

    def test_environment_excludes_ambient_credentials_and_dev_imports(self) -> None:
        with patch.dict(
            os.environ,
            {
                "FAB_TOKEN": "synthetic",
                "PYTHONPATH": "synthetic",
                "HTTPS_PROXY": "synthetic",
                "VIRTUAL_ENV": "synthetic",
            },
        ):
            environment = startup.isolated_environment(self.directory)
        for key in ("FAB_TOKEN", "PYTHONPATH", "HTTPS_PROXY", "VIRTUAL_ENV"):
            self.assertNotIn(key, environment)
        self.assertEqual(environment["HOME"], str(self.directory))
        self.assertEqual(environment["PYTHONUTF8"], "1")

    def test_runner_separates_first_warmup_latency_and_memory_samples(self) -> None:
        request = startup.load_request(self.template)
        calls = []

        def observe(
            target: startup.Target,
            flag: str,
            phase: str,
            index: int,
            timeout: float,
            directory: Path,
            environment: dict[str, str],
            memory: bool = False,
        ) -> startup.Observation:
            calls.append((target.name, flag, phase, index, memory, timeout))
            return startup.Observation(
                target.name,
                flag,
                phase,
                index,
                2.0,
                0,
                3,
                0,
                "digest",
                True,
                None,
                1024 if memory else None,
            )

        receipt: dict[str, Any] = {}
        with (
            patch.object(startup, "preflight"),
            patch.object(startup, "observe", side_effect=observe),
        ):
            startup.run(request, receipt)
        self.assertEqual(receipt["status"], "passed")
        self.assertEqual(len(calls), 4 * (1 + 3 + 30 + 3 + 30))
        for case in receipt["summaries"]:
            self.assertEqual(case["wall_ms"]["samples"], 30)
            self.assertEqual(case["peak_rss_kib"]["samples"], 30)
            self.assertEqual(case["peak_rss_kib"]["median"], 1024)
        latency_calls = [row for row in calls if row[2] == "latency"]
        self.assertEqual(
            [row[0] for row in latency_calls[:4]], ["python", "rust", "rust", "python"]
        )

    def test_failed_or_deadline_exhausted_run_has_no_partial_summary(self) -> None:
        request = startup.load_request(self.template)
        invalid = startup.Observation(
            "python",
            "--help",
            "first_observed_case",
            0,
            2,
            1,
            0,
            1,
            "",
            False,
            "nonzero_exit",
        )
        receipt: dict[str, Any] = {}
        with (
            patch.object(startup, "preflight"),
            patch.object(startup, "observe", return_value=invalid),
        ):
            with self.assertRaises(startup.BenchmarkError):
                startup.run(request, receipt)
        self.assertEqual(len(receipt["observations"]), 1)
        self.assertNotIn("summaries", receipt)
        receipt = {}
        with (
            patch.object(startup, "preflight"),
            patch.object(startup.time, "monotonic", side_effect=[0, 601]),
            patch.object(startup, "observe") as observe,
        ):
            with self.assertRaisesRegex(startup.BenchmarkError, "deadline"):
                startup.run(request, receipt)
            observe.assert_not_called()
        self.assertNotIn("summaries", receipt)

    def test_cli_writes_failed_receipt_and_never_overwrites_evidence(self) -> None:
        request = self.directory / "request.json"
        request.write_text(json.dumps(self.template), encoding="utf-8")
        output = self.directory / "receipt.json"
        args = ["--request", str(request), "--output", str(output)]
        self.assertEqual(startup.main(args), 1)
        receipt = json.loads(output.read_text(encoding="utf-8"))
        self.assertEqual(receipt["status"], "failed")
        self.assertEqual(receipt["units"], {"wall": "ms", "peak_rss": "KiB"})
        self.assertNotIn("summaries", receipt)
        with self.assertRaises(FileExistsError):
            startup.main(args)
        output.unlink()
        with patch.object(startup, "run") as run:
            self.assertEqual(startup.main([*args, "--validate-only"]), 0)
            run.assert_not_called()
        self.assertEqual(
            json.loads(output.read_text(encoding="utf-8"))["status"],
            "validated_only",
        )

    def test_invalid_json_produces_a_failed_receipt(self) -> None:
        request = self.directory / "bad.json"
        request.write_text("{", encoding="utf-8")
        output = self.directory / "receipt.json"
        self.assertEqual(
            startup.main(["--request", str(request), "--output", str(output)]), 1
        )
        self.assertEqual(
            json.loads(output.read_text(encoding="utf-8"))["error"],
            "JSONDecodeError",
        )


if __name__ == "__main__":
    unittest.main()

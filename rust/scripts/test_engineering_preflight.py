# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline tests of the exact-inventory engineering receipt helper."""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import sys
import unittest
import uuid
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import Mock, patch

import engineering_preflight as preflight


class EngineeringPreflightTests(unittest.TestCase):
    def setUp(self) -> None:
        self.source = Path(__file__).resolve().parent
        self.artifacts = (
            self.source.parent / "target" / f"engineering-preflight-{uuid.uuid4().hex}"
        )
        self.addCleanup(lambda: shutil.rmtree(self.artifacts, ignore_errors=True))
        self.host = preflight._HOSTS[sys.platform]

    def _run(
        self, *, discovered: int = preflight._COUNT, result: Any = None
    ) -> tuple[int, dict[str, Any]]:
        suite = Mock()
        suite.countTestCases.return_value = discovered
        if result is None:
            skips = (
                [
                    (
                        SimpleNamespace(id=lambda name=name: name),
                        reason,
                    )
                    for name, reason in preflight._WINDOWS_SKIPS
                ]
                if self.host == "windows"
                else []
            )
            result = SimpleNamespace(
                testsRun=preflight._COUNT,
                failures=[],
                errors=[],
                expectedFailures=[],
                skipped=skips,
                wasSuccessful=lambda: True,
            )
        acceptance = SimpleNamespace(
            execute=lambda *args, **kwargs: ({"pty": True}, "captured"), os=os
        )

        def execute_suite(*args: Any) -> Any:
            acceptance.execute(terminal={"kind": "fixture"})
            return result

        with (
            patch.object(
                preflight.unittest.defaultTestLoader, "discover", return_value=suite
            ),
            patch.object(preflight.importlib, "import_module", return_value=acceptance),
            patch.object(
                preflight.unittest.TextTestRunner, "run", side_effect=execute_suite
            ),
            contextlib.redirect_stdout(io.StringIO()),
        ):
            status = preflight._preflight(
                self.source, self.artifacts, "a" * 40, self.host, "contracts-oracle"
            )
        report = json.loads(
            (self.artifacts / "engineering-receipt.json").read_text(encoding="utf-8")
        )
        return status, report

    def test_exact_inventory_retains_scope_log_and_pty_receipts(self) -> None:
        previous = Path.cwd()
        status, report = self._run()
        self.assertEqual(status, 0)
        self.assertEqual(Path.cwd(), previous)
        self.assertEqual(report["head"], "a" * 40)
        self.assertEqual(report["platform"], self.host)
        self.assertEqual(report["shard"], "contracts-oracle")
        self.assertTrue(report["passed"])
        self.assertEqual(report["suites"][0]["run"], preflight._COUNT)
        self.assertEqual(report["pty_receipts"], [{"pty": True}])
        self.assertTrue((self.artifacts / "native-acceptance.log").is_file())

    def test_inventory_mismatch_persists_failed_receipt_without_running_tests(
        self,
    ) -> None:
        status, report = self._run(discovered=preflight._COUNT - 1)
        self.assertEqual(status, 1)
        self.assertFalse(report["passed"])
        self.assertEqual(report["suites"][0]["reason"], "inventory_mismatch")
        self.assertEqual(report["suites"][0]["run"], 0)
        self.assertFalse((self.artifacts / "native-acceptance.log").exists())

    def test_failed_partial_expected_failure_and_unknown_skips_fail_closed(
        self,
    ) -> None:
        cases: list[dict[str, Any]] = [
            {"wasSuccessful": lambda: False, "failures": ["failure"]},
            {"testsRun": preflight._COUNT - 1},
            {"expectedFailures": ["unexpected"]},
            {
                "skipped": [
                    (SimpleNamespace(id=lambda: "unapproved.test"), "unapproved skip")
                ]
            },
        ]
        for overrides in cases:
            with self.subTest(overrides=overrides):
                shutil.rmtree(self.artifacts, ignore_errors=True)
                result = SimpleNamespace(
                    testsRun=preflight._COUNT,
                    failures=[],
                    errors=[],
                    expectedFailures=[],
                    skipped=[],
                    wasSuccessful=lambda: True,
                )
                result.__dict__.update(overrides)
                status, report = self._run(result=result)
                self.assertEqual(status, 1)
                self.assertFalse(report["passed"])

    def test_existing_artifacts_and_invalid_scope_are_rejected(self) -> None:
        self.artifacts.mkdir(parents=True)
        with self.assertRaises(FileExistsError):
            preflight._preflight(
                self.source, self.artifacts, "a" * 40, self.host, "contracts"
            )
        for head, host, shard in [
            ("short", self.host, "contracts"),
            ("a" * 40, "not-native", "contracts"),
            ("a" * 40, self.host, "../escape"),
        ]:
            with self.subTest(head=head, host=host, shard=shard):
                with self.assertRaises(ValueError):
                    preflight._preflight(self.source, self.artifacts, head, host, shard)
        with self.assertRaises(ValueError):
            preflight._preflight(
                self.source, self.source / "artifacts", "a" * 40, self.host, "contracts"
            )


if __name__ == "__main__":
    unittest.main()

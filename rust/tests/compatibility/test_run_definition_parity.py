# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Fail-closed orchestration guards, separate from released definition receipts."""

from __future__ import annotations

import base64
import copy
import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "definition_parity_runner", Path(__file__).with_name("run_definition_parity.py")
)
if SPEC is None or SPEC.loader is None:
    raise AssertionError("Definition source runner is unavailable")
RUNNER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNNER)


class DefinitionParityRunnerTests(unittest.TestCase):
    """No capture, source pin, request ordering or failure can be skipped."""

    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.tests = self.root / "tests"
        self.tests.mkdir()
        self.artifacts = self.root / "observations"
        for family, fixture, _ in RUNNER.COMPARISONS:
            (self.tests / f"definition_{family}_reference.py").touch()
            receipt = {"source": RUNNER.SOURCE, "cases": [{"body": "exact bytes"}]}
            if family == "export":
                receipt["cases"] = []
                receipt["platform"] = "win32"
            (self.tests / fixture).write_text(json.dumps(receipt), encoding="utf-8")
        self.patch = patch.object(RUNNER, "TESTS", self.tests)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def process(self, arguments: list[str], **options: Any) -> subprocess.CompletedProcess:
        family = Path(arguments[2]).stem.removeprefix("definition_").removesuffix("_reference")
        fixture = next(name for name_family, name, _ in RUNNER.COMPARISONS if name_family == family)
        receipt = json.loads((self.tests / fixture).read_text(encoding="utf-8"))
        if family == "export":
            receipt["platform"] = RUNNER.sys.platform
        Path(arguments[3]).write_text(json.dumps(receipt), encoding="utf-8")
        return subprocess.CompletedProcess(arguments, 0)

    def test_all_four_captures_are_required_with_timeouts_and_private_outputs(self) -> None:
        with patch.object(RUNNER.subprocess, "run", side_effect=self.process) as invoked:
            RUNNER.run(self.artifacts)
        self.assertEqual(invoked.call_count, 4)
        for call, (family, _, local_files) in zip(invoked.call_args_list, RUNNER.COMPARISONS):
            self.assertEqual(Path(call.args[0][2]).name, f"definition_{family}_reference.py")
            self.assertTrue(call.kwargs["check"])
            self.assertEqual(call.kwargs["timeout"], RUNNER.TIMEOUT_SECONDS)
            self.assertEqual(call.kwargs["cwd"], RUNNER.WORKSPACE)
            self.assertEqual(call.kwargs["env"]["PYTHONUTF8"], "1")
            self.assertEqual(call.args[0][1], "-B")
            self.assertEqual(len(call.args[0]), 5 if local_files else 4)
            self.assertEqual(Path(call.args[0][3]).parent.parent, self.artifacts)
        self.assertEqual(list(self.artifacts.iterdir()), [])

    def test_each_failure_and_timeout_stops_without_fallback(self) -> None:
        for failed in range(4):
            for exception in (
                subprocess.CalledProcessError(9, ["source"]),
                subprocess.TimeoutExpired(["source"], RUNNER.TIMEOUT_SECONDS),
            ):
                with self.subTest(stage=failed, error=type(exception).__name__):
                    count = 0

                    def process(arguments: list[str], **options: Any) -> subprocess.CompletedProcess:
                        nonlocal count
                        index = count
                        count += 1
                        if index == failed:
                            raise exception
                        return self.process(arguments, **options)

                    with patch.object(RUNNER.subprocess, "run", side_effect=process):
                        with self.assertRaises(type(exception)):
                            RUNNER.run(self.artifacts)
                    self.assertEqual(count, failed + 1)

    def test_missing_input_prevents_any_capture(self) -> None:
        (self.tests / "definition_bulk_reference.py").unlink()
        with patch.object(RUNNER.subprocess, "run") as invoked:
            with self.assertRaises(FileNotFoundError):
                RUNNER.run(self.artifacts)
            invoked.assert_not_called()

    def test_missing_or_malformed_observation_is_failure(self) -> None:
        for content in (None, "invalid JSON"):
            with self.subTest(content=content):
                def process(arguments: list[str], **options: Any) -> subprocess.CompletedProcess:
                    if content is not None:
                        Path(arguments[3]).write_text(content, encoding="utf-8")
                    return subprocess.CompletedProcess(arguments, 0)

                with patch.object(RUNNER.subprocess, "run", side_effect=process) as invoked:
                    with self.assertRaises((FileNotFoundError, json.JSONDecodeError)):
                        RUNNER.run(self.artifacts)
                    self.assertEqual(invoked.call_count, 1)

    def test_source_pin_request_bytes_and_array_object_order_are_not_normalized(self) -> None:
        expected = {"source": RUNNER.SOURCE, "cases": [{"body": '{"a": 1}'}, {"b": 2, "a": 1}]}
        variants = []
        for source in ("newer-main", None):
            actual = copy.deepcopy(expected)
            actual["source"] = source
            variants.append(actual)
        actual = copy.deepcopy(expected)
        actual["cases"].reverse()
        variants.append(actual)
        actual = copy.deepcopy(expected)
        actual["cases"][0]["body"] = '{"a":1}'
        variants.append(actual)
        actual = copy.deepcopy(expected)
        actual["cases"][1] = {"a": 1, "b": 2}
        variants.append(actual)
        for actual in variants:
            with self.subTest(actual=actual):
                with self.assertRaises(AssertionError):
                    RUNNER.compare("set", actual, expected)
        RUNNER.compare("set", expected, expected)

    def test_only_export_text_newlines_use_host_translation_never_binary_or_observations(self) -> None:
        encode = lambda value: base64.b64encode(value).decode("ascii")
        expected = {
            "source": RUNNER.SOURCE,
            "platform": "win32",
            "cases": [
                {"name": "text", "expected_base64": encode(b"a\r\r\nb\r\n")},
                {"name": "binary", "expected_base64": encode(b"\xff\r\n")},
            ],
        }
        original = copy.deepcopy(expected)
        actual = copy.deepcopy(expected)
        actual["platform"] = "linux"
        actual["cases"][0]["expected_base64"] = encode(b"a\r\nb\n")
        with patch.object(RUNNER.os, "name", "posix"), patch.object(RUNNER.sys, "platform", "linux"):
            RUNNER.compare("export", actual, expected)
            corrupted = copy.deepcopy(actual)
            corrupted["cases"][1]["expected_base64"] = encode(b"\xff\n")
            with self.assertRaises(AssertionError):
                RUNNER.compare("export", corrupted, expected)
            with self.assertRaises(AssertionError):
                RUNNER.compare("export", expected, expected)
        self.assertEqual(expected, original)


if __name__ == "__main__":
    unittest.main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independent source, inventory, drift, I/O and anti-replay controls."""

from __future__ import annotations

import builtins
import copy
import importlib.util
import io
import json
import socket
import subprocess
import sys
import time
import unittest
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "_independent_query_argument_reference", HERE / "reference.py"
)
if SPEC is None or SPEC.loader is None:
    raise AssertionError("Argument reference unavailable")
REFERENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REFERENCE)


class ArgumentReferenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.cases = REFERENCE.load_cases()
        cls.reference = REFERENCE.Reference()
        cls.raw = (HERE / "cases.json").read_bytes()

    def observations(self) -> list[dict]:
        """Create independent source observations for comparator controls only."""
        return [
            {"id": case["id"], "parse": self.reference.observe(case["argv"])}
            for case in self.cases
        ]

    def test_all_frozen_expectations_match_actual_source_repeatably(self) -> None:
        before = copy.deepcopy(self.cases)
        first = self.observations()
        self.assertEqual(first, self.observations())
        self.assertEqual(REFERENCE.compare(self.cases, first), [])
        self.assertEqual(before, self.cases)
        self.assertEqual(len(first), 60)

    def test_scalar_vs_list_and_query_presence_are_measured_before_join(self) -> None:
        scalar = self.reference.arguments(["ls", "-q", ""])
        tokens = self.reference.arguments(["get", ".", "-q", ""])
        self.assertEqual(scalar.query, "")
        self.assertFalse(scalar.query)
        self.assertEqual(tokens.query, [""])
        self.assertTrue(tokens.query)
        self.assertIsNone(self.reference.arguments(["ls"]).query)
        self.assertIsNone(self.reference.arguments(["get", "."]).query)

    def test_source_blob_drift_fails_before_execution(self) -> None:
        for relative in REFERENCE.BLOBS:
            with self.subTest(source=relative):
                with self.assertRaisesRegex(AssertionError, "Source drift"):
                    REFERENCE.source_tree(relative, b"def replacement(): pass\n")

    def test_full_source_tree_drift_is_not_bypassed(self) -> None:
        with patch.object(
            REFERENCE.COMMANDS,
            "verify_source",
            side_effect=AssertionError("protected tree drift"),
        ):
            with self.assertRaisesRegex(AssertionError, "protected tree drift"):
                REFERENCE.Reference()

    def test_fixture_corruption_cannot_be_frozen_from_native_results(self) -> None:
        changed = json.loads(self.raw)
        changed["cases"][2]["read"]["query"] = "replayed"
        with self.assertRaisesRegex(AssertionError, "Immutable"):
            REFERENCE.validate_manifest(changed, json.dumps(changed).encode())
        with self.assertRaisesRegex(AssertionError, "protected bytes"):
            REFERENCE.validate_manifest(changed, self.raw)

    def test_consumer_output_inventory_and_input_corruption_fail(self) -> None:
        manifest = REFERENCE.load_consumers()
        for operation in ("result", "missing", "argv"):
            changed = copy.deepcopy(manifest)
            if operation == "missing":
                changed["cases"].pop()
            elif operation == "result":
                changed["cases"][0]["result"] = {"data": []}
            else:
                changed["cases"][0]["argv"] = ["ls", "-q", ""]
            with self.assertRaisesRegex(AssertionError, "Immutable consumer"):
                REFERENCE.load_consumers(json.dumps(changed).encode())

    def test_actual_source_consumers_never_replay_expected_payload(self) -> None:
        sources = REFERENCE.source_consumers()
        self.assertEqual(len(sources), 16)
        for case, inputs, observed in sources:
            self.assertNotIn("expected", inputs)
            self.assertNotIn("result", inputs)
            self.assertNotIn("stdout", inputs)
            self.assertEqual(observed, REFERENCE.consumer_expected(case))

    def test_consumer_byte_or_request_corruption_is_detected(self) -> None:
        sources = REFERENCE.source_consumers()
        for field in ("stdout", "requests", "scopes"):
            corrupted = copy.deepcopy(sources[0][2])
            if field == "stdout":
                corrupted[field] += "\n"
            else:
                corrupted[field] = []
            with patch.object(REFERENCE.COMMANDS, "run_rust", return_value=[corrupted]):
                with self.assertRaisesRegex(AssertionError, "consumer != source"):
                    REFERENCE.compare_consumers(
                        Path(sys.executable), Path(sys.executable)
                    )

    def test_source_get_provider_uses_expression_not_fixture_result(self) -> None:
        manifest = REFERENCE.load_consumers()
        case = copy.deepcopy(
            next(case for case in manifest["cases"] if case["id"] == "get_last_query")
        )
        case["argv"] = ["get", ".", "-q", "properties.label", "--output_format", "json"]
        folder_reference = REFERENCE.support("folder_commands")
        observed = REFERENCE.consumer_source(
            self.reference,
            folder_reference,
            REFERENCE.consumer_inputs(manifest, case),
            case,
            folder_reference.load_sources(),
        )
        self.assertEqual(json.loads(observed["stdout"])["result"]["data"], ["ok"])
        self.assertNotEqual(observed, REFERENCE.consumer_expected(case))

    def test_missing_duplicate_or_reordered_native_inventory_fails(self) -> None:
        original = self.observations()
        for changed in (
            original[:-1],
            original + original[:1],
            list(reversed(original)),
        ):
            with self.assertRaisesRegex(AssertionError, "inventory"):
                REFERENCE.compare(self.cases, changed)

    def test_native_query_corruption_fails(self) -> None:
        changed = self.observations()
        changed[2]["parse"]["read"]["query"] = None
        self.assertTrue(REFERENCE.compare(self.cases, changed))

    def test_native_path_or_flags_cannot_hide_behind_same_query(self) -> None:
        for field, value in (("path", ["wrong"]), ("all", True), ("name", "dir")):
            changed = self.observations()
            changed[0]["parse"]["read"][field] = value
            self.assertTrue(REFERENCE.compare(self.cases, changed))

    def test_parse_failure_cannot_be_relabelled_as_success(self) -> None:
        changed = self.observations()
        index = next(i for i, case in enumerate(self.cases) if case.get("error"))
        changed[index]["parse"] = changed[0]["parse"]
        self.assertTrue(REFERENCE.compare(self.cases, changed))

    def test_expected_data_is_never_sent_to_native_probe(self) -> None:
        cases = [copy.deepcopy(self.cases[0])]
        cases[0]["expected"] = {"secret_expected_only": "must-not-replay"}
        completed = subprocess.CompletedProcess(
            ["probe"], 0, '{"id":"ls_absent","parse":{"error":true}}\n', ""
        )
        with patch.object(REFERENCE.subprocess, "run", return_value=completed) as run:
            REFERENCE.native_observations(Path(sys.executable), cases)
        sent = json.loads(run.call_args.kwargs["input"])
        self.assertEqual(set(sent), {"id", "parse_only", "command"})
        self.assertNotIn("must-not-replay", run.call_args.kwargs["input"])

    def test_network_process_import_sleep_and_file_guards(self) -> None:
        attempts = [
            lambda: socket.socket(),
            lambda: subprocess.run([sys.executable, "-c", "pass"], check=True),
            lambda: builtins.__import__("msal"),
            lambda: time.sleep(0),
            lambda: io.open("must-not-be-created", "w"),
        ]
        for attempt in attempts:
            with self.subTest(attempt=attempt):
                with self.assertRaises(REFERENCE.COMMANDS.OfflineViolation):
                    with REFERENCE.COMMANDS.offline_boundary():
                        attempt()

    def test_caught_side_effect_still_fails_source_boundary(self) -> None:
        with self.assertRaises(REFERENCE.COMMANDS.OfflineViolation):
            with REFERENCE.COMMANDS.offline_boundary():
                try:
                    builtins.open("must-not-be-created", "w")
                except REFERENCE.COMMANDS.OfflineViolation:
                    pass


if __name__ == "__main__":
    unittest.main()

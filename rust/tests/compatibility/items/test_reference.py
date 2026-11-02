# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline regression checks for the independent item reference (stdlib only)."""

from __future__ import annotations

import copy
import importlib.util
import json
import socket
import subprocess
import sys
import unittest
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("item_reference", HERE / "reference.py")
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("Cannot load the independent item reference")
reference = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(reference)


class ItemReferenceTests(unittest.TestCase):
    """Protect the source boundary, immutable cases, and offline safeguards."""

    @classmethod
    def setUpClass(cls) -> None:
        """Read the manifest and verify source pins before any observations."""
        cls.manifest = reference.load_cases()
        cls.sources = reference.load_sources()

    def test_all_independent_goldens(self) -> None:
        """Exercise both comparable cases and separately labeled source quirks."""
        cases = self.manifest["cases"] + self.manifest["python_only"]
        for case in cases:
            with self.subTest(case=case["id"]):
                reference.assert_observation(
                    case, reference.observe(case, self.sources)
                )

    def test_repeatability_and_fixture_immutability(self) -> None:
        """Check recursion does not mutate hand-authored fixture inputs."""
        before = copy.deepcopy(self.manifest)
        case = next(
            case
            for case in self.manifest["cases"]
            if case["id"] == "list_three_pages_empty_middle"
        )
        first = reference.observe(case, self.sources)
        second = reference.observe(case, self.sources)
        self.assertEqual(first, second)
        self.assertEqual(before, self.manifest)

    def test_additive_query_punctuation_golden(self) -> None:
        """Pin Python quote-plus punctuation separately from the original cases."""
        case = json.loads((HERE / "query-encoding.json").read_text(encoding="utf-8"))
        self.assertEqual(case["source_commit"], reference.SOURCE_COMMIT)
        reference.assert_observation(case, reference.observe(case, self.sources))

    def test_source_drift_fails_closed(self) -> None:
        """Changing an executed source file must not silently select a new oracle."""
        root = reference.ROOT / "src" / "fabric_cli"
        sources = {
            name: root.joinpath(*name.split("/")).read_bytes()
            for name in reference.SOURCE_SHA256
        }
        reference._verify_digests(sources)
        changed = dict(sources)
        changed["client/fab_api_item.py"] += b"\n# synthetic drift\n"
        with self.assertRaises(AssertionError):
            reference._verify_digests(changed)
        incomplete = dict(sources)
        del incomplete["client/fab_api_types.py"]
        with self.assertRaises(AssertionError):
            reference._verify_digests(incomplete)

    def test_comparison_preserves_json_scalar_types(self) -> None:
        """Python equality must not blur boolean/integer/float or absent/null."""
        for left, right in (
            ({"flag": True}, {"flag": 1}),
            ({"number": 1}, {"number": 1.0}),
            ({}, {"type": None}),
            ([1, 2], [2, 1]),
        ):
            with self.subTest(left=left, right=right):
                self.assertFalse(reference._same_json(left, right))
        self.assertTrue(
            reference._same_json(
                {"id": "i", "extra": {"a": None, "b": True}},
                {"extra": {"b": True, "a": None}, "id": "i"},
            )
        )

    def test_comparison_detects_independent_field_mutations(self) -> None:
        """Request, rows, raw success, credential and error changes must fail."""
        case = self.manifest["cases"][0]
        actual = reference.observe(case, self.sources)
        mutations: list[dict[str, Any]] = []
        for field, value in (
            ("method", "POST"),
            ("url", "https://not-the-source.invalid/items"),
            ("scope", "https://not-the-source.invalid/.default"),
        ):
            mutated = copy.deepcopy(actual)
            mutated["requests"][0][field] = value
            mutations.append(mutated)
        mutated = copy.deepcopy(actual)
        mutated["rows"] = [{"id": "invented", "displayName": "Not in golden"}]
        mutations.append(mutated)
        mutated = copy.deepcopy(actual)
        mutated["python"]["status_code"] = 201
        mutations.append(mutated)
        mutated = copy.deepcopy(actual)
        mutated["python"]["body"] = {"value": [], "total_pages": 1}
        mutations.append(mutated)
        mutated = copy.deepcopy(actual)
        mutated["auth_scopes"] = []
        mutations.append(mutated)
        for index, mutated in enumerate(mutations):
            with self.subTest(field=index), self.assertRaises(AssertionError):
                reference.assert_observation(case, mutated)

        error_case = next(
            case
            for case in self.manifest["cases"]
            if case["id"] == "list_400_body_over_headers"
        )
        error = reference.observe(error_case, self.sources)
        for field in ("error_code", "message", "request_id", "exception_type"):
            mutated = copy.deepcopy(error)
            mutated["python"][field] = "invented"
            with self.subTest(error_field=field), self.assertRaises(AssertionError):
                reference.assert_observation(error_case, mutated)
        mutated = copy.deepcopy(error)
        mutated["error_code"] = "invented"
        with self.assertRaises(AssertionError):
            reference.assert_observation(error_case, mutated)
        mutated = copy.deepcopy(error)
        mutated["rows"] = []
        with self.assertRaises(AssertionError):
            reference.assert_observation(error_case, mutated)

    def test_offline_guard_rejects_side_effects_even_if_caught(self) -> None:
        """No socket, file access, process, sleep or real import may escape."""
        probes = [
            lambda: socket.socket(),
            lambda: (HERE / "cases.json").read_text(encoding="utf-8"),
            lambda: (HERE / "must-not-exist").write_text("forbidden", encoding="utf-8"),
            lambda: subprocess.Popen(["must-not-run"]),
            lambda: reference.time.sleep(1),
            lambda: __import__("msal"),
        ]
        for index, probe in enumerate(probes):
            with self.subTest(probe=index), self.assertRaises(
                reference.OfflineViolation
            ):
                with reference.offline_boundary({}):
                    try:
                        probe()
                    except reference.OfflineViolation:
                        pass


if __name__ == "__main__":
    unittest.main()

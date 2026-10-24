# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Guard the additive raw-metadata comparisons without changing old expectations."""

from __future__ import annotations

import copy
import importlib.util
import json
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "_raw_metadata_reference", Path(__file__).with_name("metadata_reference.py")
)
if SPEC is None or SPEC.loader is None:
    raise AssertionError("Cannot load additive metadata reference")
METADATA = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(METADATA)
REFERENCE = METADATA.REFERENCE


class MetadataReferenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.sources = REFERENCE.load_sources()

    def test_all_synthetic_cases_execute_original_get_helper(self) -> None:
        for case in METADATA.cases():
            with self.subTest(case=case["id"]):
                actual = REFERENCE.observe(case["input"], self.sources)
                REFERENCE.assert_observation(case, actual, python=True)

    def test_parser_semantics_are_observed_not_an_arbitrary_duplicate_policy(self) -> None:
        first = json.loads(METADATA.SAMPLES[0][1])
        numbers = first["future"]["a"]
        self.assertEqual([type(value) for value in numbers], [int, float, float, int])
        self.assertEqual(json.dumps(numbers), "[1, 1.0, -0.0, 9007199254740993]")
        duplicate = json.loads(METADATA.SAMPLES[3][1])
        self.assertEqual(
            list(duplicate), ["before", "displayName", "id", "nested", "type"]
        )
        self.assertEqual(
            json.dumps(duplicate["nested"]), '{"z": 2.0, "a": 1}'
        )
        self.assertIs(type(duplicate["before"]), float)
        self.assertEqual(duplicate["displayName"], "N")
        self.assertEqual(duplicate["id"], "item")

    def test_sorted_or_replayed_metadata_cannot_pass(self) -> None:
        case = METADATA.cases()[0]
        changed = copy.deepcopy(case["input"])
        parsed = json.loads(changed["responses"][0]["body"])
        changed["responses"][0]["body"] = json.dumps(parsed, sort_keys=True)
        actual = REFERENCE.observe(changed, self.sources)
        with self.assertRaisesRegex(AssertionError, "contract mismatch"):
            REFERENCE.assert_observation(case, actual, python=True)
        changed = copy.deepcopy(case["input"])
        changed["responses"][0]["body"] = (
            r'{"id":"item","displayName":"Changed","new":true}'
        )
        actual = REFERENCE.observe(changed, self.sources)
        self.assertEqual(
            json.loads(actual["stdout"])["result"]["data"],
            ["id", "displayName", "new"],
        )

    def test_inventory_and_expected_paths_are_discriminating(self) -> None:
        self.assertEqual(
            [case["id"] for case in METADATA.cases()],
            [
                "unknown_before_known", "reordered_keys", "missing_type",
                "parser_duplicate_keys",
            ],
        )
        for case in METADATA.cases():
            actual = REFERENCE.observe(case["input"], self.sources)
            mutated = copy.deepcopy(case)
            mutated["expected"]["output"]["result"]["data"].reverse()
            with self.assertRaises(AssertionError):
                REFERENCE.assert_observation(mutated, actual, python=True)


if __name__ == "__main__":
    unittest.main()

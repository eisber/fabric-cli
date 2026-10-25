# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Guard source-route completeness and the explicit comparator selection."""

from __future__ import annotations

import copy
import importlib.util
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    "_specialized_reference", Path(__file__).with_name("specialized_reference.py")
)
if SPEC is None or SPEC.loader is None:
    raise AssertionError("Cannot load specialized route reference")
SPECIALIZED = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SPECIALIZED)
REFERENCE = SPECIALIZED.REFERENCE


class SpecializedReferenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.sources = REFERENCE.load_sources()
        cls.rows = SPECIALIZED.routes(cls.sources)

    def test_complete_hand_authored_routes_execute_actual_source_helper(self) -> None:
        inventory = SPECIALIZED.cases(self.rows)
        self.assertEqual(len(inventory), 176)
        self.assertEqual(sum(allowed for _, allowed in inventory), 124)
        for case, _ in inventory:
            with self.subTest(case=case["id"]):
                actual = REFERENCE.observe(case["input"], self.sources)
                REFERENCE.assert_observation(case, actual, python=True)

    def test_unknown_types_fail_in_source_instead_of_generic_success(self) -> None:
        for kind in ("Future", "notebooks", "Notebook/../items"):
            case = copy.deepcopy(SPECIALIZED.METADATA.cases()[2]["input"])
            case["current_item"]["type"] = kind
            with self.subTest(kind=kind), self.assertRaises(Exception) as caught:
                REFERENCE.observe(case, self.sources)
            self.assertEqual(type(caught.exception).__name__, "FabricCLIError")
            self.assertEqual(caught.exception.status_code, "InvalidItemType")
            self.assertEqual(
                str(caught.exception),
                f"[InvalidItemType] '{kind}' is not a valid Fabric item type",
            )

    def test_source_routes_do_not_relabel_historical_get_or_safety(self) -> None:
        manifest = REFERENCE.load_cases()
        boundaries = REFERENCE.load_native_boundaries()
        for case in manifest["cases"]:
            source = REFERENCE.observe(case["input"], self.sources)
            if case["id"] in boundaries["safety_exceptions"]:
                native = REFERENCE._expected(boundaries["safety_exceptions"][case["id"]])
                self.assertEqual(REFERENCE.compare_native_case(
                    case, source, native, boundaries,
                    request_mode="source-routed",
                ), "native-safety")
            else:
                self.assertEqual(REFERENCE.compare_native_case(
                    case, source, source, boundaries,
                    request_mode="source-routed",
                ), "source-exact")
                if case["input"]["command"][0] == "get":
                    with self.assertRaises(AssertionError):
                        REFERENCE.compare_native_case(
                            case, source, REFERENCE._expected(case), boundaries,
                            request_mode="source-routed",
                        )
        case = manifest["cases"][0]
        source = REFERENCE.observe(case["input"], self.sources)
        with self.assertRaises(ValueError):
            REFERENCE.compare_native_case(
                case, source, source, boundaries, request_mode="automatic",
            )
        with self.assertRaises(ValueError):
            REFERENCE.compare_native_case(
                case, source, source, boundaries, "historical-typed-order", "source-routed",
            )


if __name__ == "__main__":
    unittest.main()

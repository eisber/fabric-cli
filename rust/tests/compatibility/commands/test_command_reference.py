# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Guard independent command expectations and the actual Python call boundary."""

from __future__ import annotations

import copy
import importlib.util
import json
import sys
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "_bounded_command_reference", HERE / "reference.py"
)
if SPEC is None or SPEC.loader is None:
    raise AssertionError("Cannot load the command reference")
REFERENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REFERENCE)


class CommandReferenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = REFERENCE.load_cases()
        cls.sources = REFERENCE.load_sources()
        cls.boundaries = REFERENCE.load_native_boundaries()
        cls.cases = {
            case["id"]: case
            for case in cls.manifest["cases"] + cls.manifest["source_only"]
        }

    def observation(self, case_id: str) -> dict[str, Any]:
        """Observe source, never return an expectation-shaped stand-in."""
        return REFERENCE.observe(self.cases[case_id]["input"], self.sources)

    def test_source_pin_edits_are_rejected(self) -> None:
        for field, value in (
            ("commit", "0" * 40),
            ("source_file_count", 1),
            ("source_tree_sha256", "0" * 64),
        ):
            with self.subTest(field=field):
                changed = copy.deepcopy(REFERENCE.SOURCE_PIN)
                changed[field] = value
                with self.assertRaisesRegex(AssertionError, "source pin changed"):
                    REFERENCE.verify_source(changed)

    def test_existing_source_validator_is_not_bypassed(self) -> None:
        with patch.object(
            REFERENCE.WORKSPACES,
            "verify_source",
            side_effect=AssertionError("Python source drift"),
        ) as validator:
            with self.assertRaisesRegex(AssertionError, "Python source drift"):
                REFERENCE.load_sources()
            validator.assert_called_once_with(REFERENCE.SOURCE_PIN)

    def test_source_manifest_identity_is_pinned(self) -> None:
        for field, value in (
            ("source_commit", "0" * 40),
            ("schema_version", 99),
            ("_copyright", "unattributed"),
        ):
            with self.subTest(field=field):
                changed = copy.deepcopy(self.manifest)
                changed[field] = value
                with self.assertRaisesRegex(AssertionError, "identity changed"):
                    REFERENCE.validate_manifest(changed)

    def test_inventory_cannot_be_shrunk_or_trivialized(self) -> None:
        for mutation in ("delete", "duplicate", "responses", "expected"):
            with self.subTest(mutation=mutation):
                changed = copy.deepcopy(self.manifest)
                if mutation == "delete":
                    changed["cases"].pop()
                elif mutation == "duplicate":
                    changed["cases"][-1] = changed["cases"][0]
                elif mutation == "responses":
                    changed["cases"][0]["input"]["responses"] = []
                else:
                    changed["cases"][0]["expected"] = {}
                with self.assertRaises(AssertionError):
                    REFERENCE.validate_manifest(changed)

    def test_old_eight_cases_are_reused_unchanged(self) -> None:
        original = REFERENCE.WORKSPACES.load_cases()["cases"]
        self.assertEqual(self.manifest["workspaces"], original)
        self.assertEqual(
            tuple(case["id"] for case in original), REFERENCE.OLD_CASE_IDS
        )
        query = next(case for case in original if case["id"] == "query_projection")
        self.assertEqual(
            query["query"], "[?id != 'skip'].{workspace: name, key: id}"
        )
        with self.assertRaisesRegex(ValueError, "Query cases"):
            REFERENCE.observe(REFERENCE.workspace_input(query), self.sources)

    def test_expectation_edits_do_not_redefine_source(self) -> None:
        case = copy.deepcopy(self.cases["item_bytype_enum_names"])
        actual = self.observation(case["id"])
        REFERENCE.assert_observation(case, actual, python=True)
        case["expected"]["output"]["result"]["data"].reverse()
        with self.assertRaisesRegex(AssertionError, "contract mismatch"):
            REFERENCE.assert_observation(case, actual, python=True)

    def test_every_protocol_field_is_checked(self) -> None:
        case = self.cases["item_token_pagination"]
        original = self.observation(case["id"])
        mutations = {
            "id": "other-case",
            "exit_code": 1,
            "stdout": "{}\n",
            "stderr": "unexpected diagnostic\n",
            "requests": original["requests"][:-1],
            "scopes": [["https://storage.azure.com/.default"]],
        }
        for field, value in mutations.items():
            with self.subTest(field=field):
                changed = copy.deepcopy(original)
                changed[field] = value
                with self.assertRaisesRegex(AssertionError, "contract mismatch"):
                    REFERENCE.assert_observation(case, changed, python=True)
        changed = {**original, "unapproved_field": True}
        with self.assertRaisesRegex(AssertionError, "contract mismatch"):
            REFERENCE.assert_observation(case, changed, python=True)

    def test_new_stream_expectations_are_byte_exact(self) -> None:
        case = self.cases["item_long_id"]
        original = self.observation(case["id"])
        for stdout in (
            original["stdout"].replace("\n", "\r\n"),
            original["stdout"].rstrip("\n"),
            original["stdout"].replace("    ", "  "),
            original["stdout"].replace(REFERENCE.TIMESTAMP, "2001-01-01T00:00:00Z"),
            original["stdout"].replace("\\u6771\\u4eac", "\u6771\u4eac"),
        ):
            with self.subTest(stdout=stdout):
                with self.assertRaisesRegex(AssertionError, "contract mismatch"):
                    REFERENCE.assert_observation(
                        case, {**original, "stdout": stdout}, python=True
                    )

    def test_get_uri_difference_must_be_declared(self) -> None:
        for mutation in ("waiver", "python_requests", "equal"):
            with self.subTest(mutation=mutation):
                changed = copy.deepcopy(self.manifest)
                case = next(
                    case for case in changed["cases"]
                    if case["id"] == "get_metadata_paths"
                )
                if mutation == "waiver":
                    case.pop("comparison")
                elif mutation == "python_requests":
                    case.pop("python_requests")
                else:
                    case["python_requests"] = case["expected"]["requests"]
                with self.assertRaisesRegex(AssertionError, "URI seam"):
                    REFERENCE.validate_manifest(changed)

    def test_native_generic_uri_is_not_silently_equated(self) -> None:
        case = self.cases["get_metadata_paths"]
        actual = self.observation(case["id"])
        self.assertIn("/notebooks/item", actual["requests"][0])
        with self.assertRaisesRegex(AssertionError, "requests"):
            REFERENCE.compare_rust(case, actual, actual)

    def test_reference_fixtures_are_discriminating(self) -> None:
        name = self.cases["item_byname_stable_unicode"]
        typed = self.cases["item_bytype_enum_names"]
        rows = name["expected"]["output"]["result"]["data"]
        self.assertEqual(
            [row["id"] for row in rows if row["name"].lower() == "alpha.notebook"],
            ["first", "second"],
        )
        self.assertEqual(
            [row["id"] for row in rows if row["name"].lower().startswith("same.")],
            ["graph-api", "graph-model"],
        )
        self.assertEqual(
            typed["expected"]["output"]["result"]["data"][2]["id"], "api-z"
        )
        long_id = self.cases["item_long_id"]["expected"]["output"]["result"]["data"]
        self.assertGreater(len(long_id[0]["id"]), 100)
        self.assertEqual(self.cases["item_future_only"]["expected"]["output"]["result"]["data"], [])
        self.assertEqual(
            len(self.cases["item_hidden_all"]["expected"]["output"]["result"]["hidden_data"]),
            4,
        )
        paths = self.cases["get_metadata_paths"]["expected"]["output"]["result"]["data"]
        self.assertIn("properties.emptyList", paths)
        self.assertNotIn("properties.emptyObject", paths)

    def test_source_executes_input_not_expected_payload(self) -> None:
        case = copy.deepcopy(self.cases["item_hidden_config"])
        case["input"]["responses"][0]["body"]["value"][0]["displayName"] = "Changed"
        actual = REFERENCE.observe(case["input"], self.sources)
        self.assertEqual(
            json.loads(actual["stdout"])["result"]["data"],
            [{"name": "Changed.Notebook"}],
        )
        with self.assertRaisesRegex(AssertionError, "contract mismatch"):
            REFERENCE.assert_observation(case, actual, python=True)

    def test_response_scripts_are_consumed_exactly(self) -> None:
        extra = copy.deepcopy(self.cases["item_empty"]["input"])
        extra["responses"].append({"status": 200, "body": {"value": []}})
        with self.assertRaisesRegex(AssertionError, "unused scripted"):
            REFERENCE.observe(extra, self.sources)
        short = copy.deepcopy(self.cases["item_token_pagination"]["input"])
        short["responses"].pop()
        with self.assertRaisesRegex(REFERENCE.OfflineViolation, "additional HTTP"):
            REFERENCE.observe(short, self.sources)

    def test_query_engine_is_not_reimplemented(self) -> None:
        inputs = copy.deepcopy(self.cases["get_metadata_paths"]["input"])
        inputs["command"] = ["get", ".", "--query", "id", "--output_format", "json"]
        with self.assertRaisesRegex(ValueError, "Query cases"):
            REFERENCE.observe(inputs, self.sources)

    def test_json_selection_uses_the_actual_global_output_format_flag(self) -> None:
        for case_id in ("item_empty", "get_metadata_paths"):
            with self.subTest(case=case_id):
                inputs = copy.deepcopy(self.cases[case_id]["input"])
                self.assertEqual(inputs["command"][-2:], ["--output_format", "json"])
                inputs["command"][-2] = "--output"
                with self.assertRaises(ValueError):
                    REFERENCE.observe(inputs, self.sources)

    def test_unapproved_paths_and_folder_listing_are_explicit(self) -> None:
        for command in (
            ["ls", "/Other.Workspace", "--output_format", "json"],
            ["get", "Other.Notebook", "--output_format", "json"],
        ):
            with self.subTest(command=command):
                inputs = copy.deepcopy(self.cases["item_empty"]["input"])
                inputs["command"] = command
                with self.assertRaises(ValueError):
                    REFERENCE.observe(inputs, self.sources)
        inputs = copy.deepcopy(self.cases["item_empty"]["input"])
        inputs["config"] = {"folder_listing_enabled": True}
        with self.assertRaisesRegex(ValueError, "Folder listing"):
            REFERENCE.observe(inputs, self.sources)

    def test_all_offline_guards_block_before_side_effect(self) -> None:
        for name in ("network", "read", "write", "identity", "process", "sleep"):
            with self.subTest(name=name):
                with self.assertRaises(REFERENCE.OfflineViolation):
                    REFERENCE.probe_boundary(name)

    def test_fresh_source_observations_are_deterministic(self) -> None:
        for case_id in ("item_byname_stable_unicode", "get_metadata_paths"):
            with self.subTest(case=case_id):
                self.assertEqual(
                    self.observation(case_id), self.observation(case_id)
                )

    def test_source_only_cannot_be_counted_as_native_parity(self) -> None:
        only = self.manifest["source_only"][0]
        self.assertEqual(only["id"], "get_empty_metadata")
        self.assertTrue(only["reason"])
        self.assertNotIn(only["id"], REFERENCE.NEW_CASE_IDS)
        self.assertEqual(only["expected"]["output"], None)

    def test_get_comparison_requires_real_projection_integration(self) -> None:
        cases = copy.deepcopy(self.manifest["cases"])
        before = copy.deepcopy(cases)
        active, unsupported = REFERENCE.native_inventory(cases, False)
        self.assertEqual(active, cases)
        self.assertEqual(len(active), 18)
        self.assertEqual(len(unsupported), 2)
        self.assertTrue(
            all(case["input"]["command"][0] == "get" for case in unsupported)
        )
        integrated, still_unsupported = REFERENCE.native_inventory(cases, True)
        self.assertEqual(integrated, cases)
        self.assertEqual(still_unsupported, [])
        self.assertEqual(cases, before)

    def test_native_safety_is_separate_from_unchanged_source(self) -> None:
        for case_id, boundary in self.boundaries["safety_exceptions"].items():
            with self.subTest(case=case_id):
                workspace = case_id in REFERENCE.OLD_CASE_IDS
                if workspace:
                    case = next(
                        case for case in self.manifest["workspaces"]
                        if case["id"] == case_id
                    )
                    actual = REFERENCE.observe(
                        REFERENCE.workspace_input(case), self.sources
                    )
                    REFERENCE.assert_workspace(case, actual)
                else:
                    case = self.cases[case_id]
                    actual = self.observation(case_id)
                    REFERENCE.assert_observation(case, actual, python=True)
                with self.assertRaisesRegex(AssertionError, "contract mismatch"):
                    REFERENCE.assert_native_boundary(
                        case, actual, actual, boundary, workspace=workspace
                    )
                self.assertEqual(boundary["expected"]["exit_code"], 1)
                self.assertEqual(len(boundary["expected"]["requests"]), 1)

    def test_missing_projection_requires_actual_not_supported_failure(self) -> None:
        for case_id, boundary in self.boundaries["unsupported_projection"].items():
            with self.subTest(case=case_id):
                case = self.cases[case_id]
                actual = self.observation(case_id)
                self.assertEqual(actual["exit_code"], 0)
                self.assertEqual(
                    boundary["expected"]["output"]["result"]["error_code"],
                    "NotSupported",
                )
                with self.assertRaisesRegex(AssertionError, "contract mismatch"):
                    REFERENCE.assert_native_boundary(
                        case, actual, actual, boundary
                    )

    def test_metadata_schema_order_is_not_source_order_parity(self) -> None:
        for case_id, boundary in self.boundaries["metadata_order"].items():
            with self.subTest(case=case_id):
                case = self.cases[case_id]
                source_paths = case["expected"]["output"]["result"]["data"]
                native_paths = boundary["expected"]["output"]["result"]["data"]
                self.assertNotEqual(source_paths, native_paths)
                self.assertCountEqual(source_paths, native_paths)
                actual = self.observation(case_id)
                with self.assertRaisesRegex(AssertionError, "contract mismatch"):
                    REFERENCE.assert_native_boundary(
                        case, actual, actual, boundary
                    )

    def test_default_metadata_comparison_requires_source_order_and_keeps_uri_seam(self) -> None:
        for case_id, historical in self.boundaries["metadata_order"].items():
            with self.subTest(case=case_id):
                case = self.cases[case_id]
                python = self.observation(case_id)
                current = REFERENCE._expected(case)
                self.assertEqual(
                    REFERENCE.compare_native_case(
                        case, python, current, self.boundaries
                    ),
                    "generic-get-uri",
                )
                old = REFERENCE._expected(historical)
                with self.assertRaisesRegex(AssertionError, "contract mismatch"):
                    REFERENCE.compare_native_case(
                        case, python, old, self.boundaries
                    )
                self.assertEqual(
                    REFERENCE.compare_native_case(
                        case, python, old, self.boundaries, "historical-typed-order"
                    ),
                    "metadata-schema-order",
                )
                with self.assertRaisesRegex(AssertionError, "contract mismatch"):
                    REFERENCE.compare_native_case(
                        case, python, current, self.boundaries,
                        "historical-typed-order",
                    )
                for field, replacement in (
                    ("exit_code", 1), ("stderr", "private"),
                    ("scopes", []), ("requests", python["requests"]),
                    ("stdout", current["stdout"].replace("\n", "\r\n")),
                ):
                    with self.subTest(field=field), self.assertRaises(AssertionError):
                        REFERENCE.compare_native_case(
                            case, python, {**current, field: replacement}, self.boundaries
                        )

    def test_historical_no_projection_mode_asserts_old_failures_not_current_success(self) -> None:
        for case_id, boundary in self.boundaries["unsupported_projection"].items():
            case = self.cases[case_id]
            python = self.observation(case_id)
            old = REFERENCE._expected(boundary)
            self.assertEqual(
                REFERENCE.compare_native_case(
                    case, python, old, self.boundaries, "historical-no-projection"
                ),
                "unsupported-projection",
            )
            with self.assertRaises(AssertionError):
                REFERENCE.compare_native_case(case, python, old, self.boundaries)
        with self.assertRaisesRegex(ValueError, "Unknown GET"):
            REFERENCE.compare_native_case(case, python, old, self.boundaries, "waived")

    def test_native_boundaries_cannot_be_erased_or_called_success(self) -> None:
        manifest = json.loads(
            (HERE / "native-boundaries.json").read_text(encoding="utf-8")
        )
        for category in REFERENCE.NATIVE_BOUNDARY_IDS:
            with self.subTest(category=category):
                changed = copy.deepcopy(manifest)
                changed[category].pop()
                with self.assertRaisesRegex(AssertionError, "inventory changed"):
                    REFERENCE.validate_native_boundaries(changed)
        changed = copy.deepcopy(manifest)
        changed["source_commit"] = "0" * 40
        with self.assertRaisesRegex(AssertionError, "identity changed"):
            REFERENCE.validate_native_boundaries(changed)
        changed = copy.deepcopy(manifest)
        changed["unsupported_projection"][0]["expected"]["exit_code"] = 0
        with self.assertRaisesRegex(AssertionError, "not a successful"):
            REFERENCE.validate_native_boundaries(changed)


def _new_case_test(case_id: str) -> Any:
    def test(self: CommandReferenceTests) -> None:
        case = self.cases[case_id]
        REFERENCE.assert_observation(
            case, self.observation(case_id), python=True
        )

    test.__name__ = f"test_source_{case_id}"
    test.__doc__ = f"Execute the actual pinned command/helper boundary: {case_id}."
    return test


def _workspace_test(case_id: str) -> Any:
    def test(self: CommandReferenceTests) -> None:
        case = next(
            case for case in self.manifest["workspaces"] if case["id"] == case_id
        )
        actual = REFERENCE.observe(REFERENCE.workspace_input(case), self.sources)
        REFERENCE.assert_workspace(case, actual)

    test.__name__ = f"test_workspace_{case_id}"
    test.__doc__ = f"Reuse unchanged workspace command expectations: {case_id}."
    return test


for _case_id in (*REFERENCE.NEW_CASE_IDS, "get_empty_metadata"):
    setattr(
        CommandReferenceTests,
        f"test_source_{_case_id}",
        _new_case_test(_case_id),
    )
for _case_id in REFERENCE.OLD_CASE_IDS:
    if _case_id != "query_projection":
        setattr(
            CommandReferenceTests,
            f"test_workspace_{_case_id}",
            _workspace_test(_case_id),
        )


if __name__ == "__main__":
    unittest.main()

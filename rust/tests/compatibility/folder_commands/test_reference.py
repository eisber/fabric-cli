# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Guards for the independent folder-command oracle, not Rust implementation tests."""

from __future__ import annotations

import ast
import builtins
import copy
import importlib.util
import io
import json
import os
import socket
import subprocess
import sys
import time
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "_independent_folder_command_reference", HERE / "reference.py"
)
if SPEC is None or SPEC.loader is None:
    raise AssertionError("Cannot load the independent folder-command reference")
REFERENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REFERENCE)


class FolderCommandReferenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.raw = (HERE / "cases.json").read_bytes()
        cls.manifest = REFERENCE.load_cases()
        cls.sources = REFERENCE.load_sources()
        cls.cases = {case["id"]: case for case in cls.manifest["cases"]}
        cls.native_raw = (HERE / "native-boundaries.json").read_bytes()
        cls.native_manifest = REFERENCE.load_native_boundaries()

    def observe(
        self, identifier: str, trace: list[str] | None = None
    ) -> dict[str, Any]:
        case = self.cases[identifier]
        return REFERENCE.observe(
            REFERENCE.case_input(case, python=True),
            self.sources,
            cache_enabled=case.get("python_cache_enabled", True),
            trace=trace,
        )

    def test_all_terminating_source_contracts(self) -> None:
        observations = REFERENCE.check_python(self.manifest, self.sources)
        self.assertEqual(len(observations), 59)
        self.assertEqual(sum(value is None for value in observations), 5)

    def test_complete_source_pin_and_drift_rejection(self) -> None:
        for field, value in (
            ("commit", "0" * 40),
            ("source_file_count", 1),
            ("source_tree_sha256", "0" * 64),
        ):
            with self.subTest(field=field):
                changed = copy.deepcopy(REFERENCE.COMMANDS.SOURCE_PIN)
                changed[field] = value
                with self.assertRaisesRegex(AssertionError, "source pin changed"):
                    REFERENCE.verify_source(changed)
        with patch.object(
            REFERENCE.COMMANDS,
            "verify_source",
            side_effect=AssertionError("source tree drift"),
        ):
            with self.assertRaisesRegex(AssertionError, "source tree drift"):
                REFERENCE.load_sources()

    def test_complete_private_collaborator_ast_inventory(self) -> None:
        required = {
            "commands/fs/ls/fab_fs_ls_item.py": {"exec"},
            "commands/fs/ls/fab_fs_ls_folder.py": {"exec"},
            "utils/fab_cmd_fs_utils.py": {"get_ws_elements", "sort_ws_elements"},
            "utils/fab_item_util.py": {"_get_elem_type", "sort_ws_elems_by_config"},
            "utils/fab_mem_store.py": {
                "_get_workspace_items_from_api",
                "_extract_root_folders",
                "_extract_nested_folders",
                "_get_workspace_folders_from_api",
                "_get_workspace_folders_from_cache",
                "get_folder",
                "get_workspace_folders",
                "build_folder_cache_key",
            },
            "client/fab_api_folders.py": {"list_folders"},
        }
        for path, names in required.items():
            with self.subTest(path=path):
                found = {
                    node.name
                    for node in self.sources[path].body
                    if isinstance(node, (ast.FunctionDef, ast.ClassDef))
                }
                self.assertTrue(names <= found)
        folder_class = next(
            node
            for node in self.sources["core/hiearchy/fab_folder.py"].body
            if isinstance(node, ast.ClassDef) and node.name == "Folder"
        )
        self.assertIn(
            "__eq__",
            {
                node.name
                for node in folder_class.body
                if isinstance(node, ast.FunctionDef)
            },
        )

    def test_fixture_mutations_fail_even_with_unchanged_inventory(self) -> None:
        for mutation in (
            "expectation",
            "script",
            "category",
            "remove",
            "reorder",
            "pin",
        ):
            with self.subTest(mutation=mutation):
                changed = copy.deepcopy(self.manifest)
                if mutation == "expectation":
                    changed["cases"][0]["expected"]["result"]["data"] = ["forged"]
                elif mutation == "script":
                    changed["cases"][0]["input"]["responses"][0]["body"] = {}
                elif mutation == "category":
                    changed["cases"][0]["category"] = "safety"
                elif mutation == "remove":
                    changed["cases"].pop()
                elif mutation == "reorder":
                    changed["cases"].reverse()
                else:
                    changed["source_commit"] = "0" * 40
                with self.assertRaises(AssertionError):
                    REFERENCE.validate_manifest(changed, self.raw)
                altered_raw = json.dumps(changed).encode()
                with self.assertRaisesRegex(AssertionError, "Immutable"):
                    REFERENCE.validate_manifest(changed, altered_raw)

    def test_manifest_bytes_are_not_regenerated(self) -> None:
        with self.assertRaisesRegex(AssertionError, "Immutable"):
            REFERENCE.validate_manifest(self.manifest, self.raw + b"\n")
        self.assertEqual(REFERENCE.blob_identity(self.raw), REFERENCE.MANIFEST_BLOB)
        self.assertEqual(
            {category: len(ids) for category, ids in REFERENCE.INVENTORY.items()},
            {"exact": 32, "safety": 21, "snapshot-difference": 3, "unsupported": 3},
        )

    def test_every_protocol_field_and_stream_byte_is_asserted(self) -> None:
        case = self.cases["unicode_opaque_long"]
        original = self.observe(case["id"])
        REFERENCE.assert_observation(case, original, python=True)
        mutations = {
            "id": "different",
            "exit_code": 1,
            "stdout": original["stdout"].replace("\n", "\r\n"),
            "stderr": "unexpected\n",
            "requests": original["requests"][::-1],
            "scopes": [["https://storage.azure.com/.default"]],
            "unapproved_field": True,
        }
        for key, value in mutations.items():
            with self.subTest(field=key):
                with self.assertRaisesRegex(AssertionError, "contract mismatch"):
                    REFERENCE.assert_observation(
                        case, {**original, key: value}, python=True
                    )
        for stdout in (
            original["stdout"].rstrip("\n"),
            original["stdout"].replace("    ", "  "),
            original["stdout"].replace("\\u6771\\u4eac", "\u6771\u4eac"),
            original["stdout"].replace(REFERENCE.TIMESTAMP, "2020-01-01T00:00:00Z"),
        ):
            with self.subTest(stdout=stdout):
                with self.assertRaisesRegex(AssertionError, "contract mismatch"):
                    REFERENCE.assert_observation(
                        case, {**original, "stdout": stdout}, python=True
                    )

    def test_exact_comparison_does_not_ignore_request_or_scope_differences(
        self,
    ) -> None:
        case = self.cases["root_valid"]
        python = self.observe(case["id"])
        REFERENCE.compare(case, python, python)
        for field in ("requests", "scopes"):
            changed = {**python, field: []}
            with self.subTest(field=field):
                with self.assertRaisesRegex(AssertionError, "contract mismatch"):
                    REFERENCE.compare(case, python, changed)

    def test_snapshot_differences_are_not_full_exact(self) -> None:
        for identifier in REFERENCE.native_inventory(self.manifest["cases"])[
            "snapshot-difference"
        ]:
            with self.subTest(case=identifier):
                case = self.cases[identifier]
                python = self.observe(identifier)
                REFERENCE.assert_observation(case, python, python=True)
                native = REFERENCE.expected_observation(case)
                self.assertNotEqual(python, native)
                with self.assertRaisesRegex(AssertionError, "native contract mismatch"):
                    REFERENCE.compare(case, python, python)

    def test_reviewed_overlay_has_exactly_nine_bound_original_contracts(self) -> None:
        self.assertEqual(
            REFERENCE.blob_identity(self.native_raw),
            REFERENCE.NATIVE_BOUNDARIES_BLOB,
        )
        self.assertEqual(
            tuple(row["id"] for row in self.native_manifest["cases"]),
            REFERENCE.NATIVE_BOUNDARY_IDS,
        )
        for row in self.native_manifest["cases"]:
            original = self.cases[row["id"]]
            self.assertEqual(row["original_category"], original["category"])
            self.assertEqual(
                row["original_native_expected_sha256"],
                REFERENCE.expected_identity(original["expected"]),
            )
            self.assertEqual(
                set(row["expected"]),
                {"exit_code", "command", "result", "stderr", "requests", "scopes"},
            )

    def test_reviewed_overlay_content_and_inventory_cannot_drift(self) -> None:
        for mutation in (
            "unknown-id",
            "missing-id",
            "category",
            "original-hash",
            "original-category",
            "source-pin",
            "base-blob",
            "contract",
            "scope",
        ):
            with self.subTest(mutation=mutation):
                changed = copy.deepcopy(self.native_manifest)
                row = changed["cases"][0]
                if mutation == "unknown-id":
                    row["id"] = "not-reviewed"
                elif mutation == "missing-id":
                    changed["cases"].pop()
                elif mutation == "category":
                    row["category"] = "exact"
                elif mutation == "original-hash":
                    row["original_native_expected_sha256"] = "0" * 64
                elif mutation == "original-category":
                    row["original_category"] = "safety"
                elif mutation == "source-pin":
                    changed["source_commit"] = "0" * 40
                elif mutation == "base-blob":
                    changed["evidence"]["item_errors"]["blob"] = "0" * 40
                elif mutation == "contract":
                    row["expected"]["result"]["data"] = ["forged"]
                else:
                    row["expected"]["scopes"] = []
                with self.assertRaisesRegex(AssertionError, "In-memory"):
                    REFERENCE.validate_native_boundaries(
                        changed, self.native_raw, self.manifest
                    )
                with self.assertRaisesRegex(AssertionError, "Immutable"):
                    REFERENCE.validate_native_boundaries(
                        changed, json.dumps(changed).encode(), self.manifest
                    )

    def test_native_overlay_rejects_mutated_original_expectation_or_label(self) -> None:
        for field in ("expected", "category"):
            with self.subTest(field=field):
                source = copy.deepcopy(self.manifest)
                case = next(
                    row for row in source["cases"] if row["id"] == "cancel_before"
                )
                if field == "expected":
                    case["expected"]["result"]["message"] = "forged"
                else:
                    case["category"] = "exact"
                with self.assertRaisesRegex(
                    AssertionError, "original contract binding"
                ):
                    REFERENCE.validate_native_boundaries(
                        self.native_manifest, self.native_raw, source
                    )
                with self.assertRaisesRegex(
                    AssertionError, "original contract binding"
                ):
                    REFERENCE.expected_observation(case)

    def test_native_evidence_checks_historical_source_and_base_blobs(self) -> None:
        REFERENCE.verify_native_evidence(self.native_manifest)
        with patch.object(
            REFERENCE.subprocess,
            "run",
            return_value=type("Result", (), {"stdout": b"stale historical evidence"})(),
        ):
            with self.assertRaisesRegex(AssertionError, "historical source/base blob"):
                REFERENCE.verify_native_evidence(self.native_manifest)

    def test_reviewed_native_contracts_assert_every_observation_field(self) -> None:
        for identifier in REFERENCE.NATIVE_BOUNDARY_IDS:
            case = self.cases[identifier]
            expected = REFERENCE.expected_observation(case)
            for field, value in (
                ("id", "wrong"),
                ("exit_code", 9),
                ("stdout", expected["stdout"] + "\n"),
                ("stderr", "not-empty"),
                ("requests", ["not-a-request"]),
                ("scopes", [["wrong-scope"]]),
                ("unexpected", True),
            ):
                with self.subTest(case=identifier, field=field):
                    with self.assertRaisesRegex(
                        AssertionError, "native contract mismatch"
                    ):
                        REFERENCE.assert_observation(case, {**expected, field: value})

    def test_approved_category_swap_is_explicit_and_safety_does_not_gain_credit(
        self,
    ) -> None:
        native = REFERENCE.native_inventory(self.manifest["cases"])
        self.assertEqual(
            set(native["exact"]) - set(REFERENCE.INVENTORY["exact"]),
            {"current_snapshot_name"},
        )
        self.assertEqual(
            set(REFERENCE.INVENTORY["exact"]) - set(native["exact"]),
            {"workspace_flat_disabled"},
        )
        self.assertEqual(
            set(native["snapshot-difference"])
            - set(REFERENCE.INVENTORY["snapshot-difference"]),
            {"workspace_flat_disabled"},
        )
        self.assertEqual(
            set(REFERENCE.INVENTORY["snapshot-difference"])
            - set(native["snapshot-difference"]),
            {"current_snapshot_name"},
        )
        self.assertEqual(tuple(native["safety"]), REFERENCE.INVENTORY["safety"])
        self.assertEqual(
            tuple(native["unsupported"]), REFERENCE.INVENTORY["unsupported"]
        )
        self.assertEqual(
            {category: len(ids) for category, ids in native.items()},
            {"exact": 32, "safety": 21, "snapshot-difference": 3, "unsupported": 3},
        )

    def test_reviewed_overlay_never_replaces_original_python_expectations(self) -> None:
        for identifier in REFERENCE.NATIVE_BOUNDARY_IDS:
            case = self.cases[identifier]
            python = self.observe(identifier)
            REFERENCE.assert_observation(case, python, python=True)
            native = REFERENCE.expected_observation(case)
            REFERENCE.compare(case, python, native)
            if identifier == "current_snapshot_name":
                self.assertEqual(python, native)
            elif identifier == "workspace_flat_disabled":
                self.assertEqual(python["stdout"], native["stdout"])
                self.assertEqual(len(python["requests"]), 2)
                self.assertEqual(len(native["requests"]), 1)
            else:
                self.assertNotEqual(python, native)

    def test_source_executes_modified_input_not_frozen_output(self) -> None:
        case = self.cases["display_suffix_case"]
        inputs = REFERENCE.case_input(case, python=True)
        before = copy.deepcopy(inputs)
        inputs["responses"][1]["body"]["value"][0]["displayName"] = "Changed.Folder"
        actual = REFERENCE.observe(inputs, self.sources)
        self.assertEqual(
            json.loads(actual["stdout"])["result"]["data"],
            [{"name": "Alpha.Folder.Folder"}, {"name": "Changed.Folder.Folder"}],
        )
        with self.assertRaisesRegex(AssertionError, "contract mismatch"):
            REFERENCE.assert_observation(case, actual, python=True)
        self.assertNotEqual(inputs, before)

    def test_real_cached_source_and_item_parent_call_order(self) -> None:
        trace: list[str] = []
        self.observe("unknown_type_parent_lookup", trace)
        interesting = {
            "get_ws_elements",
            "get_workspace_items",
            "_get_workspace_items_from_cache",
            "_get_workspace_items_from_api",
            "ls_workspace_items",
            "get_folder",
            "get_workspace_folders",
            "_get_workspace_folders_from_cache",
            "_get_workspace_folders_from_api",
            "list_folders",
            "_extract_root_folders",
            "sort_ws_elements",
            "sort_ws_elems_by_config",
            "format_and_print_output",
            "print_output_format",
        }
        self.assertEqual(
            [name for name in trace if name in interesting],
            [
                "get_ws_elements",
                "get_workspace_items",
                "_get_workspace_items_from_cache",
                "_get_workspace_items_from_api",
                "ls_workspace_items",
                "get_folder",
                "get_workspace_folders",
                "_get_workspace_folders_from_cache",
                "_get_workspace_folders_from_api",
                "list_folders",
                "_extract_root_folders",
                "get_workspace_folders",
                "sort_ws_elements",
                "sort_ws_elems_by_config",
                "format_and_print_output",
                "print_output_format",
            ],
        )

    def test_item_parent_is_evaluated_before_missing_type(self) -> None:
        trace: list[str] = []
        actual = self.observe("missing_item_type", trace)
        self.assertEqual(json.loads(actual["stdout"])["result"]["message"], "type")
        self.assertIn("get_folder", trace)
        self.assertIn("list_folders", trace)
        self.assertNotIn("sort_ws_elements", trace)
        self.assertEqual(trace.count("_get_workspace_folders_from_api"), 1)

    def test_no_item_folder_id_defers_folders_until_items_complete(self) -> None:
        trace: list[str] = []
        self.observe("root_valid", trace)
        self.assertLess(trace.index("ls_workspace_items"), trace.index("list_folders"))
        trace.clear()
        self.observe("empty_root", trace)
        self.assertNotIn("get_folder", trace)
        self.assertLess(
            trace.index("_get_workspace_items_from_api"),
            trace.index("get_workspace_folders"),
        )

    def test_cache_disabled_source_repeats_actual_folder_api(self) -> None:
        trace: list[str] = []
        actual = self.observe("cache_disabled_repeat", trace)
        self.assertEqual(trace.count("_get_workspace_folders_from_api"), 2)
        self.assertNotIn("_get_workspace_folders_from_cache", trace)
        self.assertEqual(len(actual["requests"]), 3)
        self.assertEqual(len(actual["scopes"]), 3)

    def test_reverse_graph_executes_complete_nested_helpers(self) -> None:
        trace: list[str] = []
        self.observe("child_before_parent", trace)
        self.assertEqual(trace.count("_extract_root_folders"), 3)
        # First nested pass sees leaf before its parent; second pass resolves it.
        self.assertEqual(trace.count("_extract_nested_folders"), 3)

    def test_folder_query_and_hidden_are_bypassed_not_reimplemented(self) -> None:
        trace: list[str] = []
        actual = self.observe("folder_ignores_query_all", trace)
        self.assertNotIn("format_and_print_output", trace)
        self.assertNotIn("search", trace)
        self.assertNotIn("hidden_data", json.loads(actual["stdout"])["result"])
        trace.clear()
        self.observe("query_false", trace)
        self.assertIn("format_and_print_output", trace)
        self.assertIn("search", trace)

    def test_nonprogress_is_classified_before_any_product_function(self) -> None:
        for identifier in (
            "orphan_graph",
            "null_parent_graph",
            "nonstring_parent_graph",
            "cycle_graph",
            "self_cycle_graph",
        ):
            with self.subTest(case=identifier):
                inputs = REFERENCE.case_input(self.cases[identifier], python=True)
                with patch.object(
                    REFERENCE.OUTPUT,
                    "Reference",
                    side_effect=AssertionError("source must not be initialized"),
                ):
                    with self.assertRaises(REFERENCE.SourceNonprogress):
                        REFERENCE.observe(inputs, self.sources)
                self.assertFalse(
                    REFERENCE.graph_evidence(inputs["responses"][1]["body"]["value"])[
                        "terminates"
                    ]
                )

    def test_safety_evidence_does_not_substitute_for_valid_graph_source(self) -> None:
        for identifier in (
            "root_valid",
            "child_before_parent",
            "duplicate_first_match",
        ):
            with self.subTest(case=identifier):
                inputs = REFERENCE.case_input(self.cases[identifier], python=True)
                rows = inputs["responses"][1]["body"]["value"]
                self.assertTrue(REFERENCE.graph_evidence(rows)["terminates"])
                actual = REFERENCE.observe(inputs, self.sources)
                REFERENCE.assert_observation(
                    self.cases[identifier], actual, python=True
                )

    def test_unconsumed_or_missing_source_response_scripts_fail(self) -> None:
        inputs = REFERENCE.case_input(self.cases["empty_root"], python=True)
        inputs["responses"].append({"status": 200, "body": {"value": []}})
        with self.assertRaisesRegex(AssertionError, "unused scripted"):
            REFERENCE.observe(inputs, self.sources)
        inputs["responses"] = inputs["responses"][:1]
        with self.assertRaisesRegex(REFERENCE.OfflineViolation, "Unscripted"):
            REFERENCE.observe(inputs, self.sources)

    def test_cancellation_is_native_only_no_fake_source_cancellation(self) -> None:
        case = self.cases["cancel_before"]
        native = REFERENCE.expected_observation(case)
        python = self.observe(case["id"])
        self.assertEqual(native["requests"], [])
        self.assertEqual(native["scopes"], [])
        self.assertEqual(native["exit_code"], 1)
        self.assertEqual(len(python["requests"]), 2)
        self.assertEqual(python["exit_code"], 0)
        self.assertEqual(case["category"], "safety")

    def test_query_numeric_and_scalar_limits_stay_separate(self) -> None:
        self.assertEqual(
            REFERENCE.INVENTORY["unsupported"],
            ("query_wide_sum", "query_provider_disabled", "query_big_literal"),
        )
        for identifier in ("query_wide_sum", "query_big_literal"):
            with self.subTest(case=identifier):
                case = self.cases[identifier]
                python = self.observe(identifier)
                REFERENCE.assert_observation(case, python, python=True)
                native = REFERENCE.expected_observation(case)
                self.assertEqual(
                    json.loads(native["stdout"])["result"]["error_code"], "InvalidInput"
                )
                self.assertNotEqual(python, native)

    def test_explicit_noquery_is_a_native_only_configuration_difference(self) -> None:
        case = self.cases["query_provider_disabled"]
        self.assertEqual(REFERENCE.case_input(case)["config"]["projection"], "none")
        python = self.observe(case["id"])
        REFERENCE.assert_observation(case, python, python=True)
        self.assertEqual(json.loads(python["stdout"])["result"]["data"], ["0"])
        native = REFERENCE.expected_observation(case)
        self.assertEqual(
            json.loads(native["stdout"])["result"],
            {
                "error_code": "NotSupported",
                "message": "Query projection and GET path discovery require a configured projection provider",
            },
        )
        self.assertNotEqual(python, native)

    def test_native_graph_safety_is_not_reclassified_as_query_unsupported(self) -> None:
        boundaries = {
            "orphan_graph": "UnresolvedParent",
            "null_parent_graph": "NullParent",
            "nonstring_parent_graph": "NonStringParent",
            "cycle_graph": "Cycle",
            "self_cycle_graph": "Cycle",
        }
        for identifier, reason in boundaries.items():
            with self.subTest(case=identifier):
                case = self.cases[identifier]
                self.assertEqual(case["category"], "safety")
                result = json.loads(REFERENCE.expected_observation(case)["stdout"])[
                    "result"
                ]
                self.assertEqual(
                    result,
                    {
                        "error_code": "NotSupported",
                        "message": f"Folder graph is unsupported ({reason} safety boundary)",
                    },
                )
                self.assertTrue(case["python_nonprogress"])
        for identifier in ("item_null_folder", "item_orphan_folder"):
            with self.subTest(case=identifier):
                case = self.cases[identifier]
                self.assertEqual(case["category"], "safety")
                result = json.loads(REFERENCE.expected_observation(case)["stdout"])[
                    "result"
                ]
                self.assertEqual(
                    result,
                    {
                        "error_code": "NotSupported",
                        "message": "Folder item parent is unsupported (ItemParent safety boundary)",
                    },
                )

    def test_no_ambient_file_network_auth_process_environment_or_sleep(self) -> None:
        operations = {
            "read": lambda: builtins.open("forbidden-folder-reference", "rb"),
            "write": lambda: io.open("forbidden-folder-reference", "wb"),
            "network": lambda: socket.socket(),
            "dns": lambda: socket.getaddrinfo("outside.invalid", 443),
            "auth": lambda: builtins.__import__("fabric_cli.core.fab_auth"),
            "process": lambda: subprocess.Popen(["forbidden-folder-reference"]),
            "sleep": lambda: time.sleep(0),
            "environment-write": lambda: os.putenv("FORBIDDEN_FOLDER_ORACLE", "1"),
            "filesystem": lambda: os.mkdir("forbidden-folder-reference"),
        }
        for name, operation in operations.items():
            with self.subTest(operation=name):
                with self.assertRaises(REFERENCE.OfflineViolation):
                    with REFERENCE.offline_boundary():
                        operation()

    def test_caught_forbidden_io_still_fails_the_oracle(self) -> None:
        with self.assertRaises(REFERENCE.OfflineViolation):
            with REFERENCE.offline_boundary():
                try:
                    builtins.open("forbidden-folder-reference", "rb")
                except REFERENCE.OfflineViolation:
                    pass

    def test_product_function_side_effect_is_blocked_at_execution_boundary(
        self,
    ) -> None:
        sources = copy.deepcopy(self.sources)
        path = "utils/fab_cmd_fs_utils.py"
        tree = sources[path]
        poisoned = ast.parse(
            "def get_ws_elements(parent):\n"
            "    open('forbidden-folder-reference', 'wb')\n"
        ).body[0]
        tree.body = [
            (
                poisoned
                if isinstance(node, ast.FunctionDef) and node.name == "get_ws_elements"
                else node
            )
            for node in tree.body
        ]
        with self.assertRaises(REFERENCE.OfflineViolation):
            REFERENCE.observe(
                REFERENCE.case_input(self.cases["empty_root"], python=True), sources
            )

    def test_caught_product_import_attempt_is_not_silently_allowed(self) -> None:
        sources = copy.deepcopy(self.sources)
        path = "utils/fab_cmd_fs_utils.py"
        poisoned = ast.parse(
            "def get_ws_elements(parent):\n"
            "    try:\n"
            "        __import__('msal')\n"
            "    except Exception:\n"
            "        pass\n"
            "    return []\n"
        ).body[0]
        sources[path].body = [
            (
                poisoned
                if isinstance(node, ast.FunctionDef) and node.name == "get_ws_elements"
                else node
            )
            for node in sources[path].body
        ]
        with self.assertRaisesRegex(REFERENCE.OfflineViolation, "even if caught"):
            REFERENCE.observe(
                REFERENCE.case_input(self.cases["empty_root"], python=True), sources
            )

    def test_page_aggregation_precedes_graph_safety_proof(self) -> None:
        case = self.cases["folder_pagination"]
        inputs = REFERENCE.case_input(case, python=True)
        first_page = inputs["responses"][1]["body"]["value"]
        self.assertFalse(REFERENCE.graph_evidence(first_page)["terminates"])
        # The complete actual source API response has the root on page two.
        actual = REFERENCE.observe(inputs, self.sources)
        REFERENCE.assert_observation(case, actual, python=True)
        self.assertEqual(len(actual["requests"]), 3)

    def test_context_and_response_objects_remain_immutable(self) -> None:
        inputs = REFERENCE.case_input(self.cases["nested_context"], python=True)
        original = copy.deepcopy(inputs)
        first = REFERENCE.observe(inputs, self.sources)
        second = REFERENCE.observe(inputs, self.sources)
        self.assertEqual(inputs, original)
        self.assertEqual(first, second)

    def test_wrong_output_flag_and_unassigned_paths_fail_explicitly(self) -> None:
        for command in (
            ["ls", "--output", "json"],
            ["get", ".", "--output_format", "json"],
            ["ls", "/Other.Workspace", "--output_format", "json"],
        ):
            with self.subTest(command=command):
                inputs = REFERENCE.case_input(self.cases["empty_root"])
                inputs["command"] = command
                with self.assertRaises(ValueError):
                    REFERENCE.observe(inputs, self.sources)


if __name__ == "__main__":
    unittest.main()

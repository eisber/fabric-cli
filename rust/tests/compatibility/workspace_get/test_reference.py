# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Frozen source, protocol, inventory, provenance and offline-boundary guards."""

from __future__ import annotations

import builtins
import copy
import importlib.util
import json
import socket
import subprocess
import sys
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import patch

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "_workspace_get_reference_tests", HERE / "reference.py"
)
assert SPEC is not None and SPEC.loader is not None
REFERENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REFERENCE)

EXPECTED_IDS = (
    "ordered_unknown", "replace_existing", "empty_collections", "single_collections",
    "paged_collections", "spark_paging_quirk", "primary_paging_quirk",
    "primary_201", "primary_204", "primary_202_job_location",
    "primary_201_paged_to_200", "optional_201", "optional_204",
    "mpe_401", "mpe_403", "mpe_404", "spark_401", "spark_403", "spark_404",
    "acl_401", "acl_403", "acl_404", "invalid_mpe_json", "missing_mpe_value",
    "invalid_spark_json", "invalid_acl_json", "missing_acl_value",
    "failed_enrichment_keeps_existing", "partial_mpe_failure", "partial_acl_failure",
    "query_none", "query_empty", "query_dot", "query_quoted", "query_false",
    "query_numeric", "query_zero", "query_null", "query_empty_string",
    "query_empty_list", "query_empty_object", "query_invalid", "opaque_unicode",
    "primary_401", "primary_403", "primary_404", "primary_400", "primary_418",
    "primary_400_malformed", "primary_malformed", "primary_null",
    "query_wide_sum", "query_big_literal", "query_type_error",
    "empty_primary_optional_errors", "optional_201_paged_to_200",
    "optional_continuation_201", "primary_partial_failure", "spark_null_payload",
    "optional_nonlist_value", "query_outer_quotes_stripped",
)
BOUNDARY_IDS = {
    "native-safety": (
        "primary_malformed", "primary_null", "optional_nonlist_value",
    ),
    "unsupported": ("query_wide_sum", "query_big_literal"),
}


class WorkspaceGetReferenceTests(unittest.TestCase):
    """Use real source execution; no tests rewrite or generate expected outputs."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = REFERENCE.load_cases()
        cls.status_manifest = REFERENCE.load_status_cases()
        cls.truthiness_manifest = REFERENCE.load_truthiness_cases()
        cls.sources = REFERENCE.load_sources()
        cls.by_id = {case["id"]: case for case in cls.manifest["cases"]}

    def test_all_hand_authored_source_outcomes(self) -> None:
        for manifest in (
            self.manifest, self.status_manifest, self.truthiness_manifest
        ):
            for case in manifest["cases"]:
                with self.subTest(case=case["id"]):
                    returned: list[Any] = []
                    actual = REFERENCE.observe(
                        REFERENCE.inputs_for(manifest, case), self.sources, returned,
                        dispatch=case.get("source_dispatch", False),
                    )
                    REFERENCE.assert_observation(manifest, case, actual)
                    if "returned" in case["expected"]:
                        self.assertEqual(returned, [case["expected"]["returned"]])

    def test_additive_status_cases_are_frozen_and_not_blanket_parity(self) -> None:
        raw = (HERE / "status-cases.json").read_bytes()
        self.assertEqual(
            REFERENCE.blob_identity(raw),
            "fb605e652be16b395c59d51ff9f22b6726744276",
        )
        self.assertEqual(
            tuple(case["id"] for case in self.status_manifest["cases"]),
            (
                "status_primary_201",
                "status_primary_204",
                "status_optional_201",
                "status_optional_200_then_201",
                "status_primary_201_then_200",
                "status_optional_201_then_200",
            ),
        )
        self.assertEqual(
            [case["id"] for case in self.status_manifest["cases"] if case["category"] == "exact"],
            ["status_optional_201"],
        )
        changed = copy.deepcopy(self.status_manifest)
        changed["cases"][3]["category"] = "exact"
        with self.assertRaises(AssertionError):
            REFERENCE.validate_status_manifest(changed, raw)
        with self.assertRaises(AssertionError):
            REFERENCE.validate_status_manifest(changed, json.dumps(changed).encode())
        for case in self.status_manifest["cases"]:
            if case["category"] != "exact":
                self.assertNotEqual(
                    REFERENCE.expected_observation(self.status_manifest, case),
                    REFERENCE.expected_observation(
                        self.status_manifest, case, native=True
                    ),
                )

    def test_status_native_script_removes_only_unrequested_continuation(self) -> None:
        case = self.status_manifest["cases"][-1]
        source = REFERENCE.inputs_for(self.status_manifest, case)
        native = REFERENCE.inputs_for(self.status_manifest, case, native=True)
        self.assertEqual(source["responses"][:2], native["responses"][:2])
        self.assertEqual(source["responses"][3:], native["responses"][2:])
        self.assertEqual(source["responses"][2], {
            "status": 200, "body": {"value": [{"id": "last"}]}
        })
        self.assertEqual(
            {key: value for key, value in source.items() if key != "responses"},
            {key: value for key, value in native.items() if key != "responses"},
        )
        for manifest_case in self.status_manifest["cases"]:
            for is_native in (False, True):
                inputs = REFERENCE.inputs_for(
                    self.status_manifest, manifest_case, native=is_native
                )
                self.assertEqual(
                    set(inputs), {"id", "command", "current_workspace", "responses"}
                )

    def test_complete_dispatch_truthiness_cases_are_independently_frozen(self) -> None:
        raw = (HERE / "truthiness-cases.json").read_bytes()
        self.assertEqual(
            REFERENCE.blob_identity(raw),
            "3cf5447ad47fe7afd23080be37961e0de16d09b9",
        )
        self.assertEqual(
            tuple(case["id"] for case in self.truthiness_manifest["cases"]),
            (
                "dispatch_query_empty_role_assignments",
                "dispatch_query_empty_spark_settings",
                "dispatch_query_false_scalar",
            ),
        )
        self.assertTrue(all(
            case["category"] == "exact" and case["source_dispatch"] is True
            for case in self.truthiness_manifest["cases"]
        ))
        changed = copy.deepcopy(self.truthiness_manifest)
        changed["cases"][0]["expected"]["output"] = {
            "status": "Success", "command": "get", "result": {"data": []}
        }
        with self.assertRaises(AssertionError):
            REFERENCE._validate_manifest(
                changed, raw, REFERENCE.TRUTHINESS_MANIFEST_BLOB
            )
        with self.assertRaises(AssertionError):
            REFERENCE._validate_manifest(
                changed, json.dumps(changed).encode(),
                REFERENCE.TRUTHINESS_MANIFEST_BLOB,
            )

    def test_complete_main_dispatch_does_not_add_an_empty_success_envelope(self) -> None:
        for case in self.truthiness_manifest["cases"]:
            returned: list[Any] = []
            actual = REFERENCE.observe(
                REFERENCE.inputs_for(self.truthiness_manifest, case),
                self.sources, returned, dispatch=True,
            )
            REFERENCE.assert_observation(self.truthiness_manifest, case, actual)
            self.assertEqual(len(returned), 1)
            self.assertEqual(returned[0]["roleAssignments"], [])
            self.assertEqual(returned[0]["sparkSettings"], {})
            self.assertEqual(actual["exit_code"], 0)
            self.assertEqual(len(actual["requests"]), 4)
            self.assertEqual(actual["scopes"], [REFERENCE.SCOPE] * 4)
            self.assertEqual(actual["stderr"], "")
            if case["query"] == "flag":
                self.assertEqual(
                    json.loads(actual["stdout"])["result"]["data"], ["False"]
                )
                self.assertNotIn("UnexpectedError", actual["stdout"])
            else:
                self.assertEqual(actual["stdout"], "")
                wrong = dict(actual)
                wrong["stdout"] = json.dumps({
                    "timestamp": REFERENCE.TIMESTAMP,
                    "status": "Success", "command": "get",
                    "result": {"data": []},
                }, indent=4) + "\n"
                with self.assertRaises(AssertionError):
                    REFERENCE.assert_observation(
                        self.truthiness_manifest, case, wrong
                    )
            inputs = REFERENCE.inputs_for(self.truthiness_manifest, case, native=True)
            self.assertEqual(
                set(inputs), {"id", "command", "current_workspace", "responses"}
            )

    def test_inventory_and_difference_denominators_are_fixed(self) -> None:
        cases = self.manifest["cases"]
        self.assertEqual(tuple(case["id"] for case in cases), EXPECTED_IDS)
        self.assertEqual(len(cases), 61)
        self.assertEqual(sum(case["category"] == "exact" for case in cases), 56)
        for category, identifiers in BOUNDARY_IDS.items():
            self.assertEqual(
                tuple(case["id"] for case in cases if case["category"] == category),
                identifiers,
            )
        for case in cases:
            if case["category"] != "exact":
                source = REFERENCE.expected_observation(self.manifest, case)
                native = REFERENCE.expected_observation(self.manifest, case, native=True)
                self.assertNotEqual(source, native)
                self.assertEqual(
                    native["exit_code"], 0 if case["id"] == "optional_nonlist_value" else 1
                )
                self.assertTrue(case["boundary"])
                self.assertEqual(
                    set(native),
                    {"id", "exit_code", "stdout", "stderr", "requests", "scopes"},
                )

    def test_manifest_original_bytes_are_pinned(self) -> None:
        raw = (HERE / "cases.json").read_bytes()
        self.assertEqual(
            REFERENCE.blob_identity(raw),
            "f63fd4307f8712504c23c38e381c6ea994696e9e",
        )
        REFERENCE.validate_manifest(self.manifest, raw)
        for mutation in ("expectation", "input", "category", "removal", "provenance"):
            manifest = copy.deepcopy(self.manifest)
            if mutation == "expectation":
                manifest["cases"][0]["expected"]["output"]["result"]["data"] = []
            elif mutation == "input":
                manifest["scripts"]["empty"][0]["status"] = 201
            elif mutation == "category":
                manifest["cases"][0]["category"] = "unsupported"
            elif mutation == "removal":
                manifest["cases"].pop()
            else:
                manifest["source_commit"] = "0" * 40
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                REFERENCE.validate_manifest(manifest, raw)
            with self.subTest(reencoded=mutation), self.assertRaises(AssertionError):
                REFERENCE.validate_manifest(manifest, json.dumps(manifest).encode())
        with self.assertRaises(AssertionError):
            REFERENCE.validate_manifest(self.manifest, raw + b"\n")

    def test_only_crlf_checkout_normalization_is_permitted(self) -> None:
        self.assertEqual(
            REFERENCE.blob_identity(b"a\nb\n"),
            REFERENCE.blob_identity(b"a\r\nb\r\n"),
        )
        self.assertNotEqual(
            REFERENCE.blob_identity(b"a\nb\n"),
            REFERENCE.blob_identity(b"a\nb"),
        )

    def test_complete_source_tree_and_executed_blobs_are_verified(self) -> None:
        self.assertEqual(len(REFERENCE.BLOBS), 21)
        self.assertEqual(
            REFERENCE.COMMANDS.SOURCE_PIN,
            {
                "commit": "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4",
                "source_file_count": 259,
                "source_tree_sha256": "b044061fc4d8e43d104addf0de4d6860451ebd9fd0f5b0ddf81d5de1da3b41ce",
            },
        )
        with patch.object(REFERENCE, "_git", return_value="0" * 40):
            with self.assertRaisesRegex(AssertionError, "Git tree"):
                REFERENCE.verify_source()
        original = Path.read_bytes
        target = REFERENCE.ROOT / "src" / "fabric_cli" / "client" / "fab_api_workspace.py"

        def changed(path: Path) -> bytes:
            content = original(path)
            return content + b"\n# drift\n" if path == target else content

        with patch.object(Path, "read_bytes", changed):
            with self.assertRaisesRegex(AssertionError, "source drift"):
                REFERENCE.verify_source()

    def test_support_oracles_are_content_pinned_and_read_only(self) -> None:
        self.assertEqual(
            set(REFERENCE.SUPPORT_BLOBS), {"commands", "items", "output", "workspaces"}
        )
        original = Path.read_bytes
        target = HERE.parent / "commands" / "reference.py"

        def changed(path: Path) -> bytes:
            return b"changed old oracle" if path == target else original(path)

        with patch.object(Path, "read_bytes", changed):
            with self.assertRaisesRegex(AssertionError, "Protected read-only"):
                REFERENCE._support()

    def test_native_adapter_contains_only_actual_host_inputs(self) -> None:
        for case in self.manifest["cases"]:
            with self.subTest(case=case["id"]):
                inputs = REFERENCE.inputs_for(self.manifest, case)
                self.assertEqual(
                    set(inputs), {"id", "command", "current_workspace", "responses"}
                )
                self.assertEqual(inputs["command"][:4], ["get", ".", "--output_format", "json"])
                self.assertEqual(inputs["id"], case["id"])
                self.assertNotIn("expected", json.dumps(inputs))
        self.assertEqual(
            REFERENCE.inputs_for(self.manifest, self.by_id["query_none"])["command"],
            ["get", ".", "--output_format", "json"],
        )
        self.assertEqual(
            REFERENCE.inputs_for(self.manifest, self.by_id["query_empty"])["command"][-2:],
            ["--query", ""],
        )

    def test_unconsumed_or_unexpected_requests_fail_even_if_source_catches(self) -> None:
        case = self.by_id["single_collections"]
        for kind in ("extra", "missing"):
            inputs = REFERENCE.inputs_for(self.manifest, case)
            if kind == "extra":
                inputs["responses"].append({"status": 200, "body": {}})
                expected_error = AssertionError
            else:
                inputs["responses"].pop()
                expected_error = REFERENCE.OfflineViolation
            with self.subTest(kind=kind), self.assertRaises(expected_error):
                REFERENCE.observe(inputs, self.sources)

    def test_primary_201_has_no_enrichment_no_output_and_empty_dict_return(self) -> None:
        case = self.by_id["primary_201"]
        returned: list[Any] = []
        actual = REFERENCE.observe(
            REFERENCE.inputs_for(self.manifest, case), self.sources, returned
        )
        self.assertEqual(returned, [{}])
        self.assertEqual(actual["stdout"], "")
        self.assertEqual(actual["stderr"], "")
        self.assertEqual(actual["exit_code"], 0)
        self.assertEqual(len(actual["requests"]), 1)
        self.assertEqual(actual["scopes"], [REFERENCE.SCOPE])

    def test_quoted_query_uses_actual_source_nargs_rules(self) -> None:
        for identifier, scalar in (
            ("query_quoted", "雪"),
            ("query_outer_quotes_stripped", "None"),
        ):
            case = self.by_id[identifier]
            actual = REFERENCE.observe(
                REFERENCE.inputs_for(self.manifest, case), self.sources
            )
            REFERENCE.assert_observation(self.manifest, case, actual)
            self.assertEqual(
                json.loads(actual["stdout"])["result"]["data"], [scalar]
            )

    def test_partial_collection_failures_are_not_partial_successes(self) -> None:
        for identifier, omitted in (
            ("partial_mpe_failure", "managedPrivateEndpoints"),
            ("partial_acl_failure", "roleAssignments"),
        ):
            case = self.by_id[identifier]
            actual = REFERENCE.observe(REFERENCE.inputs_for(self.manifest, case), self.sources)
            REFERENCE.assert_observation(self.manifest, case, actual)
            data = json.loads(actual["stdout"])["result"]["data"][0]
            self.assertNotIn(omitted, data)
            self.assertEqual(len(actual["requests"]), 5)
            self.assertNotIn("partial", actual["stdout"])

    def test_source_body_and_request_order_are_not_set_comparisons(self) -> None:
        case = self.by_id["replace_existing"]
        actual = REFERENCE.observe(REFERENCE.inputs_for(self.manifest, case), self.sources)
        data = json.loads(actual["stdout"])["result"]["data"][0]
        self.assertEqual(
            list(data), ["roleAssignments", "z", "sparkSettings", "managedPrivateEndpoints", "a"]
        )
        for field, value in (
            ("requests", actual["requests"][::-1]),
            ("scopes", actual["scopes"][:-1]),
            ("stdout", actual["stdout"].replace("\n", "\r\n")),
            ("stdout", actual["stdout"].replace('"z": 0', '"z": 0.0')),
            ("exit_code", 1),
            ("stderr", "diagnostic\n"),
            ("id", "different"),
        ):
            broken = {**actual, field: value}
            with self.subTest(field=field), self.assertRaises(AssertionError):
                REFERENCE.assert_observation(self.manifest, case, broken)
        broken = {**actual, "extra_protocol_field": True}
        with self.assertRaises(AssertionError):
            REFERENCE.assert_observation(self.manifest, case, broken)

    def test_all_ambient_boundaries_are_denied_without_side_effects(self) -> None:
        actions = {
            "read": lambda: (HERE / "cases.json").read_bytes(),
            "write": lambda: (HERE / "forbidden-write").write_text("not written"),
            "network": lambda: socket.getaddrinfo("offline.invalid", 443),
            "identity": lambda: builtins.__import__("msal"),
            "process": lambda: subprocess.run(["must-not-run"], check=True),
            "sleep": lambda: time.sleep(1),
        }
        for name, action in actions.items():
            with self.subTest(boundary=name), self.assertRaises(REFERENCE.OfflineViolation):
                with REFERENCE.COMMANDS.offline_boundary():
                    action()
        self.assertFalse((HERE / "forbidden-write").exists())

    def test_caught_ambient_violation_still_invalidates_the_observation(self) -> None:
        def forbidden_query(*unused: Any, **kwargs: Any) -> Any:
            return builtins.__import__("azure.identity")

        case = self.by_id["query_false"]
        with patch.object(REFERENCE.jmespath, "search", forbidden_query):
            with self.assertRaises(REFERENCE.OfflineViolation):
                REFERENCE.observe(REFERENCE.inputs_for(self.manifest, case), self.sources)
        with self.assertRaises(REFERENCE.OfflineViolation):
            with REFERENCE.COMMANDS.offline_boundary():
                try:
                    builtins.open("must-not-open")
                except REFERENCE.OfflineViolation:
                    pass

    def test_native_safety_and_unsupported_require_actual_full_protocol(self) -> None:
        for category, identifiers in BOUNDARY_IDS.items():
            for identifier in identifiers:
                case = self.by_id[identifier]
                expected = REFERENCE.expected_observation(self.manifest, case, native=True)
                REFERENCE.assert_observation(self.manifest, case, expected, native=True)
                source = REFERENCE.expected_observation(self.manifest, case)
                with self.subTest(category=category, case=identifier):
                    with self.assertRaises(AssertionError):
                        REFERENCE.assert_observation(self.manifest, case, source, native=True)
                    for field in ("stdout", "requests", "scopes", "exit_code"):
                        broken = dict(expected)
                        broken[field] = (
                            ""
                            if field == "stdout"
                            else 1 - expected["exit_code"]
                            if field == "exit_code"
                            else []
                        )
                        with self.assertRaises(AssertionError):
                            REFERENCE.assert_observation(self.manifest, case, broken, native=True)

    def test_native_jsonl_transport_is_existing_read_probe_not_fixture_replay(self) -> None:
        cases = [self.by_id["empty_collections"], self.by_id["primary_401"]]
        inputs = [REFERENCE.inputs_for(self.manifest, case) for case in cases]
        observations = [
            REFERENCE.expected_observation(self.manifest, case) for case in cases
        ]
        raw = "".join(json.dumps(row) + "\n" for row in observations).encode()
        probe = HERE / "explicit-caller-probe.exe"
        process = SimpleNamespace(returncode=0, stderr=b"", stdout=raw)
        with patch.object(Path, "is_file", return_value=True), patch.object(
            REFERENCE.COMMANDS.subprocess, "run", return_value=process
        ) as run:
            self.assertEqual(REFERENCE.COMMANDS.run_rust(probe, inputs), observations)
            transmitted = [
                json.loads(line)
                for line in run.call_args.kwargs["input"].decode().splitlines()
            ]
            self.assertEqual(transmitted, inputs)
            for invalid in (raw.rstrip(b"\n"), raw.replace(b"\n", b"\r\n"), b""):
                process.stdout = invalid
                with self.assertRaises(AssertionError):
                    REFERENCE.COMMANDS.run_rust(probe, inputs)
            process.stdout = b"".join(
                json.dumps(row).encode() + b"\n" for row in observations[::-1]
            )
            with self.assertRaises(AssertionError):
                REFERENCE.COMMANDS.run_rust(probe, inputs)
            process.stdout = raw
            process.stderr = b"unexpected diagnostic"
            with self.assertRaises(AssertionError):
                REFERENCE.COMMANDS.run_rust(probe, inputs)


if __name__ == "__main__":
    unittest.main()

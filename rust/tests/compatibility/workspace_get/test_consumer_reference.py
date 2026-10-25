# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independent consumer guards; run only through the host's shared OS mutex."""

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
    "_workspace_consumer_reference_tests", HERE / "consumer_reference.py"
)
assert SPEC is not None and SPEC.loader is not None
REFERENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REFERENCE)

EXPECTED_IDS = (
    "consumer_ordered_unknown",
    "consumer_enrichment_slots",
    "consumer_failed_slots",
    "consumer_empty",
    "consumer_single",
    "consumer_collections_paged",
    "consumer_mpe_401",
    "consumer_mpe_403",
    "consumer_mpe_404",
    "consumer_spark_401",
    "consumer_spark_403",
    "consumer_spark_404",
    "consumer_acl_401",
    "consumer_acl_403",
    "consumer_acl_404",
    "consumer_mpe_malformed",
    "consumer_mpe_missing_value",
    "consumer_spark_malformed",
    "consumer_spark_without_value",
    "consumer_acl_malformed",
    "consumer_acl_missing_value",
    "consumer_mpe_partial_403",
    "consumer_acl_partial_404",
    "consumer_primary_401",
    "consumer_primary_403",
    "consumer_primary_404",
    "consumer_primary_400",
    "consumer_primary_400_malformed",
    "consumer_primary_malformed",
    "consumer_primary_201",
    "consumer_primary_204",
    "consumer_query_none",
    "consumer_query_empty",
    "consumer_query_dot",
    "consumer_query_quoted",
    "consumer_query_false",
    "consumer_query_float",
    "consumer_query_zero",
    "consumer_query_numeric",
    "consumer_query_syntax_error",
    "consumer_query_type_error",
    "consumer_query_empty_list",
    "consumer_query_empty_object",
    "consumer_query_empty_string",
    "consumer_personal_unicode_opaque",
    "consumer_primary_continuation",
    "consumer_spark_continuation",
    "consumer_foreign_mpe_continuation",
    "consumer_trailing_201",
    "consumer_trailing_204",
    "consumer_optional_201_then_200",
    "consumer_optional_201_single",
    "consumer_mpe_null_value",
    "consumer_acl_nonlist_value",
    "consumer_spark_null",
    "consumer_spark_array",
    "consumer_query_wide_sum",
    "consumer_query_big_literal",
)


class ConsumerReferenceTests(unittest.TestCase):
    """Require all source, byte protocol, valid-domain and safety denominators."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest = REFERENCE.load_cases()
        cls.sources = REFERENCE.SOURCE.load_sources()
        cls.by_id = {case["id"]: case for case in cls.manifest["cases"]}

    def test_all_frozen_consumer_source_protocols(self) -> None:
        for case in self.manifest["cases"]:
            with self.subTest(case=case["id"]):
                actual = REFERENCE.observe(self.manifest, case, self.sources)
                REFERENCE.SOURCE.assert_observation(self.manifest, case, actual)

    def test_consumer_inventory_is_fixed_and_separately_classified(self) -> None:
        cases = self.manifest["cases"]
        self.assertEqual(tuple(case["id"] for case in cases), EXPECTED_IDS)
        self.assertEqual(len(cases), 58)
        self.assertEqual(sum(case["category"] == "exact" for case in cases), 44)
        self.assertEqual(
            tuple(case["id"] for case in cases if case["category"] == "native-safety"),
            REFERENCE.SAFETY_IDS,
        )
        self.assertEqual(
            tuple(case["id"] for case in cases if case["category"] == "unsupported"),
            REFERENCE.UNSUPPORTED_IDS,
        )
        self.assertEqual(len(REFERENCE.SAFETY_IDS), 12)
        self.assertEqual(len(REFERENCE.UNSUPPORTED_IDS), 2)
        for case in cases:
            if case["category"] != "exact":
                self.assertNotEqual(
                    REFERENCE.SOURCE.expected_observation(self.manifest, case),
                    REFERENCE.SOURCE.expected_observation(
                        self.manifest, case, native=True
                    ),
                )

    def test_complete_handwritten_manifest_is_content_pinned(self) -> None:
        raw = (HERE / "consumer-cases.json").read_bytes()
        self.assertEqual(
            REFERENCE.blob_identity(raw),
            "7e7c3dd435dd241979da16c1cb6c3dcefd679a23",
        )
        REFERENCE.validate_manifest(self.manifest, raw)
        for change in ("expected", "input", "label", "native", "removal", "policy"):
            manifest = copy.deepcopy(self.manifest)
            if change == "expected":
                manifest["cases"][0]["expected"]["output"] = None
            elif change == "input":
                manifest["scripts"]["empty"][0]["body"].pop("displayName")
            elif change == "label":
                manifest["cases"][-1]["category"] = "exact"
            elif change == "native":
                manifest["cases"][29]["native_expected"]["exit_code"] = 0
            elif change == "removal":
                manifest["cases"].pop()
            else:
                manifest["policy"]["trailing_201_204"] = "ignore failure"
            with self.subTest(change=change):
                with self.assertRaises(AssertionError):
                    REFERENCE.validate_manifest(manifest, raw)
                with self.assertRaises(AssertionError):
                    REFERENCE.validate_manifest(manifest, json.dumps(manifest).encode())

    def test_every_nominal_primary_has_identity_fields(self) -> None:
        for case in self.manifest["cases"]:
            inputs = REFERENCE.inputs_for(self.manifest, case)
            primary = inputs["responses"][0]
            if primary["status"] in {200, 201}:
                if case["id"] == "consumer_primary_malformed":
                    self.assertEqual(primary["body"], "not-json")
                    continue
                with self.subTest(case=case["id"]):
                    self.assertIsInstance(primary["body"], dict)
                    self.assertIsInstance(primary["body"]["id"], str)
                    self.assertIsInstance(primary["body"]["displayName"], str)
        for field in ("id", "displayName"):
            for invalid in ("missing", None, 7, False, [], {}):
                manifest = copy.deepcopy(self.manifest)
                primary = manifest["cases"][0]["input"]["responses"][0]["body"]
                if invalid == "missing":
                    primary.pop(field)
                else:
                    primary[field] = invalid
                raw = json.dumps(manifest).encode()
                with (
                    self.subTest(field=field, invalid=invalid),
                    patch.object(
                        REFERENCE,
                        "blob_identity",
                        return_value=REFERENCE.MANIFEST_BLOB,
                    ),
                    self.assertRaisesRegex(AssertionError, "id/displayName"),
                ):
                    REFERENCE.validate_manifest(manifest, raw)

    def test_prior_70_fixtures_runner_guards_and_docs_remain_immutable(self) -> None:
        self.assertEqual(len(REFERENCE.IMMUTABLE_BASE_BLOBS), 7)
        self.assertEqual(
            REFERENCE.BASE_REFERENCE_BLOB,
            "2c2166f14b793e91b86bafe389fb3ce40a60cb50",
        )
        REFERENCE.verify_immutable_base()
        self.assertEqual(
            REFERENCE.SOURCE.MANIFEST_BLOB, "f63fd4307f8712504c23c38e381c6ea994696e9e"
        )
        self.assertEqual(
            REFERENCE.SOURCE.STATUS_MANIFEST_BLOB,
            "fb605e652be16b395c59d51ff9f22b6726744276",
        )
        self.assertEqual(
            REFERENCE.SOURCE.TRUTHINESS_MANIFEST_BLOB,
            "3cf5447ad47fe7afd23080be37961e0de16d09b9",
        )
        original = Path.read_bytes

        def changed(path: Path) -> bytes:
            return b"changed" if path == HERE / "test_reference.py" else original(path)

        with patch.object(Path, "read_bytes", changed):
            with self.assertRaisesRegex(AssertionError, "70-case evidence"):
                REFERENCE.verify_immutable_base()

    def test_native_inputs_have_no_expected_values_or_fixture_logic(self) -> None:
        for case in self.manifest["cases"]:
            for native in (False, True):
                with self.subTest(case=case["id"], native=native):
                    inputs = REFERENCE.inputs_for(self.manifest, case, native=native)
                    self.assertEqual(
                        set(inputs),
                        {"id", "command", "current_workspace", "responses", "raw_argv"},
                    )
                    self.assertIs(inputs["raw_argv"], True)
                    self.assertEqual(
                        inputs["command"][:4],
                        ["get", ".", "--output_format", "json"],
                    )
                    self.assertNotIn("expected", json.dumps(inputs))
                    expected = REFERENCE.SOURCE.expected_observation(
                        self.manifest, case, native=native
                    )
                    self.assertEqual(
                        len(inputs["responses"]), len(expected["requests"])
                    )
                    self.assertEqual(
                        expected["scopes"],
                        [REFERENCE.SOURCE.SCOPE] * len(expected["requests"]),
                    )
                    self.assertEqual(expected["stderr"], "")

    def test_native_pruning_removes_only_frozen_unrequested_reply_suffixes(
        self,
    ) -> None:
        self.assertEqual(len(REFERENCE.PRUNED_RESPONSE_COUNTS), 5)
        for identifier, count in REFERENCE.PRUNED_RESPONSE_COUNTS.items():
            case = self.by_id[identifier]
            source = REFERENCE.inputs_for(self.manifest, case)
            native = REFERENCE.inputs_for(self.manifest, case, native=True)
            self.assertEqual(native["responses"], source["responses"][:count])
            self.assertLess(len(native["responses"]), len(source["responses"]))
            self.assertEqual(
                {key: value for key, value in native.items() if key != "responses"},
                {key: value for key, value in source.items() if key != "responses"},
            )
        manifest = copy.deepcopy(self.manifest)
        case = next(
            case for case in manifest["cases"] if case["id"] == "consumer_trailing_201"
        )
        case["native_input"]["responses"][-1]["status"] = 200
        raw = json.dumps(manifest).encode()
        with patch.object(
            REFERENCE, "blob_identity", return_value=REFERENCE.MANIFEST_BLOB
        ):
            with self.assertRaisesRegex(AssertionError, "unrequested reply suffixes"):
                REFERENCE.validate_manifest(manifest, raw)

    def test_both_probe_adapters_receive_the_same_raw_argv_protocol(self) -> None:
        for case in self.manifest["cases"]:
            for native in (False, True):
                with self.subTest(case=case["id"], native=native):
                    original = REFERENCE.SOURCE.inputs_for(
                        self.manifest, case, native=native
                    )
                    actual = REFERENCE.inputs_for(self.manifest, case, native=native)
                    self.assertEqual(actual, {**original, "raw_argv": True})
                    self.assertNotIn("raw_argv", original)

    def test_standard_fatal_native_policy_is_not_a_proposed_message(self) -> None:
        for identifier in (
            "consumer_primary_201",
            "consumer_primary_204",
            *REFERENCE.PRUNED_RESPONSE_COUNTS,
        ):
            expected = REFERENCE.SOURCE.expected_observation(
                self.manifest, self.by_id[identifier], native=True
            )
            output = json.loads(expected["stdout"])
            self.assertEqual(expected["exit_code"], 1)
            self.assertEqual(output["command"], "get")
            self.assertEqual(
                output["result"],
                {
                    "error_code": "ReadFailed",
                    "message": "Workspace metadata operation did not complete",
                },
            )
        expected = REFERENCE.SOURCE.expected_observation(
            self.manifest, self.by_id["consumer_primary_malformed"], native=True
        )
        self.assertEqual(
            json.loads(expected["stdout"])["result"],
            {
                "error_code": "InvalidResponse",
                "message": "Fabric returned invalid workspace metadata",
            },
        )

    def test_ordered_replacements_do_not_move_existing_slots(self) -> None:
        case = self.by_id["consumer_enrichment_slots"]
        actual = REFERENCE.observe(self.manifest, case, self.sources)
        REFERENCE.SOURCE.assert_observation(self.manifest, case, actual)
        metadata = json.loads(actual["stdout"])["result"]["data"][0]
        self.assertEqual(
            list(metadata),
            [
                "roleAssignments",
                "z",
                "sparkSettings",
                "displayName",
                "managedPrivateEndpoints",
                "id",
                "a",
            ],
        )
        self.assertEqual(list(metadata["managedPrivateEndpoints"][0]), ["z", "id", "a"])

    def test_optional_failures_continue_and_partial_service_failures_are_atomic(
        self,
    ) -> None:
        for identifier, field in (
            ("consumer_mpe_partial_403", "managedPrivateEndpoints"),
            ("consumer_acl_partial_404", "roleAssignments"),
        ):
            case = self.by_id[identifier]
            actual = REFERENCE.observe(self.manifest, case, self.sources)
            REFERENCE.SOURCE.assert_observation(self.manifest, case, actual)
            self.assertEqual(len(actual["requests"]), 5)
            metadata = json.loads(actual["stdout"])["result"]["data"][0]
            self.assertNotIn(field, metadata)
            self.assertNotIn("partial", actual["stdout"])
        for step in ("mpe", "spark", "acl"):
            for status in (401, 403, 404):
                case = self.by_id[f"consumer_{step}_{status}"]
                expected = REFERENCE.SOURCE.expected_observation(self.manifest, case)
                self.assertEqual(expected["exit_code"], 0)
                self.assertEqual(len(expected["requests"]), 4)
                self.assertTrue(expected["requests"][-1].endswith("/roleAssignments"))

    def test_whole_dispatch_empty_projection_is_zero_bytes_not_an_envelope(
        self,
    ) -> None:
        for suffix in ("list", "object", "string"):
            case = self.by_id[f"consumer_query_empty_{suffix}"]
            actual = REFERENCE.observe(self.manifest, case, self.sources)
            self.assertEqual(actual["stdout"], "")
            self.assertEqual(actual["stderr"], "")
            self.assertEqual(actual["exit_code"], 0)
            self.assertEqual(len(actual["requests"]), 4)
            wrong = dict(actual)
            wrong["stdout"] = (
                json.dumps(
                    {
                        "timestamp": REFERENCE.SOURCE.TIMESTAMP,
                        "status": "Success",
                        "command": "get",
                        "result": {"data": []},
                    },
                    indent=4,
                )
                + "\n"
            )
            with self.assertRaises(AssertionError):
                REFERENCE.SOURCE.assert_observation(self.manifest, case, wrong)
        case = self.by_id["consumer_query_false"]
        actual = REFERENCE.observe(self.manifest, case, self.sources)
        self.assertEqual(json.loads(actual["stdout"])["result"]["data"], ["False"])

    def test_optional_first_201_then_terminal_200_is_exact_not_a_waiver(self) -> None:
        case = self.by_id["consumer_optional_201_then_200"]
        self.assertEqual(case["category"], "exact")
        actual = REFERENCE.observe(self.manifest, case, self.sources)
        REFERENCE.SOURCE.assert_observation(self.manifest, case, actual)
        self.assertEqual(len(actual["requests"]), 6)
        self.assertEqual(
            REFERENCE.SOURCE.expected_observation(self.manifest, case),
            REFERENCE.SOURCE.expected_observation(self.manifest, case, native=True),
        )
        metadata = json.loads(actual["stdout"])["result"]["data"][0]
        self.assertEqual(
            metadata["managedPrivateEndpoints"], [{"id": "m-first"}, {"id": "m-last"}]
        )
        self.assertEqual(
            metadata["roleAssignments"], [{"id": "r-first"}, {"id": "r-last"}]
        )

    def test_complete_protocol_has_no_scope_stream_exit_or_order_waivers(self) -> None:
        case = self.by_id["consumer_single"]
        expected = REFERENCE.SOURCE.expected_observation(self.manifest, case)
        for key, value in (
            ("requests", expected["requests"][::-1]),
            ("scopes", []),
            ("stdout", expected["stdout"].replace("\n", "\r\n")),
            ("stderr", "SDK diagnostic\n"),
            ("exit_code", 1),
            ("id", "different"),
        ):
            with self.subTest(field=key), self.assertRaises(AssertionError):
                REFERENCE.SOURCE.assert_observation(
                    self.manifest, case, {**expected, key: value}
                )
        with self.assertRaises(AssertionError):
            REFERENCE.SOURCE.assert_observation(
                self.manifest, case, {**expected, "diagnostics": []}
            )


if __name__ == "__main__":
    unittest.main()

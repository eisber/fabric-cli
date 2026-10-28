# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Secret-free offline tests of checklist rejection, not of authentication."""

import copy
from datetime import datetime, timezone
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from typing import Any

import check_report as checker


ROOT = Path(__file__).parent
NOW = datetime(2026, 10, 28, 21, tzinfo=timezone.utc)


class ReportTests(unittest.TestCase):
    """Keep live evidence requirements distinct from synthetic test success."""

    def setUp(self) -> None:
        self.report = checker.load_json(ROOT / "synthetic-complete.json")

    def check(self, report: Any) -> str:
        """Validate in the explicit synthetic-only unit-test mode."""
        return checker.validate(report, synthetic_test_only=True, now=NOW)

    def assert_rejected(self, report: Any, code: str) -> None:
        """Assert the stable rejection code without printing report contents."""
        with self.assertRaises(checker.ReportError) as caught:
            self.check(report)
        self.assertEqual(str(caught.exception), code)

    def test_complete_fixture_is_only_synthetic(self) -> None:
        self.assertEqual(self.check(self.report), "synthetic_test_only_not_acceptance")
        with self.assertRaisesRegex(checker.ReportError, "synthetic_mode_mismatch"):
            checker.validate(self.report, now=NOW)
        self.report["synthetic_test_only"] = False
        with self.assertRaisesRegex(
            checker.ReportError, "synthetic_marker_in_live_report"
        ):
            checker.validate(self.report, now=NOW)

    def test_negative_fixture_matrix(self) -> None:
        fixture = checker.load_json(ROOT / "synthetic-negative-cases.json")
        self.assertTrue(fixture["synthetic_test_only"])
        self.assertEqual(fixture["base"], "synthetic-complete.json")
        for case in fixture["cases"]:
            with self.subTest(case=case["name"]):
                report = copy.deepcopy(self.report)
                target = report
                for component in case["path"][:-1]:
                    target = target[component]
                leaf = case["path"][-1]
                if case.get("delete"):
                    del target[leaf]
                else:
                    target[leaf] = case["value"]
                self.assert_rejected(report, case["error"])

    def test_every_required_case_cannot_be_omitted_duplicated_or_unproved(self) -> None:
        for index in range(len(self.report["cases"])):
            for operation in (
                "omit",
                "duplicate",
                "not_run",
                "unobserved",
                "no_receipt",
            ):
                with self.subTest(index=index, operation=operation):
                    report = copy.deepcopy(self.report)
                    if operation == "omit":
                        del report["cases"][index]
                        code = "incomplete_or_duplicate_cases"
                    elif operation == "duplicate":
                        report["cases"].append(copy.deepcopy(report["cases"][index]))
                        code = "incomplete_or_duplicate_cases"
                    elif operation == "not_run":
                        report["cases"][index]["status"] = "not_run"
                        code = "unproved_case"
                    elif operation == "unobserved":
                        report["cases"][index]["observation"] = "not_observed"
                        code = "unobserved_case"
                    else:
                        report["cases"][index]["evidence_ids"] = []
                        code = "missing_case_evidence"
                    self.assert_rejected(report, code)

    def test_duplicate_evidence_and_signoff_roles(self) -> None:
        self.report["evidence"].append(copy.deepcopy(self.report["evidence"][0]))
        self.assert_rejected(self.report, "duplicate_evidence_id")
        self.report["evidence"].pop()
        self.report["signoffs"][1]["role"] = self.report["signoffs"][0]["role"]
        self.assert_rejected(self.report, "missing_or_duplicate_signoff")

    def test_unreferenced_evidence(self) -> None:
        extra = copy.deepcopy(self.report["evidence"][0])
        extra["id"] = "synthetic-unused-receipt"
        self.report["evidence"].append(extra)
        self.assert_rejected(self.report, "unreferenced_evidence")

    def test_ambiguous_identity_targets_and_artifacts(self) -> None:
        for field, selected, code in (
            ("negative_tenant_id", "tenant_id", "ambiguous_identity_targets"),
            ("guest_home_tenant_id", "tenant_id", "ambiguous_identity_targets"),
            ("negative_account_ref", "account_ref", "ambiguous_identity_targets"),
            (
                "negative_artifact_sha256",
                "primary_artifact_sha256",
                "ambiguous_artifacts",
            ),
        ):
            with self.subTest(field=field):
                report = copy.deepcopy(self.report)
                report["scope"][field] = report["scope"][selected]
                self.assert_rejected(report, code)

    def test_changed_scope_requires_new_authorization(self) -> None:
        self.report["scope"]["fabric_host"] = "dailyapi.fabric.microsoft.com"
        self.assert_rejected(self.report, "authorization_scope_mismatch")

    def test_changed_receipt_requires_new_signoff(self) -> None:
        self.report["evidence"][0]["sha256"] = "1" * 64
        self.assert_rejected(self.report, "signoff_packet_mismatch")

    def test_empty_targets_and_unsupported_build(self) -> None:
        for field in ("tenant_id", "arm_subscription_id", "source_sha"):
            with self.subTest(field=field):
                report = copy.deepcopy(self.report)
                report["scope"][field] = "0" * len(report["scope"][field])
                if field.endswith("_id"):
                    report["scope"][field] = "00000000-0000-0000-0000-000000000000"
                self.assert_rejected(report, "empty_target")
        self.report["scope"]["os_build"] = "10.0.19045.1"
        self.assert_rejected(self.report, "unsupported_os_build")

    def test_unsupported_schema_keyword_fails_closed(self) -> None:
        with self.assertRaisesRegex(checker.ReportError, "unsupported_schema_keyword"):
            checker._schema("x", {"format": "uri"}, {})
        with self.assertRaisesRegex(
            checker.ReportError, "unsupported_schema_reference"
        ):
            checker._schema("x", {"$ref": "#/$defs/ref", "const": "ignored"}, {})

    def test_json_loader_is_bounded_and_strict(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "synthetic.json"
            for value, code in (
                ('{"a":1,"a":2}', "duplicate_json_key"),
                ('{"a":NaN}', "nonfinite_json_number"),
                ('{"a":Infinity}', "nonfinite_json_number"),
                ('{"a":', "invalid_json"),
                (" " * (checker.MAX_BYTES + 1), "report_too_large"),
            ):
                with self.subTest(code=code):
                    path.write_text(value, encoding="utf-8")
                    with self.assertRaisesRegex(checker.ReportError, code):
                        checker.load_json(path)

    def test_numeric_version_cannot_be_a_boolean(self) -> None:
        self.report["schema_version"] = True
        self.assert_rejected(self.report, "schema_type")

    def test_rebound_receipt_cannot_reuse_old_packet_signoffs(self) -> None:
        self.report["scope"]["resource_ids"] = ["88888888-2222-3333-4444-555555555555"]
        digest = checker.scope_digest(self.report["scope"])
        self.report["authorization"]["scope_sha256"] = digest
        for entry in self.report["evidence"] + self.report["signoffs"]:
            entry["scope_sha256"] = digest
        self.assert_rejected(self.report, "signoff_packet_mismatch")

    def test_cli_streams_and_exit_codes(self) -> None:
        command = [
            sys.executable,
            "-B",
            str(ROOT / "check_report.py"),
            str(ROOT / "synthetic-complete.json"),
        ]
        result = subprocess.run(command, capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertEqual(
            result.stderr, "identity_checklist_rejected: synthetic_mode_mismatch\n"
        )
        result = subprocess.run(
            command + ["--synthetic-test-only"],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "synthetic_test_only_not_acceptance\n")
        self.assertEqual(result.stderr, "")

    def test_cli_does_not_echo_unknown_fields_or_invalid_json(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "synthetic.json"
            for data in (
                json.dumps(
                    {**self.report, "synthetic-sensitive-key": "synthetic-secret"}
                ),
                '{"synthetic-sensitive-key": "synthetic-secret"',
            ):
                path.write_text(data, encoding="utf-8")
                result = subprocess.run(
                    [
                        sys.executable,
                        "-B",
                        str(ROOT / "check_report.py"),
                        str(path),
                        "--synthetic-test-only",
                    ],
                    capture_output=True,
                    text=True,
                    check=False,
                )
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stdout, "")
                self.assertNotIn("synthetic-secret", result.stderr)
                self.assertNotIn("synthetic-sensitive-key", result.stderr)
                self.assertTrue(
                    result.stderr.startswith("identity_checklist_rejected: ")
                )


if __name__ == "__main__":
    unittest.main()

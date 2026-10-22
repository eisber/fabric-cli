# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Exercise all eight reference cases and prove the comparator rejects drift."""

from __future__ import annotations

import copy
import sys
import unittest
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))

from reference import (  # noqa: E402
    assert_observation,
    load_cases,
    observe_case,
    probe_boundary,
    verify_source,
)


class WorkspaceReferenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        manifest = load_cases()
        verify_source(manifest["provenance"])
        cls.cases = {case["id"]: case for case in manifest["cases"]}
        cls.observed: dict[str, dict[str, Any]] = {}

    def check_case(self, case_id: str) -> None:
        """Check an independent golden and retain evidence for mutation checks."""
        observed = observe_case(case_id)
        assert_observation(self.cases[case_id], observed)
        self.observed[case_id] = observed

    def test_empty(self) -> None:
        self.check_case("empty")

    def test_names_types_order(self) -> None:
        self.check_case("names_types_order")

    def test_token_pagination(self) -> None:
        self.check_case("token_pagination")

    def test_query_projection(self) -> None:
        self.check_case("query_projection")

    def test_missing_value(self) -> None:
        self.check_case("missing_value")

    def test_forbidden_mapping(self) -> None:
        self.check_case("forbidden_mapping")

    def test_fabric_error(self) -> None:
        self.check_case("fabric_error")

    def test_root_from_workspace(self) -> None:
        self.check_case("root_from_workspace")

    def test_offline_guards(self) -> None:
        for probe in ("network", "write", "identity", "process", "sleep"):
            with self.subTest(probe=probe):
                self.assertEqual(probe_boundary(probe), {"probe": probe, "blocked": True})

    def test_reference_is_deterministic(self) -> None:
        for case_id in self.cases:
            with self.subTest(case=case_id):
                first = self.observed.get(case_id)
                if first is None:
                    first = observe_case(case_id)
                second = observe_case(case_id)
                assert_observation(self.cases[case_id], second)
                self.assertEqual(first, second)

    def test_source_drift_is_rejected(self) -> None:
        provenance = copy.deepcopy(load_cases()["provenance"])
        provenance["source_tree_sha256"] = "0" * 64
        with self.assertRaisesRegex(AssertionError, "Python source drift"):
            verify_source(provenance)

    def test_comparator_rejects_contract_drift(self) -> None:
        case = self.cases["token_pagination"]
        original = observe_case(case["id"])
        assert_observation(case, original)
        mutations = {
            "stdout": "{}\n",
            "stderr": "unexpected diagnostic\n",
            "exit_code": 0 if original["exit_code"] else 1,
            "requests": original["requests"][:-1],
            "credential_scopes": [["https://storage.azure.com/.default"]],
            "api_ids": list(reversed(original["api_ids"])),
            "api_total_pages": 3,
            "exception": {"type": "Unexpected"},
            "context_before": "/Other.Workspace",
            "context_after": "/Other.Workspace",
        }
        for field, value in mutations.items():
            with self.subTest(field=field):
                changed = copy.deepcopy(original)
                changed[field] = value
                with self.assertRaisesRegex(AssertionError, "contract mismatch"):
                    assert_observation(case, changed)


if __name__ == "__main__":
    unittest.main()

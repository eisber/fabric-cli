# Copyright (c) Microsoft Corporation. Licensed under the MIT License.
"""Stdlib invariants for the independent, offline source acceptance oracle."""

from __future__ import annotations

import ast
import builtins
from collections import Counter
import json
from pathlib import Path
import unittest
from unittest.mock import patch

import capture_inputs
import oracle


class SourceOracleTests(unittest.TestCase):
    """Test provenance, synthetic boundaries and observable original behavior."""

    @classmethod
    def setUpClass(cls) -> None:
        """Load fixed source bytes and frozen expectations."""
        cls.source = oracle.Source()
        cls.inventory = json.loads((oracle.FIXTURES / "inventory.json").read_text(encoding="utf-8"))
        cls.cases = json.loads((oracle.FIXTURES / "predicate_cases.json").read_text(encoding="utf-8"))
        cls.by_id = {case["id"]: case for case in cls.cases}
        cls.evaluations = json.loads((oracle.FIXTURES / "evaluation_cases.json").read_text(encoding="utf-8"))

    def test_all_frozen_expectations_match_original_source(self) -> None:
        """The normal entrypoint verifies every inventory field and captured case."""
        oracle.validate(self.source)

    def test_inventory_has_exact_source_categories_and_severities(self) -> None:
        """Guard against substitution of a smaller heuristic catalog."""
        self.assertEqual(len(self.inventory["rules"]), 64)
        self.assertEqual(
            Counter(r["category"] for r in self.inventory["rules"]),
            {"Performance": 24, "Error Prevention": 5, "DAX Expressions": 14,
             "Maintenance": 5, "Formatting": 14, "Naming Conventions": 2},
        )
        self.assertEqual(
            Counter(r["severity"] for r in self.inventory["rules"]),
            {"Warning": 47, "Info": 10, "Error": 7},
        )
        self.assertEqual(len({r["name"] for r in self.inventory["rules"]}), 64)

    def test_line_and_ast_provenance_match_each_original_lambda(self) -> None:
        """Metadata points at the original executable expression, not a port."""
        for rule, row in zip(self.inventory["rules"], self.source.rows):
            with self.subTest(rule=rule["ordinal"]):
                node = row.elts[4]
                self.assertEqual(rule["expressionLocation"], oracle.location(node))
                self.assertEqual(rule["expressionAstSha256"], oracle.ast_hash(node))
                self.assertEqual(rule["expression"], ast.get_source_segment(
                    self.source.text["_model_bpa_rules.py"], node
                ))

    def test_missing_metadata_is_null_not_invented_description_or_url(self) -> None:
        """Absent tuple members are preserved separately from authored empty text."""
        self.assertIsNone(self.inventory["rules"][0]["url"])
        self.assertIsNone(self.inventory["rules"][5]["description"])
        self.assertIsNone(self.inventory["rules"][5]["url"])

    def test_each_executed_rule_has_actual_positive_and_negative(self) -> None:
        """Only 62 expressions execute; live-only rules cannot masquerade as clean."""
        executed = {c["rule"] for c in self.cases}
        self.assertEqual(executed, set(range(1, 65)) - {14, 15})
        for ordinal in executed:
            for label, violates in (("positive", True), ("negative", False)):
                expected = self.by_id[f"r{ordinal:02d}-{label}"]["expected"]
                self.assertEqual(expected["outcome"], "predicate")
                self.assertIs(expected["violates"], violates)

    def test_case_builder_has_not_silently_changed_frozen_inputs(self) -> None:
        """Captures cannot be regenerated just to fit implementation behavior."""
        inputs = [{k: v for k, v in case.items() if k != "expected"} for case in self.cases]
        self.assertEqual(capture_inputs.predicate_inputs(), inputs)

    def test_changed_source_bytes_are_rejected_before_parsing(self) -> None:
        """Do not execute changed snapshots even when the changed Python parses."""
        original_read = Path.read_bytes

        def altered(path: Path) -> bytes:
            data = original_read(path)
            return data + b"\n" if path.name == "_model_bpa_rules.py" else data

        with patch.object(Path, "read_bytes", altered):
            with self.assertRaisesRegex(ValueError, "source hash mismatch"):
                oracle.Source()

    def test_module_bodies_and_external_imports_never_execute(self) -> None:
        """No sempy, pandas, product startup or actual TOM module is imported."""
        real_import = builtins.__import__

        def guarded(name: str, *args: object, **kwargs: object) -> object:
            if name.split(".")[0] in {"sempy", "sempy_labs", "pandas", "Microsoft", "pyspark", "IPython", "fabric_cli"}:
                raise AssertionError(f"actual external import attempted: {name}")
            return real_import(name, *args, **kwargs)

        with patch.object(builtins, "__import__", guarded):
            source = oracle.Source()
            for case in self.cases:
                self.assertEqual(oracle.predicate_result(source, case), case["expected"])
            for case in self.evaluations:
                self.assertEqual(oracle.evaluation_result(source, case), case["expected"])

    def test_synthetic_import_only_allows_inert_tom(self) -> None:
        """The original string-column helper gets only a synthetic enum namespace."""
        self.assertEqual(
            oracle.synthetic_import("Microsoft.AnalysisServices.Tabular").AnalysisServices.Tabular.DataType.String,
            "String",
        )
        with self.assertRaises(ImportError):
            oracle.synthetic_import("sempy")

    def test_selected_core_statements_are_original_and_import_free(self) -> None:
        """The selected evaluator core cannot gain connection/startup statements."""
        preflight, nodes = self.source.evaluator_nodes()
        self.assertEqual(ast.unparse(preflight.test), "tom.model.Tables.Count == 0")
        self.assertEqual(nodes[0].targets[0].id, "violations")
        self.assertEqual(nodes[-1].targets[0].id, "finalDF")
        for node in nodes:
            self.assertFalse(any(isinstance(n, (ast.Import, ast.ImportFrom, ast.With)) for n in ast.walk(node)))

    def test_original_default_arguments_are_recorded(self) -> None:
        """Default rendering is distinct from an explicitly requested dataframe."""
        defaults = self.inventory["evaluator"]["defaults"]
        self.assertIsNone(defaults["rules"])
        self.assertIs(defaults["return_dataframe"], False)
        self.assertIs(defaults["extended"], False)
        self.assertIs(defaults["check_dependencies"], True)

    def test_no_tables_empty_results_and_data_errors_stay_distinct(self) -> None:
        """A missing source column is not converted into no violations."""
        outcomes = {case["id"]: oracle.evaluation_result(self.source, case) for case in self.evaluations}
        empty = outcomes["default-catalog-no-tables"]
        clean = outcomes["no-violations-is-empty-dataframe"]
        self.assertEqual(empty["outcome"], "no_tables")
        self.assertIsNone(empty["returnValue"])
        self.assertEqual(clean["outcome"], "dataframe")
        self.assertEqual(clean["columns"], oracle.RESULT_COLUMNS)
        self.assertEqual(clean["rows"], [])
        self.assertEqual(outcomes["predicate-data-error-is-not-empty-results"]["errorType"], "TypeError")
        self.assertEqual(outcomes["malformed-bim-json"]["origin"], "oracle_json_reader_not_TOM")

    def test_missing_evidence_and_null_data_are_not_clean(self) -> None:
        """Synthetic missing evidence and actual source errors have different types."""
        for case_id, error_type in [
            ("r22-missing-row-count-is-not-zero", "MissingEvidence"),
            ("r25-none-expression-data-error", "TypeError"),
            ("r61-empty-name-data-error", "IndexError"),
            ("r62-empty-name-data-error", "IndexError"),
        ]:
            self.assertEqual(self.by_id[case_id]["expected"]["outcome"], "error")
            self.assertEqual(self.by_id[case_id]["expected"]["errorType"], error_type)

    def test_missing_dependencies_are_not_known_empty_helper_results(self) -> None:
        """Missing dependency input must not become an evaluated clean result."""
        for rule in (40, 41):
            known_empty = self.by_id[f"r{rule:02d}-negative"]
            missing = {
                **known_empty,
                "calls": {
                    key: value for key, value in known_empty["calls"].items()
                    if key != "dependencies"
                },
            }
            self.assertIs(
                oracle.predicate_result(self.source, known_empty)["violates"], False
            )
            result = oracle.predicate_result(self.source, missing)
            self.assertEqual(result["outcome"], "error")
            self.assertEqual(result["errorType"], "NameError")
            self.assertEqual(result["observedCalls"], [])

    def test_thresholds_and_relationship_double_counting(self) -> None:
        """Capture strict comparisons and the original numerator, not a redesign."""
        for case_id, expected in [
            ("r19-threshold-exactly-30-percent", False),
            ("r19-threshold-40-percent", True),
            ("r19-double-counts-both-and-many", True),
            ("r19-empty-relationships", False),
            ("r22-negative", False),
            ("r22-positive", True),
            ("r22-fractional-above-threshold", True),
        ]:
            self.assertIs(self.by_id[case_id]["expected"]["violates"], expected)

    def test_case_regex_and_null_quirks_come_from_original_code(self) -> None:
        """Deliberately preserve surprising expression behavior in synthetic cases."""
        for case_id, expected in [
            ("r04-regex-parentheses-are-not-literal", True),
            ("r11-negative", False),
            ("r16-tab-not-stripped", False),
            ("r34-negative", False),
            ("r42-negative", False),
            ("r42-cross-table-name-only-match", True),
            ("r43-reverse-activation-order", True),
            ("r57-null-is-not-empty-string", False),
            ("r63-negative", False),
        ]:
            self.assertIs(self.by_id[case_id]["expected"]["violates"], expected)

    def test_capture_refuses_to_replace_existing_expectations(self) -> None:
        """A convenient capture switch must not rewrite acceptance baselines."""
        with self.assertRaises(FileExistsError):
            oracle.capture(self.source)

    def test_manifest_changes_are_rejected(self) -> None:
        """Validation also checks frozen fixture bytes, not just live results."""
        with patch.object(oracle, "fixture_manifest", return_value={"files": []}):
            with self.assertRaisesRegex(AssertionError, "captured fixture bytes changed"):
                oracle.validate(self.source)

    def test_relationship_and_multiscope_bim_results(self) -> None:
        """Original evaluator dispatch and naming helpers produce the row shapes."""
        cases = {case["id"]: case for case in self.evaluations}
        relationships = oracle.evaluation_result(self.source, cases["relationship-and-model-scopes"])
        self.assertEqual(len(relationships["rows"]), 5)
        self.assertEqual(
            [row["Object Type"] for row in relationships["rows"]],
            ["Relationship", "Relationship", "Model", "Relationship", "Relationship"],
        )
        self.assertEqual(relationships["rows"][0]["Object Name"], "'Sales'[Id] -> 'Bridge'[Id]")
        names = oracle.evaluation_result(self.source, cases["multi-scope-case-and-names"])
        self.assertEqual(len(names["rows"]), 5)
        self.assertEqual({row["Object Type"] for row in names["rows"]}, {"Table", "Column"})

    def test_not_covered_inventory_is_explicit(self) -> None:
        """Helper responses do not establish actual dependency/query semantics."""
        coverage = json.loads((oracle.FIXTURES / "coverage.json").read_text(encoding="utf-8"))
        self.assertEqual([r["ordinal"] for r in coverage["notCoveredRules"]], [14, 15])
        self.assertEqual(len(coverage["rules"]), 64)
        self.assertTrue(coverage["rules"][39]["unverifiedWrapperMethods"])
        self.assertGreater(len(coverage["unexecutedRuntime"]), 5)


if __name__ == "__main__":
    unittest.main()

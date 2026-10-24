# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline execution and escape-guard tests for the source-pinned folder oracle."""

import ast
import builtins
import copy
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import reference


class FolderReferenceTests(unittest.TestCase):
    """Independent source, comparison, consumer and no-I/O boundary checks."""

    def test_all_hand_authored_cases_execute_actual_python(self) -> None:
        manifest, sources = reference.load_cases(), reference.load_sources()
        for case in manifest["cases"] + manifest["python_only"]:
            with self.subTest(case=case["id"]):
                reference.check_case(case, reference.observe(case, sources))

    def test_source_drift_is_not_silently_recorded(self) -> None:
        with patch.dict(
            reference.SOURCE_SHA256, {"client/fab_api_folders.py": "0" * 64}
        ):
            with self.assertRaisesRegex(AssertionError, "Source drift"):
                reference.load_sources()

    def test_swallowed_io_attempt_still_fails_the_guard(self) -> None:
        with self.assertRaises(reference.support.OfflineViolation):
            with reference.support.offline_boundary({}):
                try:
                    builtins.open("not-allowed", "w")
                except reference.support.OfflineViolation:
                    pass

    def test_actual_ast_cannot_import_or_write(self) -> None:
        for statement in ["open('not-allowed', 'w')", "__import__('os')"]:
            sources = reference.load_sources()
            for node in sources["client/fab_api_folders.py"].body:
                if isinstance(node, ast.FunctionDef) and node.name == "list_folders":
                    node.body.insert(0, ast.parse(statement).body[0])
            with self.subTest(statement=statement):
                with self.assertRaises(reference.support.OfflineViolation):
                    reference.observe(reference.load_cases()["cases"][0], sources)

    def test_literal_comparison_detects_url_rows_and_error_mutations(self) -> None:
        sources = reference.load_sources()
        for case in reference.load_cases()["cases"]:
            actual = reference.observe(case, sources)
            mutated = copy.deepcopy(actual)
            mutated["requests"][0]["url"] += "&unexpected=1"
            with self.subTest(case=case["id"], mutation="URL"):
                with self.assertRaises(AssertionError):
                    reference.check_case(case, mutated)
            mutated = copy.deepcopy(actual)
            if "rows" in mutated:
                mutated["rows"].append({"id": "extra", "displayName": "extra"})
            else:
                mutated["error_code"] = "WrongCode"
            with self.subTest(case=case["id"], mutation="rows-or-error"):
                with self.assertRaises(AssertionError):
                    reference.check_case(case, mutated)

    def test_actual_memstore_helpers_do_not_infer_null_or_orphan_roots(self) -> None:
        sources = reference.load_sources()
        program = reference.support._compile_symbols(
            sources,
            "utils/fab_mem_store.py",
            {"_extract_root_folders", "_extract_nested_folders"},
        )

        def folder(name: str, identifier: str, parent: object) -> SimpleNamespace:
            return SimpleNamespace(name=name, id=identifier, parent=parent)

        env = {"Folder": folder}
        workspace = object()
        rows = []
        with reference.offline_boundary({}):
            exec(program, env)
            assert env["_extract_root_folders"](
                {"id": "root", "displayName": "Root"}, rows, workspace
            )
            for parent in (None, "missing"):
                row = {"id": "child", "displayName": "Child", "parentFolderId": parent}
                assert not env["_extract_root_folders"](row, rows, workspace)
                assert not env["_extract_nested_folders"](row, rows)
            assert env["_extract_nested_folders"](
                {"id": "child", "displayName": "Child", "parentFolderId": "root"},
                rows,
            )
        self.assertEqual([row.id for row in rows], ["root", "child"])
        self.assertIs(rows[0].parent, workspace)
        self.assertIs(rows[1].parent, rows[0])


if __name__ == "__main__":
    unittest.main()

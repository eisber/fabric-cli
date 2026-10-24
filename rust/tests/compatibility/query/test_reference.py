# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline guard, provenance, caller and mutation controls for the real oracle."""

import argparse
import builtins
import copy
import json
import os
import socket
import subprocess
import sys
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import reference
import guards


class ReferenceTests(unittest.TestCase):
    """Every source execution is protected by the same active guard."""

    def setUp(self) -> None:
        self.fixtures = reference.fixtures()
        self.reference = reference.Reference(self.fixtures["timestamp"])
        self.guard = guards.offline()
        self.guard.__enter__()

    def tearDown(self) -> None:
        self.guard.__exit__(None, None, None)

    def test_all_frozen_cases_match_real_source_and_repeat(self) -> None:
        for case in self.fixtures["cases"]:
            with self.subTest(case=case["name"]):
                before = copy.deepcopy(case)
                first = reference.Reference(self.fixtures["timestamp"]).observe(case)
                second = reference.Reference(self.fixtures["timestamp"]).observe(case)
                self.assertEqual(reference.exact(first), reference.exact(second))
                self.assertEqual(reference.exact(case), reference.exact(before))
                expected = (
                    {"error": case["error"], "message": "Invalid jmespath query (https://jmespath.org)"}
                    if "error" in case else {"value": case["expected"]}
                )
                self.assertEqual(reference.exact(first["result"]), reference.exact(expected))

    def test_source_hash_negative_control(self) -> None:
        with self.assertRaisesRegex(AssertionError, "source drift"):
            reference.source_tree("utils/fab_jmespath.py", b"def search(): return None\n")

    def test_source_input_mutation_is_detected(self) -> None:
        original = self.reference.namespace["search"]

        def mutating_search(data: Any, expression: str, deep: bool) -> Any:
            data["injected"] = True
            return original(data, expression, deep)

        self.reference.namespace["search"] = mutating_search
        with self.assertRaisesRegex(AssertionError, "source mutated input"):
            self.reference.observe({"name": "mutation-control", "data": {"a": 1}, "expression": "."})

    def matching_observations(self) -> list[dict[str, Any]]:
        """Construct an exact baseline before deliberately corrupting observations."""
        observations = []
        for case in self.fixtures["cases"]:
            if case.get("support") == "unsupported":
                observations.append(reference.unsupported_observation(case, self.fixtures["timestamp"]))
            else:
                observations.append(reference.Reference(self.fixtures["timestamp"]).observe(case))
        self.assertEqual(reference.compare(observations), [])
        return observations

    def test_changed_native_result_is_detected(self) -> None:
        observations = self.matching_observations()
        observations[0]["result"] = {"value": []}
        self.assertTrue(reference.compare(observations))

    def test_changed_rendered_bytes_are_detected(self) -> None:
        observations = self.matching_observations()
        observations[0]["stdout"] += "\n"
        self.assertTrue(reference.compare(observations))

    def test_inventory_missing_case_is_detected(self) -> None:
        with self.assertRaisesRegex(AssertionError, "inventory"):
            reference.compare([])

    def test_unsupported_cannot_be_relabelled_as_native_exact(self) -> None:
        observations = self.matching_observations()
        index = next(i for i, case in enumerate(self.fixtures["cases"])
                     if case.get("support") == "unsupported")
        observations[index] = reference.Reference(self.fixtures["timestamp"]).observe(
            self.fixtures["cases"][index]
        )
        self.assertTrue(reference.compare(observations))

    def test_network_guard_negative_control(self) -> None:
        with self.assertRaisesRegex(AssertionError, "denied side effect"):
            socket.socket()

    def test_subprocess_guard_negative_control(self) -> None:
        with self.assertRaisesRegex(AssertionError, "denied side effect"):
            subprocess.run([sys.executable, "-c", "raise SystemExit(99)"], check=True)

    def test_disk_guard_negative_control(self) -> None:
        with self.assertRaisesRegex(AssertionError, "denied disk write"):
            builtins.open(Path(sys.executable).parents[2] / "query-oracle-must-not-exist", "w")

    def test_sleep_guard_negative_control(self) -> None:
        with self.assertRaisesRegex(AssertionError, "denied side effect"):
            time.sleep(0)

    def test_auth_import_guard_negative_control(self) -> None:
        with self.assertRaisesRegex(AssertionError, "unapproved source import"):
            self.reference.namespace["__builtins__"]["__import__"]("msal")

    def caller_namespace(self) -> tuple[dict[str, Any], list[Any]]:
        """Supply only in-memory collaborators to complete real caller definitions."""
        captured: list[Any] = []
        ns = self.reference.namespace.copy()
        ns.update({
            "Namespace": argparse.Namespace,
            "utils_jmespath": SimpleNamespace(search=ns["search"]),
            "fab_util": SimpleNamespace(process_nargs=ns["process_nargs"]),
            "utils_ui": SimpleNamespace(
                print_output_format=lambda *args, **kwargs: captured.append(kwargs["data"])
            ),
        })
        return ns, captured

    def test_ls_real_query_and_default_column_branches(self) -> None:
        ns, captured = self.caller_namespace()
        reference.definitions(
            "utils/fab_cmd_ls_utils.py", {"format_and_print_output"}, ns
        )
        data = [{"name": "Alpha.Workspace", "id": "a", "extra": 1}]
        args = argparse.Namespace(query=["\"[].{key:", "id}\""])
        ns["format_and_print_output"](data, args, False, columns=["name"])
        self.assertEqual(captured.pop(), [{"key": "a"}])
        args.query = ""
        ns["format_and_print_output"](data, args, False, columns=["name", "missing"])
        self.assertEqual(captured.pop(), [{"name": "Alpha.Workspace"}])

    def test_get_real_inventory_dot_and_false_string_branches(self) -> None:
        ns, captured = self.caller_namespace()
        reference.definitions("utils/fab_cmd_get_utils.py", {"query_and_export"}, ns)
        args = argparse.Namespace(query="", output=None, deep_traversal=False)
        data = {"flag": False}
        ns["query_and_export"](data, args, "unused")
        self.assertEqual(captured.pop(), ["flag"])
        args.query = "."
        ns["query_and_export"](data, args, "unused")
        self.assertEqual(captured.pop(), data)
        args.query = "flag"
        ns["query_and_export"](data, args, "unused")
        self.assertEqual(captured.pop(), "False")

    def test_api_real_empty_query_and_empty_result_fallback(self) -> None:
        ns, captured = self.caller_namespace()
        ns.update({
            "os": os,
            "utils": SimpleNamespace(process_nargs=ns["process_nargs"]),
            "fab_ui": ns["utils_ui"],
            "fabric_api": SimpleNamespace(do_request=lambda *args: SimpleNamespace(
                status_code=200, text='{"items":[]}', headers={}
            )),
        })
        reference.definitions("commands/api/fab_api_request.py", {"exec_command"}, ns)
        args = argparse.Namespace(
            input=None, file_path=None, endpoint="in-memory-only", show_headers=False, query=""
        )
        ns["exec_command"](args)
        payload = {"status_code": 200, "text": {"items": []}}
        self.assertEqual(captured.pop(), payload)
        args.query = "text.items"
        ns["exec_command"](args)
        self.assertEqual(captured.pop(), payload)


if __name__ == "__main__":
    unittest.main()

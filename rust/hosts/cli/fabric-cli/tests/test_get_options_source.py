# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Observe pinned GET option branches without importing the product or its state."""

from __future__ import annotations

import ast
from functools import cache
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
from types import ModuleType, SimpleNamespace
from typing import Any
import unittest
from unittest.mock import patch

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
ROOT = Path(__file__).resolve().parents[5]
DIGESTS: dict[str, dict[str, Any]] = {}


@cache
def source(path: str) -> str:
    """Read immutable source, separately identifying the current checkout bytes."""
    data = subprocess.check_output(
        ["git", "show", f"{PIN}:src/fabric_cli/{path}"], cwd=ROOT, timeout=10
    )
    current = (ROOT / "src" / "fabric_cli" / path).read_bytes()
    DIGESTS[path] = {
        "pinned_sha256": hashlib.sha256(data).hexdigest(),
        "checkout_sha256": hashlib.sha256(current).hexdigest(),
        "checkout_matches_pin": data == current,
    }
    return data.decode("utf-8")


def load(
    path: str, names: set[str], namespace: dict[str, Any]
) -> SimpleNamespace:
    """Execute unchanged selected functions with explicit offline dependencies."""
    tree = ast.parse(source(path), filename=path)
    tree.body = [
        node for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name in names
    ]
    if {node.name for node in tree.body} != names:
        raise ValueError(f"Missing pinned definitions in {path}")
    tree.body.insert(0, ast.ImportFrom(
        module="__future__", names=[ast.alias(name="annotations")], level=0
    ))
    ast.fix_missing_locations(tree)
    values = dict(namespace)
    exec(compile(tree, f"{PIN}:{path}", "exec"), values)
    return SimpleNamespace(**values)


def constants() -> set[str]:
    """Read the actual pinned metadata roots instead of substituting a fixture."""
    for node in ast.parse(source("core/fab_constant.py")).body:
        if (
            isinstance(node, ast.Assign)
            and any(
                isinstance(target, ast.Name) and target.id == "ITEM_METADATA_PROPERTIES"
                for target in node.targets
            )
        ):
            return ast.literal_eval(node.value)
    raise ValueError("Pinned metadata root constant missing")


class GetOptionsSource(unittest.TestCase):
    """Source option behavior, including intentionally inert parsed flags."""

    def setUp(self) -> None:
        self.events: list[Any] = []
        core = ModuleType("fabric_cli.core")
        core.fab_constant = SimpleNamespace(ITEM_METADATA_PROPERTIES=constants())
        package = ModuleType("fabric_cli")
        package.core = core
        self.modules = patch.dict(sys.modules, {
            "fabric_cli": package, "fabric_cli.core": core,
        })
        self.modules.start()
        self.addCleanup(self.modules.stop)
        query = load(
            "utils/fab_jmespath.py", {"search", "_get_json_paths"}, {}
        )
        self.utils = load(
            "utils/fab_cmd_get_utils.py",
            {"is_metadata_property_query", "query_and_export"},
            {
                "utils_jmespath": query,
                "utils_storage": SimpleNamespace(
                    do_output=lambda **kw: self.events.append(("file", kw["data"]))
                ),
                "utils_ui": SimpleNamespace(
                    print_output_format=lambda args, **kw: self.events.append(
                        ("stdout", kw["data"])
                    )
                ),
            },
        )

    def test_exact_metadata_root_rule_positive_and_negative(self) -> None:
        for root in constants():
            self.assertTrue(self.utils.is_metadata_property_query(root))
            self.assertTrue(self.utils.is_metadata_property_query(f"{root}.nested[0]"))
        for query in (
            "", ".", "Id", " id", "id ", "id[0]", "properties[0]", "[id]",
            "definition.parts", "connections", "schedules", "notproperties.x",
            "'properties.x'",
        ):
            self.assertFalse(self.utils.is_metadata_property_query(query), query)

    def test_metadata_query_skips_support_warning_and_confirmation(self) -> None:
        def unexpected(*args: Any) -> None:
            raise AssertionError("Metadata branch must not invoke definition policy")

        validate = load(
            "commands/fs/fab_fs_get.py", {"_validate_sensitivity_label_warning"},
            {
                "get_utils": self.utils,
                "fab_ui": SimpleNamespace(prompt_confirm=unexpected),
                "fab_item_util": SimpleNamespace(item_sensitivity_label_warnings=unexpected),
            },
        )._validate_sensitivity_label_warning
        for force in (False, True):
            for verbose in (False, True):
                args = SimpleNamespace(query="properties.nested", force=force, verbose=verbose)
                self.assertTrue(validate(args, SimpleNamespace(check_command_support=unexpected)))

    def test_definition_confirmation_decline_eof_and_force_are_distinct(self) -> None:
        message = "Item definition is retrieved without its sensitivity label. Are you sure?"
        for answer in (True, False, None):
            validate = load(
                "commands/fs/fab_fs_get.py", {"_validate_sensitivity_label_warning"},
                {
                    "get_utils": self.utils,
                    "Command": SimpleNamespace(FS_EXPORT="export"),
                    "fab_ui": SimpleNamespace(
                        prompt_confirm=lambda text: self.events.append(text) or answer
                    ),
                    "fab_item_util": SimpleNamespace(
                        item_sensitivity_label_warnings=lambda args, action: self.events.append(action)
                    ),
                },
            )._validate_sensitivity_label_warning
            item = SimpleNamespace(check_command_support=lambda command: True)
            self.assertIs(validate(SimpleNamespace(query=".", force=False), item), answer)
            self.assertEqual(self.events.pop(), message)
            self.assertTrue(validate(SimpleNamespace(query=".", force=True), item))
            self.assertEqual(self.events.pop(), "retrieved")

    def test_verbose_does_not_change_path_depth_or_explicit_projection(self) -> None:
        data = {"one": {"two": {"three": {"four": {"five": 1}}}}}
        for query in (None, "", "."):
            outputs = []
            for verbose in (False, True):
                args = SimpleNamespace(query=query, output=None, verbose=verbose)
                self.utils.query_and_export(data, args, "Demo.Workspace")
                outputs.append(self.events.pop())
            self.assertEqual(outputs[0], outputs[1])
            if not query:
                self.assertEqual(outputs[0], ("stdout", ["one.two.three.four.five"]))
        args = SimpleNamespace(query=".", output=None, verbose=True)
        self.utils.query_and_export(data, args, "Demo.Workspace", verbose=False)
        self.assertEqual(self.events, [])

    def test_output_wins_over_stdout_and_empty_projected_values(self) -> None:
        for value in ([], {}, "", False, 0, None):
            args = SimpleNamespace(query=".", output="destination", verbose=True)
            self.utils.query_and_export(value, args, "Demo.Workspace")
            self.assertEqual(self.events.pop(), ("file", value))
            args.output = None
            self.utils.query_and_export(value, args, "Demo.Workspace")
            self.assertEqual(self.events, [])

    def test_output_and_writer_errors_are_not_success(self) -> None:
        def fail(*args: Any, **kw: Any) -> None:
            raise BrokenPipeError("synthetic writer failure")

        self.utils.utils_ui.print_output_format = fail
        with self.assertRaises(BrokenPipeError):
            self.utils.query_and_export(
                {"id": "w"}, SimpleNamespace(query=".", output=None), "Demo.Workspace"
            )
        self.utils.utils_storage.do_output = fail
        with self.assertRaises(BrokenPipeError):
            self.utils.query_and_export(
                {}, SimpleNamespace(query=".", output="destination"), "Demo.Workspace"
            )

    def test_local_output_double_json_suffix_is_source_behavior(self) -> None:
        captured: list[str] = []
        storage = load(
            "utils/fab_storage.py", {"_write_to_local", "_validate_json"},
            {
                "os": SimpleNamespace(
                    path=SimpleNamespace(
                        normpath=lambda value: value,
                        dirname=lambda value: "destination",
                        exists=lambda value: True,
                    )
                ),
                "json": json,
                "open": lambda path, *args, **kw: captured.append(path) or io.StringIO(),
                "fab_ui": SimpleNamespace(print_output_format=lambda *args, **kw: None),
            },
        )
        storage._write_to_local(
            SimpleNamespace(), "destination/Demo.Workspace/get_20000101_000000.json",
            {"id": "w"}, export=False,
        )
        self.assertEqual(
            captured, ["destination/Demo.Workspace/get_20000101_000000.json.json"]
        )


if __name__ == "__main__":
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(GetOptionsSource)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    print(json.dumps({"source_pin": PIN, "sources": DIGESTS}, sort_keys=True))
    sys.exit(0 if result.wasSuccessful() else 1)

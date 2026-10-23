# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Check synthetic deployment fixtures against pinned, offline Python leaf code.

This development oracle is not imported by Rust, packaged, or used in production.
It never imports fabric_cicd (whose package initialization has broader effects).
"""

from __future__ import annotations

import __future__
import argparse
import ast
import builtins
import hashlib
import json
import logging
import platform
import re
import subprocess
import urllib.parse
from dataclasses import asdict, dataclass
from enum import Enum
from importlib.metadata import version
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import yaml
from jsonpath_ng.ext import parse

CICD_COMMIT = "5ca096c00a9a878c646c59fe537b883eba951329"
SOURCE = f"microsoft/fabric-cicd@{CICD_COMMIT}"
FIXTURES = ("yaml.json", "config.json", "ids.json", "transforms.json")
DEPENDENCIES = {"PyYAML": "6.0.3", "jsonpath-ng": "1.9.1"}
SOURCES = {
    "constants.py": {
        "ItemType",
        "DEFAULT_GUID",
        "ACCEPTED_ITEM_TYPES",
        "WORKSPACE_ID_REFERENCE_REGEX",
        "REFLEX_WORKSPACE_ID_REFERENCE_REGEX",
        "PARAMETER_FILE_NAME",
        "YAML_FILE_EXTENSIONS",
        "ITEM_ATTR_LOOKUP",
        "ITEM_VARIABLE_PREFIX",
        "WORKSPACE_VARIABLE_PREFIX",
        "ENVIRONMENT_VARIABLE_PREFIX",
        "CROSS_WORKSPACE_ITEM_SEPARATOR",
        "WORKSPACE_VARIABLE_ATTRIBUTES",
        "WORKSPACE_VARIABLES_FIXED",
        "INVALID_REPLACE_VALUE_SPARK_POOL",
        "PARAMETER_MSGS",
        "DYNAMIC_VARIABLE_MSGS",
    },
    "_common\\_exceptions.py": {"BaseCustomError", "InputError", "ParsingError"},
    "_common\\_check_utils.py": {
        "check_valid_json_content",
        "check_valid_yaml_content",
    },
    "_common\\_config_utils.py": {
        "get_config_value",
        "update_setting",
        "extract_workspace_settings",
    },
    "_parameter\\_parameter.py": {
        "_DuplicateKeyLoader",
        "_collect_duplicate_key_errors",
        "_check_duplicate_keys_constructor",
    },
    "_parameter\\_utils.py": {
        "ParsedDynamicVariable",
        "_validate_regex_structure",
        "_validate_regex_pattern",
        "extract_find_value",
        "extract_replace_value",
        "parse_dynamic_variable",
        "parse_cross_workspace_item_variable",
        "_extract_item_variable_components",
        "_validate_item_variable_components",
        "_validate_and_normalize_item_variable_attribute",
        "_extract_workspace_id",
        "parse_item_variable",
        "_extract_item_attribute",
        "extract_parameter_filters",
        "_resolve_filter_path",
        "process_input_path",
        "process_environment_key",
        "replace_key_value",
        "check_replacement",
        "_find_match",
    },
    "fabric_workspace.py": {
        "_replace_logical_ids",
        "_replace_parameters",
        "_replace_workspace_ids",
    },
}


def git(root: Path, *args: str) -> str:
    """Run a bounded local Git read, with no remote access."""
    return subprocess.run(
        ["git", "--no-pager", "-C", str(root), *args],
        check=True,
        capture_output=True,
        text=True,
        timeout=10,
    ).stdout.strip()


def load_nodes(path: Path, names: set[str], namespace: dict[str, Any]) -> None:
    """Compile only named definitions/assignments, preserving their source bodies."""
    tree = ast.parse(path.read_text(encoding="utf-8-sig"), filename=str(path))
    candidates = tree.body
    if path.name == "fabric_workspace.py":
        candidates = next(
            node.body
            for node in tree.body
            if isinstance(node, ast.ClassDef) and node.name == "FabricWorkspace"
        )
    selected = []
    found = set()
    for node in candidates:
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)):
            name = node.name
        elif isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name):
            name = node.targets[0].id
        else:
            continue
        if name in names:
            selected.append(node)
            found.add(name)
    if found != names:
        raise ValueError(f"Missing reference definitions in {path}: {names - found}")
    exec(
        compile(
            ast.Module(body=selected, type_ignores=[]),
            str(path),
            "exec",
            flags=__future__.annotations.compiler_flag,
        ),
        namespace,
    )


def reference_namespace(root: Path) -> tuple[dict[str, Any], dict[str, str]]:
    """Load pinned leaves without package initialization or service constructors."""
    if git(root, "rev-parse", "HEAD") != CICD_COMMIT:
        raise ValueError(f"Reference checkout must be exactly {CICD_COMMIT}")
    source_root = root / "src" / "fabric_cicd"
    paths = [source_root.joinpath(*name.split("\\")) for name in SOURCES]
    git(root, "diff", "--exit-code", CICD_COMMIT, "--", *map(str, paths))
    namespace: dict[str, Any] = {
        "__name__": __name__,
        "__builtins__": dict(vars(builtins)),
        "yaml": yaml,
        "json": json,
        "re": re,
        "urllib": urllib,
        "Path": Path,
        "Enum": Enum,
        "dataclass": dataclass,
        "logger": logging.getLogger("deployment_reference"),
    }
    constants: dict[str, Any] = {"Enum": Enum}
    load_nodes(paths[0], SOURCES["constants.py"], constants)
    namespace["constants"] = SimpleNamespace(
        **{name: constants[name] for name in SOURCES["constants.py"]}
    )
    namespace["ItemType"] = constants["ItemType"]
    namespace["parse"] = parse
    for name, path in zip(SOURCES, paths):
        if name != "constants.py":
            load_nodes(path, SOURCES[name], namespace)
    namespace["_DuplicateKeyLoader"].add_constructor(
        yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG,
        namespace["_check_duplicate_keys_constructor"],
    )

    def import_leaf(
        name: str,
        globals: Any = None,
        locals: Any = None,
        fromlist: Any = (),
        level: int = 0,
    ) -> SimpleNamespace:
        if name != "fabric_cicd._parameter._utils" or level != 0:
            raise ImportError(f"Reference attempted an out-of-scope import: {name}")
        return SimpleNamespace(**namespace)

    namespace["__builtins__"]["__import__"] = import_leaf
    return namespace, {
        name: hashlib.sha256(path.read_bytes()).hexdigest()
        for name, path in zip(SOURCES, paths)
    }


def workspace(case: dict[str, Any], calls: list[str]) -> SimpleNamespace:
    """Create synthetic data/callbacks; no real workspace or credentials exist."""

    def refresh() -> None:
        calls.append("refresh_deployed_items")

    return SimpleNamespace(
        environment=case.get("environment", "dev"),
        workspace_id="11111111-1111-1111-1111-111111111111",
        bulk_publish_enabled=False,
        repository_directory=Path(__file__).parent,
        repository_items={
            "Notebook": {
                str(index): SimpleNamespace(**item)
                for index, item in enumerate(case.get("items", []))
            }
        },
        workspace_items={
            "Notebook": {"Daily.Load": {"id": "22222222-2222-2222-2222-222222222222"}}
        },
        _refresh_deployed_items=refresh,
        _resolve_workspace_name=lambda: "Synthetic / Workspace",
    )


def evaluate(case: dict[str, Any], ref: dict[str, Any], calls: list[str]) -> Any:
    """Execute one declared leaf operation; unsupported operations fail explicitly."""
    operation = case["operation"]
    target = workspace(case, calls)
    if operation == "safe_yaml":
        return yaml.safe_load(case["input"])
    if operation == "parameter_yaml":
        return yaml.load(case["input"], Loader=ref["_DuplicateKeyLoader"])
    if operation == "yaml_types":
        return [
            {"type": type(value).__name__, "value": str(value)}
            for value in yaml.safe_load(case["input"])
        ]
    if operation == "config_value":
        return ref["get_config_value"](case["section"], case["key"], target.environment)
    if operation == "workspace_settings":
        return ref["extract_workspace_settings"](case["config"], target.environment)
    if operation == "environment_key":
        return ref["process_environment_key"](target.environment, case["input"])
    if operation == "dynamic":
        return ref["extract_replace_value"](target, case["input"])
    if operation == "parse_dynamic":
        return asdict(ref["parse_dynamic_variable"](case["input"]))
    if operation == "logical":
        return ref["_replace_logical_ids"](target, case["input"])
    item = SimpleNamespace(type=case.get("item_type", "Notebook"), name="Daily.Load")
    if operation == "workspace":
        return ref["_replace_workspace_ids"](target, case["input"], item)
    if operation == "key_value":
        return ref["replace_key_value"](
            target,
            case["parameter"],
            case["input"],
            target.environment,
            is_yaml=case.get("yaml", False),
        )
    if operation == "parameters":
        target.environment_parameter = case["parameters"]
        for rules in target.environment_parameter.values():
            if any("file_path" in rule for rule in rules):
                raise ValueError("Filesystem filtering is outside this oracle's scope")
        file = SimpleNamespace(
            contents=case["input"],
            file_path=Path(__file__).parent / case.get("file_name", "definition.json"),
        )
        return ref["_replace_parameters"](target, file, item)
    raise ValueError(f"Unknown fixture operation: {operation}")


def check_case(case: dict[str, Any], ref: dict[str, Any]) -> None:
    """Compare fixed expectations without generating or rewriting baselines."""
    calls: list[str] = []
    try:
        actual = evaluate(case, ref, calls)
    except Exception as error:
        expected = case.get("error")
        if (
            expected is None
            or type(error).__name__ != expected["type"]
            or expected["contains"] not in str(error)
        ):
            raise AssertionError(
                f"{case['id']}: unexpected reference failure"
            ) from error
    else:
        if "error" in case:
            raise AssertionError(f"{case['id']}: expected failure, got {actual!r}")
        if json.dumps(actual, sort_keys=True) != json.dumps(
            case["expected"], sort_keys=True
        ):
            raise AssertionError(
                f"{case['id']}: expected {case['expected']!r}, got {actual!r}"
            )
    if "calls" in case and calls != case["calls"]:
        raise AssertionError(f"{case['id']}: unexpected callbacks: {calls!r}")


def main() -> None:
    """Validate fixtures offline and print reproducible source/runtime evidence."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference-root", required=True, type=Path)
    args = parser.parse_args()
    dependencies = {name: version(name) for name in DEPENDENCIES}
    if dependencies != DEPENDENCIES:
        raise ValueError(
            f"Oracle requires {DEPENDENCIES}; found {dependencies}. "
            "Do not change fixture expectations to accommodate dependency drift."
        )
    ref, source_hashes = reference_namespace(args.reference_root.resolve())
    ids: set[str] = set()
    fixture_hashes = {}
    for name in FIXTURES:
        path = Path(__file__).parent / name
        document = json.loads(path.read_text(encoding="utf-8"))
        if document["source"] != SOURCE:
            raise ValueError(f"Wrong fixture provenance: {name}")
        fixture_hashes[name] = hashlib.sha256(path.read_bytes()).hexdigest()
        for case in document["cases"]:
            if case["id"] in ids:
                raise ValueError(f"Duplicate fixture ID: {case['id']}")
            ids.add(case["id"])
            check_case(case, ref)
    print(
        json.dumps(
            {
                "source": SOURCE,
                "python": platform.python_version(),
                "platform": platform.platform(),
                "dependencies": dependencies,
                "passed": len(ids),
                "source_sha256": source_hashes,
                "fixture_sha256": fixture_hashes,
            },
            indent=2,
            sort_keys=True,
        )
    )


if __name__ == "__main__":
    main()

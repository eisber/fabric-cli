# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture additive configured-string/config semantics from actual pinned bodies."""

from __future__ import annotations

import argparse
import ast
import copy
import hashlib
import json
import logging
import re
import subprocess
from enum import Enum
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from urllib.parse import urlsplit

import yaml

from config_reference import BASE, PIN, load
from literal_parameter_reference import oracle


def capture(root: Path) -> dict[str, Any]:
    """Execute pinned validators and consumers; never import the upstream package."""
    namespace, hashes = oracle.reference_namespace(root)
    if {name: oracle.version(name) for name in oracle.DEPENDENCIES} != oracle.DEPENDENCIES:
        raise ValueError("Pinned oracle dependencies required")
    source = root / "src" / "fabric_cicd"
    parameter_path = source / "_parameter" / "_parameter.py"
    oracle.load_nodes(source / "constants.py", {"PARAM_NAMES"}, namespace["constants"].__dict__)
    oracle.load_nodes(
        source / "_parameter" / "_utils.py",
        {"is_valid_structure", "_check_parameter_structure", "_check_semantic_model_binding_structure"},
        namespace,
    )
    oracle.load_nodes(parameter_path, {"Parameter"}, namespace)
    hashes["_parameter/_parameter.py"] = hashlib.sha256(parameter_path.read_bytes()).hexdigest()
    cases = []
    for key in ["is_regex", "ignore_case"]:
        for index, value in enumerate([None, "", "true", "TrUe", "false", "FALSE", "yes", "1", " true", "true ", "enabled", True, False, 1]):
            entry = {
                "find_value": "(source)" if key == "is_regex" and isinstance(value, str) and value.lower() == "true" else "source",
                "replace_value": {"dev": "target"},
                key: value,
            }
            parameters = {"find_replace": [entry]}
            validator = object.__new__(namespace["Parameter"])
            validator.environment_parameter = copy.deepcopy(parameters)
            validator.environment = "dev"
            validator.item_type_in_scope = ["Notebook"]
            validator.repository_directory = Path("synthetic")
            valid = all(step[0] for step in [
                validator._validate_parameter_names(),
                validator._validate_parameter_structure(),
                validator._validate_parameter("find_replace"),
            ])
            result = None
            if valid:
                target = SimpleNamespace(
                    environment="dev", environment_parameter=copy.deepcopy(parameters),
                    repository_directory=Path("synthetic"), bulk_publish_enabled=False,
                )
                result = namespace["_replace_parameters"](
                    target, SimpleNamespace(contents="source SOURCE", file_path=Path("content.py")),
                    SimpleNamespace(type="Notebook", name="Book"),
                )
            cases.append({"id": f"{key}-{index}", "kind": "parameter", "parameters": parameters,
                          "environment": "dev", "input": "source SOURCE", "valid": valid, "expected": result})

    constants: dict[str, Any] = {"Enum": Enum}
    load(source / "constants.py", {"ItemType", "ACCEPTED_ITEM_TYPES", "CONFIG_SECTIONS", "CONFIG_VALIDATION_MSGS"}, constants)
    # hasattr is the actual source validation boundary, not a configurable-name allowlist.
    module_names = set()
    for node in ast.parse((source / "constants.py").read_text(encoding="utf-8-sig")).body:
        if isinstance(node, (ast.ClassDef, ast.FunctionDef)):
            module_names.add(node.name)
        elif isinstance(node, (ast.Assign, ast.AnnAssign)):
            targets = node.targets if isinstance(node, ast.Assign) else [node.target]
            module_names.update(target.id for target in targets if isinstance(target, ast.Name))
        elif isinstance(node, (ast.Import, ast.ImportFrom)):
            module_names.update(alias.asname or alias.name.split(".")[0] for alias in node.names)
    for name in module_names:
        constants.setdefault(name, None)
    constants["VALID_GUID_REGEX"] = r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
    config_namespace: dict[str, Any] = {
        "Path": Path, "re": re, "yaml": yaml, "InputError": namespace["InputError"],
        "constants": SimpleNamespace(**constants), "logger": logging.getLogger("configured-flags"),
        "urlsplit": urlsplit,
    }
    load(source / "_common" / "_validate_env_vars.py",
         {"_VALID_HOSTNAME_REGEX", "_URL_CONSTANTS", "validate_api_url"}, config_namespace)
    load(source / "_common" / "_config_validator.py",
         {"ConfigValidator", "_get_config_fields", "_validate_guid_format"}, config_namespace)
    load(source / "_common" / "_config_utils.py",
         {"get_config_value", "update_setting", "extract_workspace_settings", "extract_publish_settings", "extract_unpublish_settings"},
         config_namespace)
    configs = [
        ("features-direct", {"features": ["enable_bulk_publish", "custom_CASE"]}, "dev", None),
        ("features-environment", {"features": {"dev": ["enable_response_collection"], "prod": ["other"]}}, "dev", None),
        ("features-missing-environment", {"features": {"prod": ["other"]}}, "dev", None),
        ("features-empty-invalid", {"features": []}, "dev", None),
        ("features-bool-invalid", {"features": [True]}, "dev", None),
        ("features-mapping-no-target", {"features": {"dev": ["x"]}}, "N/A", None),
        ("constants-typed", {"constants": {"VERSION": {"dev": 42, "prod": ["x"]}, "VALID_ENABLE_FLAGS": ["custom"]}}, "dev", None),
        ("constants-missing", {"constants": {"VERSION": {"prod": 42}}}, "dev", None),
        ("constants-unknown", {"constants": {"NOT_A_SOURCE_CONSTANT": 42}}, "dev", None),
        ("constant-url-valid", {"constants": {"FABRIC_API_ROOT_URL": "https://api.fabric.microsoft.com/"}}, "dev", None),
        ("constant-url-invalid", {"constants": {"FABRIC_API_ROOT_URL": "http://api.fabric.microsoft.com"}}, "dev", None),
        ("constant-url-path-invalid", {"constants": {"DEFAULT_API_ROOT_URL": "https://api.powerbi.com/v1"}}, "dev", None),
        ("constant-url-type-invalid", {"constants": {"DEFAULT_API_ROOT_URL": False}}, "dev", None),
        ("filters-environment", {"publish": {"exclude_regex": {"dev": "Book"}, "items_to_include": {"prod": ["Other"]}, "skip": False},
                                 "unpublish": {"skip": False, "items_to_include": ["Book.Notebook"]}}, "dev", None),
        ("folder-prefix-invalid", {"publish": {"folder_path_to_include": ["relative"]}}, "dev", None),
        ("regex-invalid", {"publish": {"exclude_regex": "("}}, "dev", None),
        ("unknown-fields-retained", {"custom": {"typed": [1, False]}, "publish": {"custom_filter": 3}}, "dev", None),
        ("features-override-replaces", {"features": ["old"]}, "dev", {"features": {"dev": ["new"]}}),
        ("constants-override-replaces", {"constants": {"VERSION": 1}}, "dev", {"constants": {"DEFAULT_GUID": "other"}}),
        ("workspace-id-precedence", {"core": {**BASE["core"], "workspace": "Name"}}, "dev", None),
        ("workspace-name", {"core": {"workspace": "Name", "repository_directory": "repository"}}, "dev", None),
        ("empty-environment-direct", {}, "", None),
    ]
    for name, changes, environment, overrides in configs:
        config = {**copy.deepcopy(BASE), **changes}
        validator = config_namespace["ConfigValidator"]()
        validator.config = copy.deepcopy(config)
        validator.environment = environment
        validator.config_override = copy.deepcopy(overrides)
        validator._apply_and_validate_overrides()
        validator._validate_config_structure()
        validator._validate_config_sections()
        validator._validate_environment_exists()
        expected = None
        if not validator.errors:
            doc = validator.config
            features = doc.get("features", [])
            if isinstance(features, dict):
                features = features.get(environment, [])
            selected_constants = {
                key: config_namespace["get_config_value"](doc["constants"], key, environment)
                for key in doc.get("constants", {})
                if config_namespace["get_config_value"](doc["constants"], key, environment) is not None
            }
            expected = {
                "document": doc,
                "workspace": config_namespace["extract_workspace_settings"](doc, environment),
                "publish": config_namespace["extract_publish_settings"](doc, environment),
                "unpublish": config_namespace["extract_unpublish_settings"](doc, environment),
                "features": features, "constants": selected_constants,
            }
        cases.append({"id": name, "kind": "config", "config": config, "environment": environment,
                      "overrides": overrides, "valid": not validator.errors, "expected": expected})
    for relative in ["_common/_config_validator.py", "_common/_config_utils.py", "_common/_validate_env_vars.py"]:
        path = source / relative
        hashes[relative] = hashlib.sha256(path.read_bytes()).hexdigest()
        subprocess.run(["git", "-C", str(root), "diff", "--exit-code", PIN, "--", str(path)], check=True, capture_output=True)
    return {"source": oracle.SOURCE, "sourceHashes": hashes, "dependencies": oracle.DEPENDENCIES,
            "boundary": "Actual pinned Parameter schema/transform and ConfigValidator overrides/sections/environment plus extractors; no filesystem/service/auth or upstream-package imports.",
            "constantNames": sorted(module_names), "cases": cases}


def main() -> None:
    """Capture only once before implementation; otherwise compare immutable data."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = json.loads(json.dumps(capture(args.reference_root)))
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(Path(__file__).with_name("configured_flags_source.json").read_text(encoding="utf-8")):
        raise AssertionError("Pinned source differs from immutable configured flags receipt")
    print(json.dumps({"source": PIN, "cases": len(result["cases"]), "status": "passed"}))


if __name__ == "__main__":
    main()

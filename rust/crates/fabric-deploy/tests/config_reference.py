# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture/check additive config contracts from unchanged pinned CICD AST bodies."""

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

import yaml

PIN = "5ca096c00a9a878c646c59fe537b883eba951329"
WORKSPACE = "11111111-1111-1111-1111-111111111111"
BASE = {
    "core": {
        "workspace_id": WORKSPACE,
        "repository_directory": "repository",
        "item_types_in_scope": ["Notebook"],
    },
    "unpublish": {"skip": True},
}


def load(path: Path, names: set[str], namespace: dict[str, Any]) -> None:
    """Execute only selected unmodified source definitions, never package imports."""
    nodes = ast.parse(path.read_text(encoding="utf-8-sig")).body
    selected = [
        node
        for node in nodes
        if (isinstance(node, (ast.ClassDef, ast.FunctionDef)) and node.name in names)
        or (
            isinstance(node, ast.Assign)
            and isinstance(node.targets[0], ast.Name)
            and node.targets[0].id in names
        )
    ]
    if len(selected) != len(names):
        raise ValueError(f"Missing selected source in {path}")
    exec(
        compile(
            ast.Module(body=selected, type_ignores=[]),
            str(path),
            "exec",
            dont_inherit=False,
        ),
        namespace,
    )


def capture(root: Path) -> dict[str, Any]:
    """Execute real validator/override/environment helpers on synthetic inputs."""

    def git(*args: str) -> str:
        return subprocess.run(
            ["git", "--no-pager", "-C", str(root), *args],
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout.strip()

    if git("rev-parse", "HEAD") != PIN:
        raise ValueError("Wrong CICD source pin")
    source = root / "src" / "fabric_cicd"
    files = [
        source / "constants.py",
        source / "_common" / "_config_validator.py",
        source / "_common" / "_config_utils.py",
    ]
    git("diff", "--exit-code", PIN, "--", *map(str, files))
    constants: dict[str, Any] = {"Enum": Enum}
    load(
        files[0],
        {
            "ItemType",
            "ACCEPTED_ITEM_TYPES",
            "CONFIG_SECTIONS",
            "CONFIG_VALIDATION_MSGS",
        },
        constants,
    )
    constants["VALID_GUID_REGEX"] = (
        r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
    )
    namespace: dict[str, Any] = {
        "Path": Path,
        "re": re,
        "yaml": yaml,
        "InputError": ValueError,
        "constants": SimpleNamespace(**constants),
        "logger": logging.getLogger("config-reference"),
        "_URL_CONSTANTS": set(),
    }
    load(
        files[1],
        {"ConfigValidator", "_get_config_fields", "_validate_guid_format"},
        namespace,
    )
    load(
        files[2],
        {
            "get_config_value",
            "update_setting",
            "extract_workspace_settings",
            "extract_publish_settings",
            "extract_unpublish_settings",
        },
        namespace,
    )
    cases = [
        ("notebook", BASE, "N/A", None),
        (
            "environment",
            {
                "core": {
                    "workspace_id": {"dev": WORKSPACE},
                    "repository_directory": {"dev": "repository"},
                    "item_types_in_scope": {"dev": ["Notebook"]},
                },
                "unpublish": {"skip": {"dev": True}},
            },
            "dev",
            None,
        ),
        (
            "missing-environment",
            {
                "core": {
                    "workspace_id": {"dev": WORKSPACE},
                    "repository_directory": "repository",
                },
            },
            "prod",
            None,
        ),
        (
            "no-environment-with-mapping",
            {
                "core": {
                    "workspace_id": {"dev": WORKSPACE},
                    "repository_directory": "repository",
                },
            },
            "N/A",
            None,
        ),
        (
            "override",
            BASE,
            "dev",
            {"core": {"workspace_id": {"dev": WORKSPACE}}, "publish": {"skip": True}},
        ),
        (
            "override-cannot-create-repository",
            {
                "core": {"workspace_id": WORKSPACE},
            },
            "dev",
            {"core": {"repository_directory": "repository"}},
        ),
        ("override-unknown-setting", BASE, "dev", {"core": {"typo": 1}}),
        ("default-unpublish", {"core": BASE["core"]}, "N/A", None),
        (
            "skip-yaml-booleans",
            yaml.safe_load(
                f"core:\n  workspace_id: {WORKSPACE}\n  repository_directory: repository\npublish:\n  skip: OFF\nunpublish:\n  skip: YES\n"
            ),
            "N/A",
            None,
        ),
        (
            "invalid-item-type",
            {
                "core": {**BASE["core"], "item_types_in_scope": ["notebook"]},
            },
            "N/A",
            None,
        ),
        (
            "optional-missing-environment",
            {
                "core": {**BASE["core"], "parameter": {"other": "parameter.yml"}},
                "unpublish": {"skip": True},
            },
            "dev",
            None,
        ),
        (
            "environment-override-merges",
            {
                "core": {
                    **BASE["core"],
                    "workspace_id": {"dev": WORKSPACE, "prod": WORKSPACE},
                },
                "unpublish": {"skip": True},
            },
            "dev",
            {"core": {"workspace_id": {"dev": "22222222-2222-2222-2222-222222222222"}}},
        ),
    ]
    output = []
    for name, config, environment, overrides in cases:
        validator = namespace["ConfigValidator"]()
        validator.config = copy.deepcopy(config)
        validator.environment = environment
        validator.config_override = copy.deepcopy(overrides)
        validator._apply_and_validate_overrides()
        validator._validate_config_structure()
        validator._validate_config_sections()
        validator._validate_environment_exists()
        result: dict[str, Any] = {"errors": validator.errors}
        if not validator.errors:
            result.update(
                {
                    "config": validator.config,
                    "workspace": namespace["extract_workspace_settings"](
                        validator.config, environment
                    ),
                    "publish": namespace["extract_publish_settings"](
                        validator.config, environment
                    ),
                    "unpublish": namespace["extract_unpublish_settings"](
                        validator.config, environment
                    ),
                }
            )
        output.append(
            {
                "id": name,
                "config": config,
                "environment": environment,
                "overrides": overrides,
                "expected": result,
            }
        )
    return {
        "source": f"microsoft/fabric-cicd@{PIN}",
        "pyyaml": yaml.__version__,
        "boundary": "Actual ConfigValidator overrides/structure/sections/environment before path resolution; actual settings extractors. No service/auth/package initialization.",
        "hashes": {
            "\\".join(path.relative_to(source).parts): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in files
        },
        "cases": output,
    }


def main() -> None:
    """Check fixed new receipts, or capture once before native implementation."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = capture(args.reference_root)
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    else:
        expected = json.loads(
            Path(__file__).with_name("config_source.json").read_text(encoding="utf-8")
        )
        if result != expected:
            raise AssertionError("Pinned config source differs from immutable receipt")
    print(
        json.dumps({"source": PIN, "cases": len(result["cases"]), "status": "passed"})
    )


if __name__ == "__main__":
    main()

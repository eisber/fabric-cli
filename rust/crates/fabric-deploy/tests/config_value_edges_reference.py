# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture remaining config-value edges before native expected results."""

from __future__ import annotations

import argparse
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

from config_reference import BASE, PIN, WORKSPACE, load


def capture(root: Path) -> dict[str, Any]:
    """Execute real validator/extractors/append helper without package imports."""
    head = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if head != PIN:
        raise ValueError("Wrong pinned source")
    source = root / "src" / "fabric_cicd"
    files = [
        source / "constants.py",
        source / "_common" / "_config_validator.py",
        source / "_common" / "_config_utils.py",
        source / "_common" / "_validate_env_vars.py",
        source / "__init__.py",
    ]
    subprocess.run(
        ["git", "-C", str(root), "diff", "--exit-code", PIN, "--", *map(str, files)],
        check=True,
        capture_output=True,
    )
    constants: dict[str, Any] = {"Enum": Enum}
    load(
        files[0],
        {
            "ItemType",
            "ACCEPTED_ITEM_TYPES",
            "CONFIG_SECTIONS",
            "CONFIG_VALIDATION_MSGS",
            "VERSION",
        },
        constants,
    )
    load(files[3], {"VALID_GUID_REGEX"}, constants)
    namespace: dict[str, Any] = {
        "Path": Path,
        "re": re,
        "yaml": yaml,
        "InputError": ValueError,
        "constants": SimpleNamespace(**constants),
        "logger": logging.getLogger("config-value-edges"),
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
    load(files[4], {"append_feature_flag"}, namespace)
    inputs = []
    for index, value in enumerate(
        [None, False, 0, -0.0, "", [], {}, True, 1, " ", [1]]
    ):
        inputs.append((f"override-{index}", {}, "dev", value))
    inputs.extend(
        [
            ("empty-publish-override", {}, "dev", {"publish": {}}),
            ("empty-features-override-invalid", {}, "dev", {"features": []}),
            ("empty-constants-override", {}, "N/A", {"constants": {}}),
            (
                "optional-path-other-environment",
                {},
                "dev",
                {"core": {"parameter": {"prod": "parameter.yml"}}},
            ),
            ("added-name-id-precedence", {}, "dev", {"core": {"workspace": "Name"}}),
            (
                "features-other-environment",
                {},
                "dev",
                {"features": {"prod": ["other"]}},
            ),
            (
                "all-item-types",
                {
                    "core": {
                        **BASE["core"],
                        "item_types_in_scope": [
                            item.value for item in constants["ItemType"]
                        ],
                    }
                },
                "dev",
                None,
            ),
            (
                "environment-case-exact",
                {"core": {**BASE["core"], "workspace_id": {"dev": WORKSPACE}}},
                "DEV",
                None,
            ),
            (
                "environment-extractor-strips",
                {"core": {**BASE["core"], "workspace_id": {" dev ": WORKSPACE}}},
                " dev ",
                None,
            ),
            (
                "optional-extractor-strips",
                {"core": {**BASE["core"], "parameter": {"dev": "parameter.yml"}}},
                " dev ",
                None,
            ),
            (
                "folder-mutual-skip-bool-source-return",
                {
                    "publish": {
                        "folder_exclude_regex": "/A",
                        "folder_path_to_include": ["/B"],
                        "skip": False,
                    }
                },
                "dev",
                None,
            ),
            (
                "folder-mutual-skip-env-invalid",
                {
                    "publish": {
                        "folder_exclude_regex": "/A",
                        "folder_path_to_include": ["/B"],
                        "skip": {"dev": False},
                    }
                },
                "dev",
                None,
            ),
            (
                "folder-other-env-prefix-invalid",
                {
                    "publish": {
                        "folder_path_to_include": {"dev": ["/A"], "prod": ["relative"]}
                    }
                },
                "dev",
                None,
            ),
            (
                "unknown-source-fields-retained",
                {
                    "core": {**BASE["core"], "future": {"typed": [False, 1]}},
                    "extra.section": {"typed": None},
                },
                "dev",
                None,
            ),
        ]
    )
    cases = []
    for name, changes, environment, overrides in inputs:
        config = {**copy.deepcopy(BASE), **changes}
        validator = namespace["ConfigValidator"]()
        validator.config = copy.deepcopy(config)
        validator.environment = environment
        validator.config_override = copy.deepcopy(overrides)
        expected = None
        error = None
        try:
            validator._apply_and_validate_overrides()
            validator._validate_config_structure()
            validator._validate_config_sections()
            validator._validate_environment_exists()
        except (AttributeError, TypeError) as failure:
            error = type(failure).__name__
        valid = not error and not validator.errors
        if valid:
            expected = {
                "document": validator.config,
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
        cases.append(
            {
                "kind": "config",
                "id": name,
                "config": config,
                "environment": environment,
                "overrides": overrides,
                "valid": valid,
                "expected": expected,
            }
        )
    for flags in [
        [""],
        [" "],
        ["Case", "case"],
        ["base", "base"],
        ["", " ", "base", "custom_CASE"],
    ]:
        namespace["constants"].FEATURE_FLAG = set()
        for flag in flags:
            namespace["append_feature_flag"](flag)
        cases.append(
            {
                "kind": "caller-flags",
                "flags": flags,
                "expected": sorted(namespace["constants"].FEATURE_FLAG),
            }
        )
    return {
        "source": f"microsoft/fabric-cicd@{PIN}",
        "pyyaml": yaml.__version__,
        "sourceHashes": {
            "\\".join(path.relative_to(source).parts): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in files
        },
        "boundary": "Actual source override/structure/sections/environment/extractors and append_feature_flag. No path resolution, URL constant cases, package initialization, environment or service access.",
        "cases": cases,
    }


def main() -> None:
    """Capture once before Rust expectations; compare fixed data thereafter."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = json.loads(json.dumps(capture(args.reference_root)))
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__)
        .with_name("config_value_edges_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError(
            "Pinned config-value source differs from immutable receipt"
        )
    print(json.dumps({"cases": len(result["cases"]), "status": "passed"}))


if __name__ == "__main__":
    main()

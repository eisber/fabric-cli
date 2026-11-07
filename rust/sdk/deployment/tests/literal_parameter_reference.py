# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture real pinned parameter validation and literal-transform observations."""

from __future__ import annotations

import argparse
import copy
import importlib.util
import json
from pathlib import Path
from typing import Any
from types import SimpleNamespace

ROOT = Path(__file__).parents[3]
spec = importlib.util.spec_from_file_location(
    "deploy_literal_source_oracle",
    ROOT / "tests" / "fixtures" / "deploy" / "reference_oracle.py",
)
oracle = importlib.util.module_from_spec(spec)
import sys

sys.modules[spec.name] = oracle
spec.loader.exec_module(oracle)


def capture(root: Path) -> dict[str, Any]:
    """Run original schema/transform methods, without parameter file or auth setup."""
    installed = {name: oracle.version(name) for name in oracle.DEPENDENCIES}
    if installed != oracle.DEPENDENCIES:
        raise ValueError(
            f"Literal source capture requires {oracle.DEPENDENCIES}; found {installed}"
        )
    namespace, hashes = oracle.reference_namespace(root)
    source = root / "src" / "fabric_cicd"
    oracle.load_nodes(
        source / "constants.py", {"PARAM_NAMES"}, namespace["constants"].__dict__
    )
    oracle.load_nodes(
        source / "_parameter" / "_utils.py",
        {
            "is_valid_structure",
            "_check_parameter_structure",
            "_check_semantic_model_binding_structure",
        },
        namespace,
    )
    oracle.load_nodes(source / "_parameter" / "_parameter.py", {"Parameter"}, namespace)
    cases = []
    for name, entries in [
        ("literal", [{"find_value": "source", "replace_value": {"dev": "target"}}]),
        (
            "ordered",
            [
                {"find_value": "source", "replace_value": {"dev": "middle"}},
                {"find_value": "middle", "replace_value": {"dev": "target"}},
            ],
        ),
        ("all-alias", [{"find_value": "source", "replace_value": {"_aLl_": "target"}}]),
        (
            "literal-backreference",
            [{"find_value": "source", "replace_value": {"dev": r"\1"}}],
        ),
        (
            "missing-environment",
            [{"find_value": "source", "replace_value": {"other": "target"}}],
        ),
        (
            "string-false-flags",
            [
                {
                    "find_value": "source",
                    "replace_value": {"dev": "target"},
                    "is_regex": "false",
                    "ignore_case": "false",
                }
            ],
        ),
        (
            "empty-replacement-invalid",
            [{"find_value": "source", "replace_value": {"dev": ""}}],
        ),
        (
            "yaml-boolean-flag-invalid",
            [
                {
                    "find_value": "source",
                    "replace_value": {"dev": "target"},
                    "is_regex": False,
                }
            ],
        ),
        (
            "mixed-all-invalid",
            [
                {
                    "find_value": "source",
                    "replace_value": {"_ALL_": "default", "dev": "target"},
                }
            ],
        ),
        (
            "wrong-replacement-type",
            [{"find_value": "source", "replace_value": {"dev": 2}}],
        ),
    ]:
        parameters = {"find_replace": entries}
        validator = object.__new__(namespace["Parameter"])
        validator.environment_parameter = copy.deepcopy(parameters)
        validator.environment = "dev"
        validator.item_type_in_scope = ["Notebook"]
        validator.repository_directory = Path("synthetic")
        steps = [
            validator._validate_parameter_names(),
            validator._validate_parameter_structure(),
            validator._validate_parameter("find_replace"),
        ]
        valid = all(step[0] for step in steps)
        output = None
        if valid:
            target = SimpleNamespace(
                environment="dev",
                environment_parameter=copy.deepcopy(parameters),
                repository_directory=Path("synthetic"),
                bulk_publish_enabled=False,
            )
            output = namespace["_replace_parameters"](
                target,
                SimpleNamespace(
                    contents="source source SOURCE",
                    file_path=Path("notebook-content.py"),
                ),
                SimpleNamespace(type="Notebook", name="Book"),
            )
        cases.append(
            {
                "id": name,
                "parameters": parameters,
                "environment": "dev",
                "input": "source source SOURCE",
                "valid": valid,
                "steps": steps,
                "expected": output,
            }
        )
    return {
        "source": oracle.SOURCE,
        "sourceHashes": hashes,
        "boundary": "Actual Parameter name/structure/find_replace schema validation and FabricWorkspace._replace_parameters, with no filters, dynamic variables, file setup, constructors or service callbacks.",
        "cases": cases,
    }


def main() -> None:
    """Capture once before native expectation, otherwise compare fixed observations."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = capture(args.reference_root)
    # JSON capture represents original tuples as arrays.
    result = json.loads(json.dumps(result))
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__)
        .with_name("literal_parameter_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError(
            "Pinned literal parameter source differs from immutable receipt"
        )
    print(
        json.dumps(
            {
                "source": oracle.CICD_COMMIT,
                "cases": len(result["cases"]),
                "status": "passed",
            }
        )
    )


if __name__ == "__main__":
    main()

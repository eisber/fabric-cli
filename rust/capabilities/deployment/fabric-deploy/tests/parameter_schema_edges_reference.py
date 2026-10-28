# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture source loader/gateway edges without source expectation changes."""

from __future__ import annotations

import argparse
import copy
import json
import shutil
from pathlib import Path
from typing import Any

from literal_parameter_reference import oracle

GUID = "11111111-1111-1111-1111-111111111111"
BASE = {"find_replace": [{"find_value": "source", "replace_value": {"dev": "target"}}]}


def capture(root: Path) -> dict[str, Any]:
    """Run actual parameter loading and validators over an owned synthetic file."""
    namespace, hashes = oracle.reference_namespace(root)
    if {
        name: oracle.version(name) for name in oracle.DEPENDENCIES
    } != oracle.DEPENDENCIES:
        raise ValueError("Pinned oracle dependencies required")
    source = root / "src" / "fabric_cicd"
    env_source = source / "_common" / "_validate_env_vars.py"
    oracle.git(root, "diff", "--exit-code", oracle.CICD_COMMIT, "--", str(env_source))
    oracle.load_nodes(
        source / "constants.py", {"PARAM_NAMES"}, namespace["constants"].__dict__
    )
    oracle.load_nodes(
        source / "_common" / "_validate_env_vars.py",
        {"VALID_GUID_REGEX"},
        namespace["constants"].__dict__,
    )
    oracle.load_nodes(source / "_parameter" / "_parameter.py", {"Parameter"}, namespace)
    oracle.load_nodes(
        source / "_parameter" / "_utils.py",
        {
            "is_valid_structure",
            "_check_parameter_structure",
            "_check_semantic_model_binding_structure",
            "replace_variables_in_parameter_file",
        },
        namespace,
    )
    namespace["constants"].FEATURE_FLAG = set()
    directory = Path("source_capture_parameter_schema_edges")
    directory.mkdir()
    try:
        (directory / "template.yml").write_text("find_replace: []\n", encoding="utf-8")
        inputs = [
            (f"extend-{index}", {**copy.deepcopy(BASE), "extend": value})
            for index, value in enumerate(
                [False, 0, "", None, [], {}, True, 1, "template.yml", ["template.yml"]]
            )
        ]
        new_binding = {"default": {"connection_id": {"dev": GUID}}}
        for index, gateway in enumerate(
            [
                [{"gateway_id": GUID, "dataset_name": "Model"}],
                "ignored",
                True,
                1,
                {"ignored": "data"},
                False,
                [],
            ]
        ):
            inputs.append(
                (
                    f"new-binding-gateway-{index}",
                    {
                        "semantic_model_binding": copy.deepcopy(new_binding),
                        "gateway_binding": gateway,
                    },
                )
            )
        inputs.append(("empty-gateway-only", {"gateway_binding": []}))
        cases = []
        for name, document in inputs:
            path = directory / "parameter.yml"
            path.write_text(
                namespace["yaml"].safe_dump(document, sort_keys=False), encoding="utf-8"
            )
            validator = object.__new__(namespace["Parameter"])
            validator.environment = "dev"
            validator.item_type_in_scope = ["Notebook", "SemanticModel"]
            validator.repository_directory = directory.resolve()
            validator.parameter_file_path = path.resolve()
            valid, loaded = validator._validate_load_parameters_to_dict()
            validator.environment_parameter = loaded
            if valid:
                try:
                    validator._handle_gateway_binding_parameter()
                    steps = [
                        validator._validate_parameter_names(),
                        validator._validate_parameter_structure(),
                    ]
                    for parameter in [
                        "find_replace",
                        "key_value_replace",
                        "spark_pool",
                        "semantic_model_binding",
                    ]:
                        if parameter in validator.environment_parameter:
                            steps.append(validator._validate_parameter(parameter))
                    valid = all(step[0] for step in steps)
                except (AttributeError, TypeError):
                    valid = False
            cases.append(
                {
                    "id": name,
                    "document": document,
                    "valid": valid,
                    "expected": validator.environment_parameter if valid else None,
                }
            )
    finally:
        shutil.rmtree(directory)
    return {
        "source": oracle.SOURCE,
        "sourceHashes": hashes,
        "dependencies": oracle.DEPENDENCIES,
        "boundary": "Unmodified source parameter loader, gateway conversion, schema validators; owned local files only, no constructors/auth/services/ambient environment. Template file is fixed empty rule-list input.",
        "cases": cases,
    }


def main() -> None:
    """Capture once before native expectations; compare fixed data thereafter."""
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
        .with_name("parameter_schema_edges_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError(
            "Pinned parameter schema edges differ from immutable receipt"
        )
    print(json.dumps({"cases": len(result["cases"]), "status": "passed"}))


if __name__ == "__main__":
    main()

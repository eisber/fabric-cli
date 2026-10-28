# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual source configured pipelines and item-parameter schemas."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from literal_parameter_reference import oracle

GUID = "11111111-1111-1111-1111-111111111111"
RULE = {"find_value": "source", "replace_value": {"dev": "target"}}


def capture(root: Path) -> dict[str, Any]:
    """Run source pipeline using injected concrete paths and workspace callbacks."""
    namespace, hashes = oracle.reference_namespace(root)
    if {
        name: oracle.version(name) for name in oracle.DEPENDENCIES
    } != oracle.DEPENDENCIES:
        raise ValueError("Pinned oracle dependencies required")
    source = root / "src" / "fabric_cicd"
    oracle.load_nodes(
        source / "constants.py", {"PARAM_NAMES"}, namespace["constants"].__dict__
    )
    oracle.load_nodes(
        source / "_common" / "_validate_env_vars.py",
        {"VALID_GUID_REGEX"},
        namespace["constants"].__dict__,
    )
    hashes["_common/_validate_env_vars.py"] = hashlib.sha256(
        (source / "_common" / "_validate_env_vars.py").read_bytes()
    ).hexdigest()
    oracle.load_nodes(source / "_parameter" / "_parameter.py", {"Parameter"}, namespace)
    oracle.load_nodes(
        source / "_parameter" / "_utils.py",
        {
            "is_valid_structure",
            "_check_parameter_structure",
            "_check_semantic_model_binding_structure",
        },
        namespace,
    )
    namespace["_resolve_filter_path"] = lambda workspace, value: (
        None
        if not value
        else [Path(path) for path in (value if isinstance(value, list) else [value])]
    )
    cases = []
    for index, extra in enumerate(
        [
            {},
            {"item_type": "Notebook"},
            {"item_type": "Report"},
            {"item_type": []},
            {"item_type": False},
            {"item_name": "Book"},
            {"item_name": ["Other"]},
            {"file_path": "content.py"},
            {"file_path": "other.py"},
            {
                "item_type": ["Report", "Notebook"],
                "item_name": ["Book"],
                "file_path": ["content.py"],
            },
        ]
    ):
        parameters = {"find_replace": [{**RULE, **extra}]}
        target = SimpleNamespace(
            environment="dev",
            environment_parameter=copy.deepcopy(parameters),
            repository_directory=Path("synthetic"),
            bulk_publish_enabled=False,
            workspace_id=GUID,
        )
        expected = namespace["_replace_parameters"](
            target,
            SimpleNamespace(contents="source SOURCE", file_path=Path("content.py")),
            SimpleNamespace(type="Notebook", name="Book"),
        )
        cases.append(
            {
                "kind": "pipeline",
                "id": f"filter-{index}",
                "parameters": parameters,
                "input": "source SOURCE",
                "file": "content.py",
                "expected": expected,
            }
        )
    for index, (parameters, content) in enumerate(
        [
            (
                {
                    "find_replace": [
                        {"find_value": "source", "replace_value": {"other": "target"}}
                    ]
                },
                "source",
            ),
            (
                {
                    "find_replace": [
                        {"find_value": "source", "replace_value": {"_ALL_": "target"}}
                    ]
                },
                "source",
            ),
            (
                {
                    "find_replace": [
                        {
                            "find_value": "$workspace.$id",
                            "replace_value": {"dev": "target"},
                        }
                    ]
                },
                GUID,
            ),
            (
                {
                    "find_replace": [
                        {
                            "find_value": "source",
                            "replace_value": {"dev": "$workspace.$id"},
                        }
                    ]
                },
                "source",
            ),
            (
                {
                    "find_replace": [
                        {
                            "find_value": "absent",
                            "replace_value": {"dev": "$workspace.$id"},
                        }
                    ]
                },
                "source",
            ),
            (
                {
                    "key_value_replace": [
                        {"find_key": "$.x", "replace_value": {"_ALL_": False}}
                    ]
                },
                '{"x": 1}',
            ),
            (
                {
                    "key_value_replace": [
                        {"find_key": "$.x", "replace_value": {"dev": "$workspace.$id"}}
                    ]
                },
                '{"x": 1}',
            ),
            (
                {
                    "key_value_replace": [
                        {
                            "find_key": "$.missing",
                            "replace_value": {"dev": "$workspace.$id"},
                        }
                    ]
                },
                '{"x": 1}',
            ),
            (
                {
                    "find_replace": [
                        {"find_value": "middle", "replace_value": {"dev": "final"}}
                    ],
                    "key_value_replace": [
                        {"find_key": "$.x", "replace_value": {"dev": "middle"}}
                    ],
                },
                '{"x": "source"}',
            ),
        ]
    ):
        target = SimpleNamespace(
            environment="dev",
            environment_parameter=copy.deepcopy(parameters),
            repository_directory=Path("synthetic"),
            bulk_publish_enabled=False,
            workspace_id=GUID,
        )
        expected = namespace["_replace_parameters"](
            target,
            SimpleNamespace(contents=content, file_path=Path("content.py")),
            SimpleNamespace(type="Notebook", name="Book"),
        )
        cases.append(
            {
                "kind": "pipeline",
                "id": f"ordered-{index}",
                "parameters": parameters,
                "input": content,
                "file": "content.py",
                "expected": expected,
            }
        )
    for index, parameters in enumerate(
        [
            {
                "spark_pool": [
                    {
                        "instance_pool_id": "pool",
                        "replace_value": {"dev": {"type": "Capacity", "name": "Pool"}},
                    }
                ]
            },
            {
                "spark_pool": [
                    {
                        "instance_pool_id": "pool",
                        "replace_value": {"dev": {"type": "Workspace", "name": "Pool"}},
                    }
                ]
            },
            {
                "spark_pool": [
                    {
                        "instance_pool_id": "pool",
                        "replace_value": {"dev": {"type": "capacity", "name": "Pool"}},
                    }
                ]
            },
            {
                "spark_pool": [
                    {
                        "instance_pool_id": "pool",
                        "replace_value": {"dev": {"type": "Capacity", "name": ""}},
                    }
                ]
            },
            {
                "spark_pool": [
                    {
                        "instance_pool_id": "pool",
                        "replace_value": {
                            "dev": {"type": "Capacity", "name": "Pool", "extra": 1}
                        },
                    }
                ]
            },
            {
                "semantic_model_binding": [
                    {"connection_id": GUID, "semantic_model_name": "Model"}
                ]
            },
            {
                "semantic_model_binding": [
                    {"connection_id": {"dev": GUID}, "semantic_model_name": "Model"}
                ]
            },
            {"semantic_model_binding": {"default": {"connection_id": {"dev": GUID}}}},
            {
                "semantic_model_binding": {
                    "default": {"connection_id": {"_ALL_": [GUID]}}
                }
            },
            {"semantic_model_binding": {"default": {"connection_id": GUID}}},
            {
                "semantic_model_binding": {
                    "models": [
                        {
                            "semantic_model_name": ["Model"],
                            "connection_id": {"dev": [GUID]},
                        }
                    ]
                }
            },
            {"semantic_model_binding": {"models": []}},
            {
                "semantic_model_binding": {
                    "models": [
                        {
                            "semantic_model_name": "",
                            "connection_id": {"dev": GUID},
                            "extra": False,
                        }
                    ]
                }
            },
            {"gateway_binding": [{"gateway_id": GUID, "dataset_name": ["Model"]}]},
            {"gateway_binding": [{"gateway_id": "bad", "dataset_name": "Model"}]},
        ]
    ):
        validator = object.__new__(namespace["Parameter"])
        validator.environment_parameter = copy.deepcopy(parameters)
        validator.environment = "dev"
        validator.item_type_in_scope = ["Notebook", "Environment", "SemanticModel"]
        validator.repository_directory = Path("synthetic")
        validator._handle_gateway_binding_parameter()
        steps = [
            validator._validate_parameter_structure(),
            validator._validate_parameter_names(),
        ]
        for name in ["spark_pool", "semantic_model_binding"]:
            if name in validator.environment_parameter:
                steps.append(validator._validate_parameter(name))
        cases.append(
            {
                "kind": "schema",
                "id": f"item-schema-{index}",
                "parameters": parameters,
                "valid": all(step[0] for step in steps),
                "normalized": validator.environment_parameter,
            }
        )
    return {
        "source": oracle.SOURCE,
        "sourceHashes": hashes,
        "dependencies": oracle.DEPENDENCIES,
        "boundary": "Actual _replace_parameters with concrete path resolver injection (not path expansion oracle), actual item-parameter validators/gateway conversion, no constructor, service/auth, ambient filesystem or environment.",
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
        .with_name("parameter_pipeline_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError("Pinned parameter pipeline differs from immutable receipt")
    print(json.dumps({"cases": len(result["cases"]), "status": "passed"}))


if __name__ == "__main__":
    main()

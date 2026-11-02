# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual full-schema regex/JSONPath programs and source payload parts."""

from __future__ import annotations

import argparse
import ast
import base64
import copy
import hashlib
import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from literal_parameter_reference import oracle


def capture(root: Path) -> dict[str, Any]:
    """Execute original full-schema steps then original transform/payload bodies."""
    installed = {name: oracle.version(name) for name in oracle.DEPENDENCIES}
    if installed != oracle.DEPENDENCIES:
        raise ValueError(f"Requires pinned dependencies {oracle.DEPENDENCIES}")
    namespace, hashes = oracle.reference_namespace(root)
    source = root / "src" / "fabric_cicd"
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
    namespace["constants"].PARAM_NAMES = [
        "find_replace",
        "key_value_replace",
        "spark_pool",
        "semantic_model_binding",
    ]

    def imported(name: str, *args: Any) -> Any:
        if name == "fabric_cicd._parameter._utils":
            return SimpleNamespace(**namespace)
        if name == "jsonpath_ng.ext":
            return SimpleNamespace(parse=namespace["parse"])
        raise ImportError(f"Out-of-scope source import {name}")

    namespace["__builtins__"]["__import__"] = imported
    path = source / "_common" / "_file.py"
    oracle.git(root, "diff", "--exit-code", oracle.CICD_COMMIT, "--", str(path))
    file_class = next(
        node
        for node in ast.parse(path.read_text(encoding="utf-8-sig")).body
        if isinstance(node, ast.ClassDef) and node.name == "File"
    )
    property_node = next(
        node
        for node in file_class.body
        if isinstance(node, ast.FunctionDef) and node.name == "base64_payload"
    )
    payload_namespace = {"base64": base64}
    exec(
        compile(ast.Module(body=[property_node], type_ignores=[]), str(path), "exec"),
        payload_namespace,
    )
    payload = payload_namespace["base64_payload"].fget
    cases = []
    for name, content, parameters in [
        (
            "capture-literal-backreference",
            "id=old; id=older;",
            {
                "find_replace": [
                    {
                        "find_value": r"id=(\w+);",
                        "is_regex": "true",
                        "replace_value": {"dev": r"\1$1\g<1>"},
                    }
                ],
            },
        ),
        (
            "typed-jsonpath-before-regex-and-literal",
            '{"value":"source","id":"id=old;"}',
            {
                "find_replace": [
                    {"find_value": "middle", "replace_value": {"dev": "final"}},
                    {
                        "find_value": r"id=(\w+);",
                        "is_regex": "true",
                        "replace_value": {"dev": "new"},
                    },
                ],
                "key_value_replace": [
                    {"find_key": "$.value", "replace_value": {"dev": "middle"}}
                ],
            },
        ),
        (
            "literal-ignore-case",
            "A.B a.b axb",
            {
                "find_replace": [
                    {
                        "find_value": "a.b",
                        "ignore_case": "true",
                        "replace_value": {"dev": r"\1$new"},
                    }
                ],
            },
        ),
        (
            "typed-jsonpath-filter",
            '{"items":[{"n":1,"value":"keep"},{"n":"2","value":"change"}]}',
            {
                "key_value_replace": [
                    {
                        "find_key": "$.items[?(@.n >= 2)].value",
                        "replace_value": {"dev": {"enabled": False, "count": 2}},
                    }
                ],
            },
        ),
    ]:
        validator = object.__new__(namespace["Parameter"])
        validator.environment_parameter = copy.deepcopy(parameters)
        validator.environment = "dev"
        validator.item_type_in_scope = ["Notebook"]
        validator.repository_directory = Path("synthetic")
        steps = [
            validator._validate_parameter_names(),
            validator._validate_parameter_structure(),
        ]
        for key in ["key_value_replace", "find_replace"]:
            if key in parameters:
                steps.append(validator._validate_parameter(key))
        if not all(step[0] for step in steps):
            raise AssertionError((name, steps))
        workspace = SimpleNamespace(
            environment="dev",
            environment_parameter=copy.deepcopy(parameters),
            repository_directory=Path("synthetic"),
            bulk_publish_enabled=False,
        )
        output = namespace["_replace_parameters"](
            workspace,
            SimpleNamespace(contents=content, file_path=Path("notebook-content.py")),
            SimpleNamespace(type="Notebook", name="Book"),
        )
        cases.append(
            {
                "id": name,
                "input": content,
                "parameters": parameters,
                "schemaSteps": steps,
                "expected": output,
                "expectedPart": payload(
                    SimpleNamespace(
                        type="text",
                        contents=output,
                        relative_path="notebook-content.py",
                    )
                ),
            }
        )
    return {
        "source": oracle.SOURCE,
        "sourceHashes": hashes,
        "fileSourceSHA256": hashlib.sha256(path.read_bytes()).hexdigest(),
        "dependencies": installed,
        "boundary": "Actual Parameter name/structure/per-section schema validation, unchanged FabricWorkspace._replace_parameters, and original File.base64_payload. No runtime/auth/service/metadata/constructor initialization. Source flags are strings, not leaf-only booleans.",
        "cases": json.loads(json.dumps(cases)),
    }


def main() -> None:
    """Capture once before native expectation, otherwise check immutable observations."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = capture(args.reference_root)
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__)
        .with_name("transform_program_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError("Pinned transform program differs from immutable receipt")
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

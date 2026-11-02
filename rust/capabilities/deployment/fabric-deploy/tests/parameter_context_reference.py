# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture injected parameter contexts without source filesystem/global reads."""

from __future__ import annotations

import argparse
import copy
import dataclasses
import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from literal_parameter_reference import oracle


def capture(root: Path) -> dict[str, Any]:
    """Run actual source filter, dynamic parser, template and expansion bodies."""
    namespace, hashes = oracle.reference_namespace(root)
    if {
        name: oracle.version(name) for name in oracle.DEPENDENCIES
    } != oracle.DEPENDENCIES:
        raise ValueError("Pinned oracle dependencies required")
    source = root / "src" / "fabric_cicd"
    oracle.load_nodes(source / "_parameter" / "_parameter.py", {"Parameter"}, namespace)
    oracle.load_nodes(
        source / "_parameter" / "_utils.py",
        {"replace_variables_in_parameter_file"},
        namespace,
    )
    constants = namespace["constants"]
    cases = []
    for kind, name, path in [
        (None, None, None),
        ("Notebook", None, None),
        ("notebook", None, None),
        (["Report", "Notebook"], "Book", None),
        ([], None, None),
        (False, None, None),
        ("", None, None),
        (None, "", None),
        (None, ["Other"], None),
        (None, ["Other", "Book"], ["content.py"]),
        (None, None, []),
        (None, None, ["other.py"]),
    ]:
        expected = namespace["check_replacement"](
            kind,
            name,
            [Path(value) for value in path] if path is not None else None,
            "Notebook",
            "Book",
            Path("content.py"),
        )
        cases.append(
            {
                "kind": "filter",
                "type": kind,
                "name": name,
                "paths": path,
                "expected": expected,
            }
        )
    for variable in [
        "$workspace.$id",
        "$workspace.id",
        "$workspace.$name",
        "$workspace.$name_encoded",
        "$workspace.Other Workspace.$id",
        "$workspace.Other.Workspace",
        "$items.Notebook.Book.$id",
        "$items.Notebook.Book.with.dots.$NAME",
        "$workspace.Other.$items.Notebook.Book.$id",
        "$items.notebook.Book.$id",
        "$items.Notebook..$id",
        "$workspace.$bad",
        "$unsupported",
    ]:
        expected = None
        try:
            expected = dataclasses.asdict(namespace["parse_dynamic_variable"](variable))
        except namespace["ParsingError"]:
            pass
        cases.append({"kind": "dynamic", "variable": variable, "expected": expected})
    validator = object.__new__(namespace["Parameter"])
    for base, template in [
        ({"find_replace": [{"a": 1}]}, {"find_replace": [{"b": 2}]}),
        ({"x": {"a": [1]}}, {"x": {"a": [2], "b": False}}),
        ({"x": "base"}, {"x": {"value": "template"}}),
        ({"x": 1}, {"extend": ["ignored.yml"], "other": 2}),
    ]:
        expected = validator._merge_template_dict(
            copy.deepcopy(base), copy.deepcopy(template)
        )
        cases.append(
            {"kind": "merge", "base": base, "template": template, "expected": expected}
        )
    for enabled in [False, True]:
        for raw in [
            "$ENV:DEFINED $ENV:MISSING $ENV:EMPTY",
            "$env:DEFINED $ENV:DEFINED.suffix",
        ]:
            constants.FEATURE_FLAG = (
                {"enable_environment_variable_replacement"} if enabled else set()
            )
            constants.ENVIRONMENT_VARIABLE_PREFIX = "$ENV:"
            namespace["os"] = SimpleNamespace(
                environ={"DEFINED": "resolved", "EMPTY": ""}
            )
            expected = namespace["replace_variables_in_parameter_file"](raw)
            cases.append(
                {
                    "kind": "environment",
                    "enabled": enabled,
                    "input": raw,
                    "expected": expected,
                }
            )
    return {
        "source": oracle.SOURCE,
        "sourceHashes": hashes,
        "dependencies": oracle.DEPENDENCIES,
        "boundary": "Unmodified source functions with synthetic concrete path lists, in-memory merge inputs, and injected environment mapping. No filesystem discovery, service/auth or ambient environment reads.",
        "cases": cases,
    }


def main() -> None:
    """Capture once before native expectations; compare on all later runs."""
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
        .with_name("parameter_context_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError("Pinned parameter contexts differ from immutable receipt")
    print(json.dumps({"cases": len(result["cases"]), "status": "passed"}))


if __name__ == "__main__":
    main()

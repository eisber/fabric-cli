# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual source conditional/repeated metadata evaluation points."""

from __future__ import annotations

import argparse
import copy
import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from literal_parameter_reference import oracle

GUID = "11111111-1111-1111-1111-111111111111"
ITEM = "22222222-2222-2222-2222-222222222222"
VARIABLE = "$items.Notebook.Book.$id"


def capture(root: Path) -> dict[str, Any]:
    """Run unchanged source consumers with traced synthetic metadata callbacks."""
    namespace, hashes = oracle.reference_namespace(root)
    if {
        name: oracle.version(name) for name in oracle.DEPENDENCIES
    } != oracle.DEPENDENCIES:
        raise ValueError("Pinned oracle dependencies required")
    namespace["_resolve_filter_path"] = lambda workspace, value: (
        None
        if not value
        else [Path(path) for path in (value if isinstance(value, list) else [value])]
    )

    class Host:
        """Only explicit data/callbacks required by the original source bodies."""

        def __init__(
            self, parameters: dict[str, Any], calls: list[str], name: str
        ) -> None:
            self.environment_parameter = copy.deepcopy(parameters)
            self.environment = "dev"
            self.repository_directory = Path("synthetic")
            self.bulk_publish_enabled = False
            self.workspace_items = {"Notebook": {"Book": {"id": ITEM}}}
            self.calls = calls
            self.name = name

        @property
        def workspace_id(self) -> str:
            """Record source access without inventing a network request."""
            self.calls.append("$workspace.$id")
            return GUID

        def _refresh_deployed_items(self) -> None:
            self.calls.append(VARIABLE)

        def _resolve_workspace_name(self) -> str:
            self.calls.append("$workspace.$name")
            return self.name

        def _resolve_workspace_id(self, name: str) -> str:
            self.calls.append(f"$workspace.{name}")
            return ITEM

    find = {"find_value": "source", "replace_value": {"dev": VARIABLE}}
    key = {"find_key": "$.x[*]", "replace_value": {"dev": VARIABLE}}
    inputs = [
        ("find-match", {"find_replace": [find]}, "source", "Name"),
        ("find-no-match", {"find_replace": [find]}, "absent", "Name"),
        (
            "find-filter-excluded",
            {"find_replace": [{**find, "item_type": "Report"}]},
            "source",
            "Name",
        ),
        (
            "find-env-missing",
            {"find_replace": [{**find, "replace_value": {"prod": VARIABLE}}]},
            "source",
            "Name",
        ),
        (
            "find-file-excluded",
            {"find_replace": [{**find, "file_path": "other.py"}]},
            "source",
            "Name",
        ),
        (
            "find-repeated-refresh",
            {
                "find_replace": [
                    find,
                    {"find_value": ITEM, "replace_value": {"dev": VARIABLE}},
                ]
            },
            "source",
            "Name",
        ),
        ("key-two-refreshes", {"key_value_replace": [key]}, '{"x":[1,2]}', "Name"),
        ("key-no-match", {"key_value_replace": [key]}, '{"other":[1,2]}', "Name"),
        (
            "key-env-missing",
            {"key_value_replace": [{**key, "replace_value": {"prod": VARIABLE}}]},
            '{"x":[1,2]}',
            "Name",
        ),
        (
            "key-filter-excluded",
            {"key_value_replace": [{**key, "item_name": "Other"}]},
            '{"x":[1,2]}',
            "Name",
        ),
        ("key-nonstructured", {"key_value_replace": [key]}, "source", "Name"),
        (
            "key-before-find",
            {
                "find_replace": [find],
                "key_value_replace": [
                    {"find_key": "$.x", "replace_value": {"dev": "source"}}
                ],
            },
            '{"x":"old"}',
            "Name",
        ),
        (
            "dynamic-find-before-filter",
            {
                "find_replace": [
                    {
                        "find_value": "$workspace.$id",
                        "replace_value": {"dev": "target"},
                        "item_name": "Other",
                    }
                ]
            },
            GUID,
            "Name",
        ),
        (
            "dynamic-find-before-env",
            {
                "find_replace": [
                    {
                        "find_value": "$workspace.Other",
                        "replace_value": {"prod": "target"},
                    }
                ]
            },
            ITEM,
            "Name",
        ),
        (
            "empty-dynamic-find",
            {
                "find_replace": [
                    {
                        "find_value": "$workspace.$name",
                        "replace_value": {"dev": "target"},
                    }
                ]
            },
            "source",
            "",
        ),
        (
            "literal-environment-find",
            {
                "find_replace": [
                    {"find_value": "$ENV:RAW", "replace_value": {"dev": "target"}}
                ]
            },
            "$ENV:RAW",
            "Name",
        ),
        (
            "literal-environment-find-no-match",
            {
                "find_replace": [
                    {"find_value": "$ENV:RAW", "replace_value": {"dev": "target"}}
                ]
            },
            "source",
            "Name",
        ),
    ]
    cases = []
    for name, parameters, content, workspace_name in inputs:
        calls: list[str] = []
        expected = namespace["_replace_parameters"](
            Host(parameters, calls, workspace_name),
            SimpleNamespace(contents=content, file_path=Path("content.py")),
            SimpleNamespace(type="Notebook", name="Book"),
        )
        cases.append(
            {
                "id": name,
                "parameters": parameters,
                "input": content,
                "workspace_name": workspace_name,
                "expected": expected,
                "calls": calls,
            }
        )
    return {
        "source": oracle.SOURCE,
        "sourceHashes": hashes,
        "dependencies": oracle.DEPENDENCIES,
        "boundary": "Actual _replace_parameters/extract/JSONPath consumers with traced explicit metadata callbacks and concrete path-filter injection; no constructors, network, ambient filesystem/environment, auth or bulk cache.",
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
        .with_name("parameter_resolution_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError(
            "Pinned metadata resolution differs from immutable receipt"
        )
    print(json.dumps({"cases": len(result["cases"]), "status": "passed"}))


if __name__ == "__main__":
    main()

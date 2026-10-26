# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independent Python-default wire and source recursive ACL response bytes."""

import __future__
import argparse
import ast
import json
import subprocess

from reference import HERE, ROOT, SOURCE


def document() -> dict:
    """Use original JSON dumps inputs and actual ApiResponse.append_text."""
    values = [
        {"name": "acl-principal", "body": {"principal": {"id": "identity", "type": "User"}, "role": "viewer"}},
        {"name": "unicode-controls", "body": {"unicode": "\u96ea\U0001f30d", "control": "\x00\n", "float": -0.0}},
        {"name": "label", "body": {"items": [{"id": "item", "type": "Notebook"}], "labelId": "label", "assignmentMethod": "Standard"}},
        {"name": "catalog", "body": {"search": "\u96ea", "pageSize": 1000, "filter": "Type eq 'Report'"}},
        {"name": "numeric", "body": {"small": 1e-7, "large": 1e20, "fraction": 1.25}},
    ]
    for value in values:
        value["wire"] = json.dumps(value["body"])
    raw = subprocess.run(["git", "show", f"{SOURCE}:src/fabric_cli/client/fab_api_types.py"],
        cwd=ROOT, check=True, capture_output=True).stdout
    tree = ast.parse(raw.decode())
    tree.body = [node for node in tree.body if isinstance(node, ast.ClassDef)]
    namespace = {"json": json}
    exec(compile(tree, "fab_api_types.py", "exec", flags=__future__.annotations.compiler_flag), namespace)
    first = '{"value": [{"id": "first", "note": "\\u96ea", "extra": "identity: value"}], "continuationToken": "next"}'
    second = '{"value": [{"id": "second", "name": "\\ud83c\\udf0d"}]}'
    response = namespace["ApiResponse"](200, first, b"", {})
    response.append_text(second)
    return {"source_commit": SOURCE, "requests": values, "merged": {
        "first": first, "second": second, "wire": response.text,
        "identities": [{"value": identity, "matched": identity in response.text}
            for identity in ["\u96ea", "\\u96ea", '"id": "first"', '"id":"first"', "\U0001f30d", "\\ud83c\\udf0d"]],
    }}


def main() -> None:
    """Initial additive capture refuses overwrite; normal runs compare exact bytes."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--initial-capture", action="store_true")
    args = parser.parse_args()
    value = document()
    path = HERE / "wire_cases.json"
    if args.initial_capture:
        if path.exists():
            raise SystemExit("Refusing to overwrite exact wire oracle")
        path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")
    else:
        assert value == json.loads(path.read_text(encoding="utf-8")), "Request/ACL byte oracle drift"
    print("source request/merged ACL exact wire bytes verified")


if __name__ == "__main__":
    main()

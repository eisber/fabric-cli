# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual source shortcut DELETE construction and offline URL preparation."""

import argparse
import ast
import hashlib
import json
from pathlib import Path
from types import SimpleNamespace

import requests


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    data = args.source.read_bytes()
    tree = ast.parse(data.decode("utf-8"))
    publisher = next(node for node in tree.body if isinstance(node, ast.ClassDef) and node.name == "ShortcutPublisher")
    function = next(node for node in publisher.body if isinstance(node, ast.FunctionDef) and node.name == "_unpublish_shortcuts")
    namespace = {}
    exec(compile(ast.fix_missing_locations(ast.Module(body=[function], type_ignores=[])), "source:ShortcutPublisher._unpublish_shortcuts", "exec"), namespace)
    cases = []

    def invoke(**kwargs: object) -> None:
        # Preparing a Request performs no network, session, netrc lookup, auth or
        # cookie/profile discovery. Never construct a Session or call send here.
        prepared = requests.Request(method=kwargs["method"], url=kwargs["url"]).prepare()
        cases.append({"method": kwargs["method"], "raw_url": kwargs["url"], "url": prepared.url})

    workspace = "11111111-1111-1111-1111-111111111111"
    item = "22222222-2222-2222-2222-222222222222"
    instance = SimpleNamespace(
        fabric_workspace_obj=SimpleNamespace(
            base_api_url=f"https://msitapi.fabric.microsoft.com/v1/workspaces/{workspace}",
            endpoint=SimpleNamespace(invoke=invoke),
        ),
        item_obj=SimpleNamespace(guid=item),
    )
    paths = ["Tables/My Shortcut", "Tables/Équipe été", "Files/a+b", "Tables/percent%value"]
    namespace["_unpublish_shortcuts"](instance, paths)
    for case, path in zip(cases, paths, strict=True):
        case["shortcut_path"] = path
    args.output.write_text(json.dumps({
        "source_commit": "5ca096c00a9a878c646c59fe537b883eba951329",
        "source_function": "_items/_lakehouse.py:ShortcutPublisher._unpublish_shortcuts",
        "source_sha256": hashlib.sha256(data).hexdigest(),
        "requests_version": requests.__version__,
        "cases": cases,
    }, ensure_ascii=True, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

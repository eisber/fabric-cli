# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual VariableLibrary lifecycle consumption of prepared settings.

The definition capability supplies prepared bytes, not a rewritten transform
oracle. Actual publish_one/post_publish_all and activate_value_set are executed.
"""

import argparse
import ast
import hashlib
import json
import logging
from pathlib import Path
from types import SimpleNamespace


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    data = args.source.read_bytes()
    tree = ast.parse(data.decode("utf-8"))
    nodes = [node for node in tree.body if isinstance(node, (ast.ClassDef, ast.FunctionDef))]

    class Publisher:
        def __init__(self, workspace: object) -> None:
            self.fabric_workspace_obj = workspace

    namespace = {
        "json": json, "logger": logging.getLogger("offline-oracle"),
        "constants": SimpleNamespace(INDENT="->"), "ItemPublisher": Publisher,
        "ItemType": SimpleNamespace(VARIABLE_LIBRARY=SimpleNamespace(value="VariableLibrary")),
        "log_header": lambda *_: None,
    }
    exec(compile(ast.fix_missing_locations(ast.Module(body=[
        ast.ImportFrom(module="__future__", names=[ast.alias("annotations")], level=0),
        *nodes,
    ], type_ignores=[])), "source:VariableLibraryPublisher", "exec"), namespace)
    original = json.dumps({"valueSetsOrder": ["dev"]})
    prepared = json.dumps({"valueSetsOrder": ["test"]})
    cases = []
    for bulk in (False, True):
        events = []
        file = SimpleNamespace(name="settings.json", contents=original)
        item = SimpleNamespace(
            name="Example", guid="22222222-2222-2222-2222-222222222222",
            skip_publish=False, item_files=[file],
        )

        def publish(**_: object) -> None:
            file.contents = prepared
            events.append({"stage": "definition", "settings": file.contents})

        def invoke(**kwargs: object) -> None:
            events.append({"stage": "api", **kwargs})

        workspace = SimpleNamespace(
            environment="test", bulk_publish_enabled=bulk,
            base_api_url="https://api.fabric.microsoft.com/v1/workspaces/11111111-1111-1111-1111-111111111111",
            repository_items={"VariableLibrary": {"Example": item}},
            endpoint=SimpleNamespace(invoke=invoke), _publish_item=publish,
        )
        publisher = namespace["VariableLibraryPublisher"](workspace)
        if bulk:
            publish()
            publisher.post_publish_all()
        else:
            publisher.publish_one("Example", item)
        cases.append({"bulk": bulk, "events": events})
    args.output.write_text(json.dumps({
        "source_commit": "5ca096c00a9a878c646c59fe537b883eba951329",
        "source_sha256": hashlib.sha256(data).hexdigest(),
        "input_settings": original, "prepared_settings": prepared, "cases": cases,
    }, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

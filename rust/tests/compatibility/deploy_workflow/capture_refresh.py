# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual publisher refresh/selection boundaries with an offline endpoint."""

import argparse
import ast
import hashlib
import json
import logging
from pathlib import Path
import re
from types import MethodType, SimpleNamespace

from capture import SourceFile


def load(path: Path, namespace: dict, names: set[str], owner: str | None = None) -> None:
    tree = ast.parse(path.read_text(encoding="utf-8"))
    nodes = tree.body if owner is None else next(node for node in tree.body if isinstance(node, ast.ClassDef) and node.name == owner).body
    selected = [node for node in nodes if isinstance(node, ast.FunctionDef) and node.name in names]
    exec(compile(ast.fix_missing_locations(ast.Module(body=[
        ast.ImportFrom(module="__future__", names=[ast.alias("annotations")], level=0),
        *selected,
    ], type_ignores=[])), str(path), "exec"), namespace)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    paths = [
        args.source / "_items" / "_base_publisher.py",
        args.source / "_items" / "_datapipeline.py",
        args.source / "_items" / "_kqlqueryset.py",
        args.source / "fabric_workspace.py",
    ]
    namespace = {"logger": logging.getLogger("offline-oracle")}
    load(paths[0], namespace, {"publish_all", "get_items_to_publish", "_mark_skipped_items", "post_publish_all"}, "ItemPublisher")
    class Publisher:
        item_type = "DataPipeline"
    for name in ("publish_all", "get_items_to_publish", "_mark_skipped_items", "post_publish_all"):
        setattr(Publisher, name, namespace[name])
    load(paths[1], namespace, {"pre_publish_all"}, "DataPipelinePublisher")
    Publisher.pre_publish_all = namespace["pre_publish_all"]
    events = []
    excluded = SimpleNamespace(name="Excluded", skip_publish=False)
    workspace = SimpleNamespace(
        repository_items={"DataPipeline": {"Excluded": excluded}},
        items_to_include=["Absent.DataPipeline"],
        _refresh_deployed_items=lambda: events.append({"method": "GET", "path": "/items"}),
    )
    publisher = Publisher()
    publisher.fabric_workspace_obj = workspace
    publisher.publish_all()
    first = {"events": list(events), "skip_publish": excluded.skip_publish}

    events.clear()
    namespace.update({
        "json": json, "re": re,
        "constants": SimpleNamespace(
            SHELL_ONLY_PUBLISH=[], NON_MOVABLE_ITEM_TYPES=[],
            FEATURE_FLAG={"disable_workspace_folder_publish"}, INDENT="->",
        ),
        "FeatureFlag": SimpleNamespace(DISABLE_WORKSPACE_FOLDER_PUBLISH=SimpleNamespace(value="disable_workspace_folder_publish")),
        "ItemType": SimpleNamespace(
            VARIABLE_LIBRARY=SimpleNamespace(value="VariableLibrary"),
            KQL_QUERYSET=SimpleNamespace(value="KQLQueryset"),
            KQL_DATABASE=SimpleNamespace(value="KQLDatabase"),
        ),
    })
    load(paths[3], namespace, {"_publish_item"}, "FabricWorkspace")
    load(paths[2], namespace, {"func_process_file", "replace_cluster_uri"})
    load(paths[2], namespace, {"pre_publish_all", "publish_one"}, "KQLQuerysetPublisher")
    item = SimpleNamespace(
        name="Example", type="KQLQueryset", guid="", description="source fixture",
        logical_id="33333333-3333-3333-3333-333333333333", folder_id="", skip_publish=False,
        item_files=[
            SourceFile(".platform", '{"metadata":{"displayName":"Example"}}'),
            SourceFile("content.json", '{"queryset":{}}'),
        ],
    )
    def invoke(**kwargs: object) -> dict:
        events.append(kwargs)
        return {"body":{"id":"22222222-2222-2222-2222-222222222222"}}
    workspace = SimpleNamespace(
        base_api_url="https://api.fabric.microsoft.com/v1/workspaces/11111111-1111-1111-1111-111111111111",
        endpoint=SimpleNamespace(invoke=invoke), responses=None,
        repository_items={"KQLQueryset":{"Example":item}}, deployed_items={},
        _apply_publish_filters=lambda *_: False,
        _replace_logical_ids=lambda content: content,
        _replace_parameters=lambda file, _: file.contents,
        _replace_workspace_ids=lambda content, _: content,
    )
    def refresh() -> None:
        events.append({"method":"GET","path":"/items"})
        workspace.deployed_items={"KQLQueryset":{"Example":SimpleNamespace(guid="44444444-4444-4444-4444-444444444444",folder_id="")}}
    workspace._refresh_deployed_items = refresh
    workspace._publish_item = MethodType(namespace["_publish_item"],workspace)
    publisher = SimpleNamespace(fabric_workspace_obj=workspace,item_type="KQLQueryset")
    namespace["pre_publish_all"](publisher)
    namespace["publish_one"](publisher,"Example",item)
    args.output.write_text(json.dumps({
        "source_commit":"5ca096c00a9a878c646c59fe537b883eba951329",
        "source_sha256":{path.name:hashlib.sha256(path.read_bytes()).hexdigest() for path in paths},
        "excluded_pipeline":first, "initially_absent_queryset":{
            "events":events,"returned_id":item.guid,
            "fresh_cache_id":workspace.deployed_items["KQLQueryset"]["Example"].guid,
        },
    },indent=2)+"\n",encoding="utf-8")


if __name__ == "__main__":
    main()

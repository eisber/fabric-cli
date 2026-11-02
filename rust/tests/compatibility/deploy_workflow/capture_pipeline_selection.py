# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual source dependency sorting over excluded repository pipelines."""

import argparse
import ast
from collections import defaultdict, deque
import hashlib
import json
import logging
from pathlib import Path
import re
from types import MethodType, SimpleNamespace


def load(path: Path, namespace: dict, names: set[str], owner: str | None = None) -> None:
    tree = ast.parse(path.read_text(encoding="utf-8"))
    nodes = tree.body
    if owner:
        nodes = next(node for node in nodes if isinstance(node, ast.ClassDef) and node.name == owner).body
    functions = [node for node in nodes if isinstance(node, ast.FunctionDef) and node.name in names]
    exec(compile(ast.fix_missing_locations(ast.Module(body=[
        ast.ImportFrom(module="__future__", names=[ast.alias("annotations")], level=0),
        *functions,
    ], type_ignores=[])), str(path), "exec"), namespace)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    constants = SimpleNamespace(
        ITEM_TYPE_TO_FILE={"DataPipeline": "pipeline-content.json"},
        VALID_GUID_REGEX=r"[a-fA-F0-9]{8}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{4}-[a-fA-F0-9]{12}",
    )
    namespace = {
        "Path": Path, "json": json, "re": re, "defaultdict": defaultdict,
        "deque": deque, "logger": logging.getLogger("offline-oracle"),
        "constants": constants, "ParsingError": RuntimeError,
        "ItemType": SimpleNamespace(DATA_PIPELINE=SimpleNamespace(value="DataPipeline")),
    }
    files = [
        args.source / "_items" / "_manage_dependencies.py",
        args.source / "_items" / "_datapipeline.py",
        args.source / "fabric_workspace.py",
        args.source / "_items" / "_base_publisher.py",
    ]
    load(files[0], namespace, {"set_publish_order", "sort_items"})
    load(files[1], namespace, {"_reference_value", "_iter_pipeline_reference_ids", "find_referenced_datapipelines", "_get_datapipeline_publish_order"})
    load(files[2], namespace, {"_convert_id_to_name"}, "FabricWorkspace")
    load(files[3], namespace, {"get_items_to_publish"}, "ItemPublisher")
    ids = {
        "Selected": "33333333-3333-3333-3333-333333333333",
        "Left": "55555555-5555-5555-5555-555555555555",
        "Right": "66666666-6666-6666-6666-666666666666",
    }
    repository = {}
    definitions = {}
    for name in ids:
        directory = args.workspace / f"{name}.DataPipeline"
        directory.mkdir(parents=True, exist_ok=True)
        reference = {"Left": "Right", "Right": "Left"}.get(name)
        definition = {"properties": {"activities": [] if reference is None else [{
            "type": "InvokePipeline", "typeProperties": {"pipelineId": ids[reference]},
        }]}}
        (directory / "pipeline-content.json").write_text(json.dumps(definition), encoding="utf-8")
        repository[name] = SimpleNamespace(name=name, logical_id=ids[name], path=directory)
        definitions[name] = definition
    workspace = SimpleNamespace(
        repository_items={"DataPipeline": repository}, items_to_include=["Selected.DataPipeline"],
    )
    workspace._convert_id_to_name = MethodType(namespace["_convert_id_to_name"], workspace)
    publisher = SimpleNamespace(fabric_workspace_obj=workspace, item_type="DataPipeline")
    selected = list(namespace["get_items_to_publish"](publisher))
    try:
        order = namespace["_get_datapipeline_publish_order"](publisher)
    except RuntimeError:
        order = {"error": "cycle"}
    args.output.write_text(json.dumps({
        "source_commit": "5ca096c00a9a878c646c59fe537b883eba951329",
        "source_sha256": {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in files},
        "logical_ids": ids, "definitions": definitions, "items_to_include": workspace.items_to_include,
        "selected": selected, "order": order,
    }, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

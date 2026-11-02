# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual SJD folder resolution and get_export_path destination policy."""

import ast
import base64
from argparse import Namespace
from collections import deque
from copy import deepcopy
from enum import Enum, auto
import json
import os
from pathlib import Path
import sys
from typing import Any, Callable, List, Optional

from definition_export_fallback_reference import PIN, load
from definition_transfer_input_reference import source


def main() -> None:
    """Capture immutable source predicates with no filesystem, auth or service I/O."""
    output = Path(sys.argv[1])
    if output.exists():
        raise RuntimeError("Receipt must be new")
    scope = {
        "Enum": Enum, "auto": auto, "List": List, "Optional": Optional,
        "Namespace": Namespace, "deque": deque, "FabricElement": object, "json": json, "Any": Any,
        "deepcopy": deepcopy,
        "base64": base64,
    }
    path = "src/fabric_cli/core/fab_exceptions.py"
    nodes = [node for node in source(path).body if (
        isinstance(node, ast.ClassDef) and node.name == "FabricCLIError"
    ) or (
        isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id.startswith("DEFAULT_ERROR_")
            for target in node.targets
        )
    )]
    exec(compile(ast.Module(body=nodes, type_ignores=[]), path, "exec"), scope)
    constants = {}
    for node in source("src/fabric_cli/core/fab_constant.py").body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id in {
                    "ERROR_NOT_SUPPORTED", "ERROR_INVALID_PATH", "ERROR_NOT_FOUND",
                }:
                    constants[target.id] = ast.literal_eval(node.value)
    scope["fab_constant"] = Namespace(**constants)
    path = "src/fabric_cli/errors/common.py"
    common = next(node for node in source(path).body if isinstance(node, ast.ClassDef) and node.name == "CommonErrors")
    common.body = [node for node in common.body if isinstance(node, ast.FunctionDef) and node.name in {
        "no_such_file_or_directory", "only_supported_for_lakehouse_files",
        "folder_not_found_in_item", "resource_not_found",
    }]
    assert len(common.body) == 4
    exec(compile(ast.Module(body=[common], type_ignores=[]), path, "exec"), scope)
    scope["ErrorMessages"] = Namespace(Common=scope["CommonErrors"])
    path = "src/fabric_cli/core/fab_types.py"
    nodes = [node for node in source(path).body if isinstance(node, ast.ClassDef) and node.name in {
        "_BaseItemType", "ItemType", "SparkJobDefinitionFolders", "LakehouseFolders",
    }]
    assert len(nodes) == 4
    exec(compile(ast.Module(body=nodes, type_ignores=[]), path, "exec"), scope)
    mapping = next(node for node in source(path).body if isinstance(node, ast.AnnAssign)
                   and isinstance(node.target, ast.Name) and node.target.id == "ItemFoldersMap")
    pairs = [(key, value) for key, value in zip(mapping.value.keys, mapping.value.values)
             if isinstance(key, ast.Attribute) and key.attr in {"LAKEHOUSE", "SPARK_JOB_DEFINITION"}]
    mapping.value.keys, mapping.value.values = map(list, zip(*pairs))
    exec(compile(ast.Module(body=[mapping], type_ignores=[]), path, "exec"), scope)
    item_class = next(node for node in source("src/fabric_cli/core/hiearchy/fab_item.py").body
                      if isinstance(node, ast.ClassDef) and node.name == "Item")
    get_folders = next(node for node in item_class.body if isinstance(node, ast.FunctionDef) and node.name == "get_folders")
    exec(compile(ast.Module(body=[get_folders], type_ignores=[]), "pinned:Item.get_folders", "exec"), scope)

    class Item:
        """Synthetic metadata; folder vocabulary comes from actual source."""

        def __init__(self, kind: object, name: str = "Job") -> None:
            self.item_type, self.id = kind, "item"
            self.name = f"{name}.{kind}"
            self.workspace = Namespace(id="workspace")
            self.path = f"/Demo.Workspace/{self.name}"

        def check_command_support(self, command: object) -> bool:
            return True

        def get_folders(self) -> list[str]:
            return scope["get_folders"](self)

    class OneLake:
        """Resolved source item-root folder."""

        def __init__(self, folder: str, ident: str, item: Item, kind: object) -> None:
            self.item, self.local_path = item, folder
            self.path = f"/Demo.Workspace/{item.name}/{folder}"

    scope.update({
        "Item": Item, "OneLakeItem": OneLake,
        "OneLakeItemType": Namespace(FOLDER="folder"), "Command": Namespace(FS_LS="ls"),
        "process_path_part": lambda parts, context, raise_error: context[-1],
    })
    load("src/fabric_cli/core/fab_handle_context.py", {"_handle_path_in_item"}, scope)
    load("src/fabric_cli/utils/fab_storage.py", {"get_export_path", "write_to_storage"}, scope)
    load("src/fabric_cli/utils/fab_cmd_export_utils.py", {"decode_payload", "export_json_parts"}, scope)
    load("src/fabric_cli/commands/fs/export/fab_fs_export_item.py", {"export_single_item"}, scope)
    scope["fab_item_util"] = Namespace(resolve_definition_format=lambda *args: "")
    scope["item_api"] = Namespace(get_item_withdefinition=lambda *args: {
        "definition": {"parts": [{"path": "part.txt", "payload": "eA=="}]},
    })
    scope["utils_export"] = Namespace(
        decode_payload=scope["decode_payload"], export_json_parts=scope["export_json_parts"],
    )
    scope["fab_ui"] = Namespace(print_grey=lambda _: None)
    scope["fab_storage"] = Namespace(get_export_path=scope["get_export_path"])
    scope["utils_storage"] = Namespace(write_to_storage=scope["write_to_storage"])
    records = []
    kinds = scope["ItemType"]
    for kind, folder, local_exists in [
        (kinds.SPARK_JOB_DEFINITION, "Resources", False),
        (kinds.SPARK_JOB_DEFINITION, "Libs", False),
        (kinds.SPARK_JOB_DEFINITION, "Main", False),
        (kinds.SPARK_JOB_DEFINITION, "Resources", True),
        (kinds.LAKEHOUSE, "Files", False),
    ]:
        item = Item(kind)
        input_path = f"Demo.Workspace/{item.name}/{folder}"
        context_error = []
        writes = []

        def resolve(path: str) -> object:
            try:
                return scope["_handle_path_in_item"](folder, [], item, deque(), True)
            except scope["FabricCLIError"] as error:
                context_error.append({"code": error.status_code, "message": error.message})
                raise

        scope["handle_context"] = Namespace(get_command_context=resolve)
        scope["os"] = Namespace(path=Namespace(
            expanduser=lambda value: value, exists=lambda value: local_exists, join=os.path.join,
        ), makedirs=lambda *args, **kwargs: None)

        def record_write(destination: str) -> Callable[..., None]:
            def write(args: object, path: str, data: object, *extra: object) -> None:
                writes.append({"type": destination, "path": path.replace(os.sep, "/"), "payload": data})
            return write

        scope["_write_to_local"] = record_write("local")
        scope["_write_to_lakehouse"] = record_write("lakehouse")
        scope["_write_to_sjd"] = record_write("sparkJobDefinition")
        record = {"type": str(kind), "folder": folder, "input": input_path,
                  "local_exists": local_exists, "source_folders": item.get_folders()}
        try:
            selected = []

            def selected_path(path: str) -> dict:
                destination = scope["get_export_path"](path)
                selected.append(dict(destination))
                return destination

            scope["fab_storage"] = Namespace(get_export_path=selected_path)
            scope["export_single_item"](
                Item(kinds.DATA_PIPELINE, "Book"),
                Namespace(force=True, output=input_path),
            )
            record["destination"] = selected[0]
            record["error"] = None
        except scope["FabricCLIError"] as error:
            record["destination"] = None
            record["error"] = {"code": error.status_code, "message": error.message}
        record["context_errors"] = context_error
        record["remote_write_calls"] = [write for write in writes if write["type"] != "local"]
        record["local_write_calls"] = [write for write in writes if write["type"] == "local"]
        records.append(record)
    output.write_text(json.dumps({"source": PIN, "cases": records}, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

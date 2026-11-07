# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual pinned workspace selection and recursive folder order offline."""

import ast
from argparse import Namespace
from copy import deepcopy
import json
from pathlib import Path
import subprocess
import sys
from typing import Optional, Sequence, Union

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
ROOT = Path(__file__).resolve().parents[4]


def load(path: str, names: set[str], scope: dict) -> None:
    """Execute only actual source function bodies, not product module imports."""
    source = subprocess.check_output(["git", "show", f"{PIN}:{path}"], cwd=ROOT, encoding="utf-8")
    nodes = [node for node in ast.parse(source).body if isinstance(node, ast.FunctionDef) and node.name in names]
    assert {node.name for node in nodes} == names
    exec(compile(ast.Module(body=nodes, type_ignores=[]), path, "exec"), scope)


class FabricCLIError(Exception):
    """Synthetic source command-support failure."""


class Workspace:
    """Synthetic immutable workspace metadata."""


class Folder:
    """Synthetic immutable folder metadata."""

    def __init__(self, name: str, parent: object) -> None:
        self.name = name + ".Folder"
        self.parent = parent
        self.workspace = parent.workspace if isinstance(parent, Folder) else parent


class Kind:
    """Pinned type display/member pair used by the actual sorting function."""

    def __init__(self, display: str, member: str) -> None:
        self.display, self.name = display, member

    def __str__(self) -> str:
        return self.display


class Item:
    """Source support predicate with true, false and exception outcomes."""

    def __init__(self, name: str, ident: str, kind: Kind, parent: object, support: object = True) -> None:
        self.short_name, self.id, self.item_type, self.parent = name, ident, kind, parent
        self.name = name + "." + str(kind)
        self.support = support

    def check_command_support(self, command: object) -> bool:
        if self.support == "raise":
            raise FabricCLIError("source unsupported command")
        return self.support


def main() -> None:
    """Write a new immutable discovery receipt before native expectations."""
    output = Path(sys.argv[1])
    if output.exists():
        raise RuntimeError("Receipt must be new")
    workspace = Workspace()
    parent = Folder("Outer", workspace)
    target = Folder("Selected", parent)
    child = Folder("Child", target)
    pipeline, notebook = Kind("DataPipeline", "DATA_PIPELINE"), Kind("Notebook", "NOTEBOOK")
    items = [
        Item("zulu", "22222222-2222-2222-2222-222222222222", notebook, target),
        Item("alpha", "33333333-3333-3333-3333-333333333333", pipeline, child),
        Item("false", "44444444-4444-4444-4444-444444444444", pipeline, child, False),
        Item("rejected", "55555555-5555-5555-5555-555555555555", pipeline, target, "raise"),
    ]
    mem = Namespace(get_workspace_items=lambda _: items, get_workspace_folders=lambda _: [parent, target, child])
    scope = {
        "Workspace": Workspace, "Folder": Folder, "Item": Item, "FabricElement": object,
        "Union": Union, "Sequence": Sequence, "Namespace": Namespace, "deepcopy": deepcopy,
        "Optional": Optional, "FabricCLIError": FabricCLIError, "ContextItemsSupportMap": dict,
        "utils_mem_store": mem, "fab_mem_store": mem,
        "fab_constant": Namespace(FAB_FOLDER_LISTING_ENABLED="folders", FAB_OUTPUT_ITEM_SORT_CRITERIA="sort"),
        "fab_state_config": Namespace(get_config=lambda _: "bytype"),
        "Command": Namespace(FS_EXPORT="export", FS_BULKEXPORT="bulkexport"),
    }
    load("src/fabric_cli/utils/fab_cmd_fs_utils.py", {"get_ws_elements"}, scope)
    scope["utils_fs"] = Namespace(get_ws_elements=scope["get_ws_elements"])
    load("src/fabric_cli/utils/fab_cmd_bulk_export_utils.py", {"is_command_supported"}, scope)
    scope["bulk_export_utils"] = Namespace(is_command_supported=scope["is_command_supported"])
    load("src/fabric_cli/commands/fs/bulk_export/fab_fs_bulk_export_folder.py", {"_collect_context_items"}, scope)
    collected = scope["_collect_context_items"](target)
    load("src/fabric_cli/utils/fab_item_util.py", {"_get_elem_type", "sort_ws_elems_by_config"}, scope)
    scope["fab_item_util"] = Namespace(sort_ws_elems_by_config=scope["sort_ws_elems_by_config"])
    events, execution = [], []
    scope["fab_storage"] = Namespace(get_export_path=lambda _: {"type": "local", "path": "output"})
    scope["fab_ui"] = Namespace(
        print_grey=lambda text: events.append(text), print_output_format=lambda *args, **kwargs: None
    )
    scope["export_single_item"] = lambda item, args: execution.append(item.id) or True
    load("src/fabric_cli/commands/fs/export/fab_fs_export_item.py", {"export_bulk_items"}, scope)
    scope["export_bulk_items"](workspace, Namespace(all=True, force=True, output="output"))
    output.write_text(json.dumps({
        "source": PIN,
        "folder_supported": [item.id for item in collected["supported_items"]],
        "folder_rejected": [item.id for item in collected["unsupported_items"]],
        "workspace_selection": events[0].splitlines(),
        "workspace_execution": execution,
    }, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

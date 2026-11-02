# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual pinned transfer handlers using only synthetic injected state."""

import ast
import base64
import copy
import json
import os
from argparse import Namespace
from pathlib import Path
import subprocess
import sys
import tempfile
from typing import Any, Optional, Union

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
ROOT = Path(__file__).resolve().parents[5]
WORKSPACE = "11111111-1111-1111-1111-111111111111"
ITEM = "22222222-2222-2222-2222-222222222222"


def load(path: str, names: set[str], scope: dict) -> None:
    """Execute unchanged source functions, excluding imports and initialization."""
    source = subprocess.check_output(
        ["git", "show", f"{PIN}:src/fabric_cli/{path}"], cwd=ROOT, encoding="utf-8"
    )
    nodes = [
        node for node in ast.parse(source).body
        if isinstance(node, ast.FunctionDef) and node.name in names
    ]
    if {node.name for node in nodes} != names:
        raise RuntimeError(f"Missing pinned functions: {path}")
    exec(compile(ast.Module(body=nodes, type_ignores=[]), path, "exec"), scope)


class Item:
    """Synthetic context, not a service object."""

    def __init__(self, existing: bool = True) -> None:
        self._id = ITEM if existing else None
        self.workspace = Namespace(id=WORKSPACE)
        self.item_type = "DataPipeline"
        self.folder_id = None
        self.short_name = "Pipe"
        self.name = self.full_name = "Pipe.DataPipeline"
        self.path = "/Demo.Workspace/Pipe.DataPipeline"

    @property
    def id(self) -> Optional[str]:
        return self._id

    def check_command_support(self, command: str) -> bool:
        return True


class Workspace:
    id = WORKSPACE
    path = "/Demo.Workspace"
    full_name = "Demo.Workspace"


class SourceError(Exception):
    pass


def capture() -> dict:
    """Run source handlers with real isolated local reads and writes."""
    records = []
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        output = root / "output"
        output.mkdir()
        input_path = root / "input"
        input_path.mkdir()
        (input_path / "pipeline-content.json").write_bytes(b'{"name":"synthetic"}')
        (input_path / "cache.abf").write_bytes(b"synthetic cache exclusion")
        for case in ["export", "bulk-export", "import-create", "import-update"]:
            events, calls, outputs, effects = [], [], [], []
            ui = Namespace(
                print_grey=lambda text: events.append(["grey", text]),
                print_warning=lambda text: events.append(["warning", text]),
                print_output_format=lambda args=None, **kw: outputs.append(kw),
                prompt_confirm=lambda *args: True,
            )
            storage = Namespace(
                get_export_path=lambda path: {"type": "local", "path": path},
                get_import_path=lambda path: {"type": "local", "path": path},
            )
            common = {
                "os": os, "json": json, "base64": base64, "Namespace": Namespace,
                "Optional": Optional, "Union": Union, "Any": Any,
                "deepcopy": copy.deepcopy, "Item": Item, "Workspace": Workspace,
                "Folder": type("Folder", (), {}), "FabricElement": object,
                "FabricCLIError": SourceError, "ApiResponse": object,
                "fab_constant": Namespace(ERROR_NOT_SUPPORTED="NotSupported"),
                "ItemType": Namespace(NOTEBOOK="Notebook", ENVIRONMENT="Environment"),
                "fab_ui": ui, "utils_ui": ui, "utils_storage": storage,
                "fab_storage": storage,
            }
            load("utils/fab_util.py", {"process_nargs"}, common)
            load("utils/fab_storage.py", {"_validate_json", "_write_to_local"}, common)
            common["write_to_storage"] = lambda args, path, data, **kw: common[
                "_write_to_local"
            ](args, path["path"], data, export=True)
            storage.write_to_storage = common["write_to_storage"]
            common["utils_storage"] = storage
            load(
                "utils/fab_cmd_export_utils.py",
                {"decode_payload", "export_json_parts", "clean_notebook_cells"}, common,
            )
            common["utils_export"] = Namespace(**common)
            item_util = Namespace(
                resolve_definition_format=lambda *args, **kw: "",
                item_sensitivity_label_warnings=lambda args, action: ui.print_warning(
                    f"Item definition is {action} without its sensitivity label and its data"
                ) if args.force else None,
            )
            common["fab_item_util"] = item_util
            payload = {
                "definition": {"parts": [{
                    "path": "pipeline-content.json",
                    "payload": base64.b64encode(b'{"name":"synthetic"}').decode(),
                    "payloadType": "InlineBase64",
                }]}
            }

            def read_item(args: Namespace, item_uri: bool) -> dict:
                calls.append({"api": "get_item_withdefinition", "format": args.format})
                return copy.deepcopy(payload)

            def create_item(args: Namespace, payload: str) -> Namespace:
                calls.append({"api": "create_item", "body": payload})
                return Namespace(status_code=201, text=json.dumps({"id": ITEM}))

            def update_item(args: Namespace, payload: str) -> None:
                calls.append({"api": "update_item_definition", "body": payload})

            common["item_api"] = Namespace(
                get_item_withdefinition=read_item, create_item=create_item,
                update_item_definition=update_item,
            )
            args = Namespace(
                output=[str(output)], input=[str(input_path)], force=True,
                format=None, all=False, recursive=True,
            )
            item = Item(existing=case != "import-create")
            if case == "export":
                load("commands/fs/export/fab_fs_export_item.py", {"export_single_item"}, common)
                common["export_item"] = Namespace(**common)
                common["fab_util"] = Namespace(**common)
                load("commands/fs/fab_fs_export.py", {"exec_command"}, common)
                common["exec_command"](args, item)
            elif case == "bulk-export":
                args.output = str(output)
                common["ContextItemsSupportMap"] = dict
                common["Command"] = Namespace(FS_BULKEXPORT="bulk-export")
                common["fab_mem_store"] = common["utils_mem_store"] = Namespace(
                    get_workspace_items=lambda context: [item]
                )
                load(
                    "utils/fab_cmd_bulk_export_utils.py",
                    {"print_bulk_export_summary", "_count_items_by_type",
                     "create_bulk_export_payload", "is_command_supported",
                     "export_definition_parts_to_storage",
                     "_strip_parent_folders_from_definition_paths",
                     "_validate_definition_parts_paths_are_under_export_path"}, common,
                )
                common["bulk_export_utils"] = Namespace(**common)
                load(
                    "commands/fs/bulk_export/fab_fs_bulk_export_workspace.py",
                    {"bulk_export_workspace", "_filter_response_by_supported_items",
                     "_path_belongs_to_item"}, common,
                )

                def bulk(args: Namespace, body: str) -> Namespace:
                    calls.append({"api": "bulk_export_definitions", "body": body})
                    parts = copy.deepcopy(payload["definition"]["parts"])
                    parts[0]["path"] = "/Pipe.DataPipeline/pipeline-content.json"
                    return Namespace(text=json.dumps({
                        "definitionParts": parts,
                        "itemDefinitionsIndex": [{"id": ITEM, "rootPath": "/Pipe.DataPipeline"}],
                    }))

                common["item_api"].bulk_export_definitions = bulk
                load(
                    "commands/fs/fab_fs_bulk_export.py",
                    {"exec_command", "_validate_bulk_export_arguments",
                     "_confirm_export_preconditions"}, common,
                )
                common["bulk_export_folder"] = lambda *args: None
                common["fab_util"] = Namespace(**common)
                common["exec_command"](args, Workspace())
            else:
                load(
                    "utils/fab_cmd_import_utils.py",
                    {"get_payload_for_item_type", "_build_definition",
                     "_encode_file_to_base64"}, common,
                )
                common["utils_import"] = Namespace(**common)
                common["utils_mem_store"] = Namespace(
                    upsert_item_to_cache=lambda item: effects.append(item.id)
                )
                load(
                    "commands/fs/impor/fab_fs_import_item.py",
                    {"import_single_item", "_import_create_item", "_import_update_item"},
                    common,
                )
                common["import_item"] = Namespace(**common)
                common["utils"] = Namespace(**common)
                load("commands/fs/fab_fs_import.py", {"exec_command"}, common)
                common["exec_command"](args, item)
            files = {
                file.relative_to(output).as_posix(): base64.b64encode(
                    file.read_bytes()
                ).decode() for file in sorted(output.rglob("*")) if file.is_file()
            }
            record = {"case": case, "calls": calls, "outputs": outputs,
                      "effects": effects, "files": files, "events": events}
            text = json.dumps(record).replace(
                json.dumps(str(root))[1:-1], "<ROOT>"
            )
            records.append(json.loads(text))
    return {"source": PIN, "platform": sys.platform, "cases": records}


if __name__ == "__main__":
    destination = Path(sys.argv[1])
    if destination.exists():
        raise RuntimeError("Source receipts are immutable; use a new destination")
    destination.write_text(json.dumps(capture(), indent=2) + "\n", encoding="utf-8")

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture pinned bulk request/filter/path behavior before native observations."""

import copy
import json
from pathlib import Path
import sys
from types import SimpleNamespace

from definition_export_reference import PIN, load_functions


class SourceError(Exception):
    """Injected source exception identity, not a replacement algorithm."""


def main() -> None:
    """Execute actual leaves with synthetic caller-owned metadata snapshots."""
    output = Path(sys.argv[1])
    if output.exists():
        raise RuntimeError("Use a new receipt path; existing expectations are immutable")
    items = [
        SimpleNamespace(id="11111111-1111-1111-1111-111111111111", supported=True),
        SimpleNamespace(id="22222222-2222-2222-2222-222222222222", supported=False),
        SimpleNamespace(id="33333333-3333-3333-3333-333333333333", supported=True),
    ]
    scope = {
        "json": json, "ContextItemsSupportMap": dict, "Workspace": object,
        "FabricCLIError": SourceError, "Item": object,
        "utils_mem_store": SimpleNamespace(get_workspace_items=lambda _: items),
        "bulk_export_utils": SimpleNamespace(is_command_supported=lambda item: item.supported),
        "BulkExportErrors": SimpleNamespace(path_mismatch=lambda: "path mismatch"),
        "fab_constant": SimpleNamespace(ERROR_INVALID_DEFINITION_PAYLOAD="InvalidDefinitionPayload"),
    }
    load_functions("src/fabric_cli/utils/fab_cmd_bulk_export_utils.py",
        {"create_bulk_export_payload", "_strip_parent_folders_from_definition_paths"}, scope)
    load_functions("src/fabric_cli/commands/fs/bulk_export/fab_fs_bulk_export_workspace.py",
        {"_filter_response_by_supported_items", "_path_belongs_to_item"}, scope)
    response = {
        "itemDefinitionsIndex": [
            {"id": items[0].id, "rootPath": "/Folder/A.Notebook"},
            {"id": items[1].id, "rootPath": "/Unsupported.Item"},
            {"id": "44444444-4444-4444-4444-444444444444", "rootPath": "/Unknown.Item"},
        ],
        "definitionParts": [
            {"path": "/Folder/A.Notebook/a.txt", "payload": "YQ=="},
            {"path": "/Folder/A.NotebookSibling/b.txt", "payload": "Yg=="},
            {"path": "/Unsupported.Item/u.txt", "payload": "dQ=="},
            {"path": "/Unknown.Item/x.txt", "payload": "eA=="},
        ],
    }
    filtered = copy.deepcopy(response)
    supported = scope["_filter_response_by_supported_items"](filtered, object())
    paths = []
    for from_path, path in [
        ("WS.Workspace", "/Folder/A.Notebook/a.txt"),
        ("WS.Workspace/Parent.Folder", "/Parent/A.Notebook/a.txt"),
        ("WS.Workspace/Parent.Folder/Child.Folder", "/Parent/Child/A.Notebook/a.txt"),
        ("WS.Workspace/Parent.Folder/Child.Folder", "/Wrong/Child/A.Notebook/a.txt"),
    ]:
        item = {"definitionParts": [{"path": path}]}
        record = {"from_path": from_path, "input": copy.deepcopy(item)}
        try:
            scope["_strip_parent_folders_from_definition_paths"](item, from_path)
            record["output"] = item
        except SourceError as error:
            record["error"] = list(error.args)
        paths.append(record)
    receipt = {
        "source": PIN,
        "all_body": scope["create_bulk_export_payload"]([]),
        "selective_body": scope["create_bulk_export_payload"]([item.id for item in items[:2]]),
        "items": [{"id": item.id, "supported": item.supported} for item in items],
        "workspace_filter": {
            "input": response, "output": filtered,
            "supported_ids": [item.id for item in supported["supported_items"]],
            "unsupported_ids": [item.id for item in supported["unsupported_items"]],
        },
        "paths": paths,
    }
    output.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(f"Pinned bulk request/filter and {len(paths)} path cases captured to {output}")


if __name__ == "__main__":
    main()

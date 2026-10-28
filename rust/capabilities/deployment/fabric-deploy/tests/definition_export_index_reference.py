# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual bulk response-index counting with duplicate cached item IDs."""

import json
from pathlib import Path
import sys
from argparse import Namespace

from definition_export_discovery_reference import PIN, Item, Kind, Workspace, load


def main() -> None:
    """Write a new source receipt without imports of product/global state."""
    output = Path(sys.argv[1])
    if output.exists():
        raise RuntimeError("Receipt must be new")
    workspace = Workspace()
    duplicate = "22222222-2222-2222-2222-222222222222"
    other = "33333333-3333-3333-3333-333333333333"
    items = [
        Item("Book", duplicate, Kind("Notebook", "NOTEBOOK"), workspace),
        Item("Pipeline", duplicate, Kind("DataPipeline", "DATA_PIPELINE"), workspace),
        Item("Model", other, Kind("SemanticModel", "SEMANTIC_MODEL"), workspace),
    ]
    scope = {
        "Workspace": Workspace, "Item": Item, "ContextItemsSupportMap": dict,
        "utils_mem_store": Namespace(get_workspace_items=lambda _: items),
        "bulk_export_utils": Namespace(is_command_supported=lambda item: True),
    }
    load("src/fabric_cli/commands/fs/bulk_export/fab_fs_bulk_export_workspace.py",
         {"_path_belongs_to_item", "_filter_response_by_supported_items"}, scope)
    load("src/fabric_cli/utils/fab_cmd_bulk_export_utils.py", {"_count_items_by_type"}, scope)
    cases = []
    for missing in [False, True]:
        index = [{"id": other, "rootPath": "/Model.SemanticModel"}]
        if not missing:
            index.insert(0, {"id": duplicate, "rootPath": "/Pipeline.DataPipeline"})
        support = scope["_filter_response_by_supported_items"]({"itemDefinitionsIndex": index}, workspace)
        cases.append({
            "missing_duplicate": missing,
            "exported_types": scope["_count_items_by_type"](support["supported_items"]),
            "skipped_types": scope["_count_items_by_type"](support["unsupported_items"]),
        })
    output.write_text(json.dumps({"source": PIN, "cases": cases}, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

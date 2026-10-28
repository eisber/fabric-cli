# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual pinned import local-file, remote and confirmation boundaries."""

import base64
import json
import os
from argparse import Namespace
from pathlib import Path
import shutil
import sys
from types import SimpleNamespace
from typing import Any, Optional

from definition_export_reference import PIN, load_functions
from definition_import_lifecycle_reference import Response, SourceError


class OneLake:
    """Caller-resolved OneLake metadata, never a storage implementation."""

    def __init__(self) -> None:
        self.item = SimpleNamespace(item_type="Lakehouse")
        self.local_path = "Files/input"
        self.path = "/Demo.Workspace/Lake.Lakehouse/Files/input"


def capture(root: Path, name: str, existing: bool, answers: list[bool]) -> dict:
    """Run the actual source front door with only external boundaries injected."""
    events: list[dict] = []
    requests: list[dict] = []
    effects: list[dict] = []
    item = SimpleNamespace(
        id="22222222-2222-2222-2222-222222222222" if existing else None,
        workspace=SimpleNamespace(id="11111111-1111-1111-1111-111111111111"),
        item_type="DataPipeline", folder_id="44444444-4444-4444-4444-444444444444",
        short_name="Synthetic", name="Synthetic.DataPipeline",
        path="/Demo.Workspace/Chosen.Folder/Synthetic.DataPipeline",
    )

    def context(path: str) -> OneLake:
        if path == "remote":
            return OneLake()
        raise ValueError("not a Fabric path")

    def request(args: Namespace, data: Optional[str] = None) -> Response:
        requests.append({"method": args.method.upper(), "path": "/v1/" + args.uri,
                         "body": data or ""})
        return Response({"id": "33333333-3333-3333-3333-333333333333"}, 201)

    pending_answers = iter(answers)
    ui = SimpleNamespace(
        prompt_confirm=lambda message="Are you sure?": (
            events.append({"question": message}) or next(pending_answers)
        ),
        print_warning=lambda text: events.append({"warning": text}),
        print_grey=lambda text: events.append({"grey": text.replace(str(root), "<input-root>")}),
        print_output_format=lambda args, message: events.append({"output": message}),
    )
    constants = SimpleNamespace(ERROR_NOT_SUPPORTED="NotSupported",
                                ERROR_INVALID_PATH="InvalidPath",
                                ERROR_INVALID_DEFINITION_PAYLOAD="InvalidDefinitionPayload")
    scope = {
        "json": json, "os": os, "base64": base64, "Namespace": Namespace,
        "Optional": Optional, "Any": Any, "Item": object, "ApiResponse": Response,
        "FabricCLIError": SourceError, "fab_constant": constants,
        "OneLakeItem": OneLake, "ItemType": SimpleNamespace(LAKEHOUSE="Lakehouse"),
        "handle_context": SimpleNamespace(get_command_context=context),
        "fabric_api": SimpleNamespace(do_request=request),
    }
    load_functions("src/fabric_cli/utils/fab_storage.py", {"get_import_path"}, scope)
    storage = SimpleNamespace(get_import_path=scope["get_import_path"])
    load_functions("src/fabric_cli/utils/fab_cmd_import_utils.py",
                   {"get_payload_for_item_type", "_build_definition", "_encode_file_to_base64"}, scope)
    utils_import = SimpleNamespace(**scope)
    load_functions("src/fabric_cli/client/fab_api_item.py",
                   {"create_item", "update_item_definition"}, scope)
    scope.update({
        "utils_storage": storage, "utils_import": utils_import, "utils_ui": ui,
        "item_api": SimpleNamespace(create_item=scope["create_item"],
                                    update_item_definition=scope["update_item_definition"]),
        "fab_item_util": SimpleNamespace(resolve_definition_format=lambda **kwargs: ""),
        "utils_mem_store": SimpleNamespace(upsert_item_to_cache=lambda value: effects.append(
            {"id": getattr(value, "_id", value.id)}
        )),
        "ItemType": SimpleNamespace(ENVIRONMENT="Environment", LAKEHOUSE="Lakehouse"),
    })
    load_functions("src/fabric_cli/commands/fs/impor/fab_fs_import_item.py",
                   {"import_single_item", "_import_create_item", "_import_update_item"}, scope)
    args = Namespace(input="remote" if name == "remote" else str(root / "file.bin"),
                     force=not answers)
    result = {"name": name, "existing": existing, "answers": answers,
              "events": events, "requests": requests, "effects": effects}
    try:
        scope["import_single_item"](item, args)
        result["completed"] = True
    except Exception as error:
        result["error"] = {"type": type(error).__name__, "args": list(error.args)}
    return result


def main() -> None:
    """Capture only to new caller-owned paths, leaving older oracles unchanged."""
    output, root = Path(sys.argv[1]), Path(sys.argv[2])
    if output.exists() or root.exists():
        raise RuntimeError("Capture requires new, caller-owned paths")
    root.mkdir()
    try:
        (root / "file.bin").write_bytes(b"not traversed by os.walk")
        records = [
            capture(root, "local-file-create", False, []),
            capture(root, "initial-decline", True, [False]),
            capture(root, "overwrite-decline", True, [True, False]),
            capture(root, "overwrite-accept", True, [True, True]),
            capture(root, "remote", False, []),
        ]
        output.write_text(json.dumps({"source": PIN, "cases": records}, indent=2) + "\n",
                          encoding="utf-8")
        print(f"Captured {len(records)} actual pinned import front-door cases")
    finally:
        shutil.rmtree(root)


if __name__ == "__main__":
    main()

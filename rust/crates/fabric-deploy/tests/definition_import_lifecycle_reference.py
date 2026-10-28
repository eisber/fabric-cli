# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture immutable Python import orchestration, not a replacement engine."""

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


class SourceError(Exception):
    """Preserve the pinned source's message and error-code arguments."""


class Response:
    """Synthetic response at the actual Python HTTP boundary."""

    def __init__(self, value: dict, status: int = 200) -> None:
        self.value = value
        self.status_code = status
        self.text = json.dumps(value)

    def json(self) -> dict:
        """Return the synthetic HTTP boundary's supplied response document."""
        return self.value


def capture(root: Path, existing: bool, states: list[str]) -> dict:
    """Execute actual payload, API, readiness and publish functions together."""
    requests: list[dict] = []
    events: list[dict] = []
    remaining = iter(states)
    created = "33333333-3333-3333-3333-333333333333"

    def request(args: Namespace, data: Optional[str] = None) -> Response:
        requests.append({"method": args.method.upper(), "path": "/v1/" + args.uri,
                         "body": data or ""})
        if args.method == "get":
            return Response({"properties": {"publishDetails": {"state": next(remaining)}}})
        if args.uri.endswith("/items"):
            return Response({"id": created}, 201)
        return Response({}, 200)

    api_scope = {"Namespace": Namespace, "Optional": Optional, "ApiResponse": Response,
                 "fabric_api": SimpleNamespace(do_request=request)}
    api_names = {"create_item", "update_item_definition", "environment_publish",
                 "environment_get_publish_details"}
    load_functions("src/fabric_cli/client/fab_api_item.py", api_names, api_scope)
    item_api = SimpleNamespace(**{name: api_scope[name] for name in api_names})
    ui = SimpleNamespace(
        print_info=lambda text: events.append({"info": text}),
        print_progress=lambda text: events.append({"progress": text}),
    )
    scope = {
        "base64": base64, "json": json, "os": os, "Namespace": Namespace,
        "Any": Any, "Optional": Optional, "Item": object, "ApiResponse": Response,
        "ItemType": SimpleNamespace(ENVIRONMENT="Environment"),
        "item_api": item_api, "utils_ui": ui, "FabricCLIError": SourceError,
        "fab_constant": SimpleNamespace(ERROR_UNEXPECTED_ERROR="UnexpectedError",
                                      ERROR_INVALID_DEFINITION_PAYLOAD="InvalidDefinitionPayload"),
        "time": SimpleNamespace(sleep=lambda delay: events.append({"delay": delay})),
    }
    load_functions("src/fabric_cli/utils/fab_cmd_import_utils.py", {
        "get_payload_for_item_type", "_build_definition", "_encode_file_to_base64",
        "check_environment_publish_ready", "trigger_environment_publish",
        "wait_for_environment_publish", "_wait_for_environment_publish_state",
        "_handle_retry",
    }, scope)
    scope["utils_import"] = SimpleNamespace(**scope)
    load_functions("src/fabric_cli/commands/fs/impor/fab_fs_import_item.py",
                   {"_import_update_item", "_import_create_item"}, scope)
    item = SimpleNamespace(item_type="Environment", folder_id=None, short_name="Synthetic")
    args = Namespace(ws_id="11111111-1111-1111-1111-111111111111")
    payload = scope["get_payload_for_item_type"](str(root), item)
    if existing:
        args.id = "22222222-2222-2222-2222-222222222222"
    result = {"existing": existing, "states": states, "requests": requests, "events": events}
    try:
        response = scope["_import_update_item" if existing else "_import_create_item"](
            args, payload, item
        )
        result["completed"] = True
        if response is not None:
            result["created_id"] = json.loads(response.text)["id"]
    except Exception as error:
        result["error"] = {"type": type(error).__name__, "args": list(error.args)}
    return result


def main() -> None:
    """Write a new source receipt before adding Rust lifecycle expectations."""
    output = Path(sys.argv[1])
    root = Path(sys.argv[2])
    if output.exists() or root.exists():
        raise RuntimeError("Capture requires new, caller-owned paths")
    root.mkdir()
    try:
        (root / "Resources").mkdir()
        (root / "environment.yml").write_bytes(b"dependencies:\r\n  - numpy\n")
        (root / "Resources" / "b.whl").write_bytes(bytes([0, 255, 13, 10]))
        (root / "Resources" / "a.jar").write_bytes(b"jar")
        (root / "cache.abf").write_bytes(b"excluded")
        records = [
            capture(root, False, ["running", "SUCCESS"]),
            capture(root, True, ["running", "failed", "running", "success"]),
            capture(root, True, ["cancelled", "success"]),
            capture(root, False, ["failed"]),
            capture(root, False, ["cancelled"]),
            capture(root, False, ["Unknown"] * 60),
        ]
        output.write_text(json.dumps({"source": PIN, "cases": records}, indent=2) + "\n",
                          encoding="utf-8")
        print(f"Captured {len(records)} actual pinned import lifecycles")
    finally:
        shutil.rmtree(root)


if __name__ == "__main__":
    main()

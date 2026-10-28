# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independent additive source cases for the central resource release-gap tests."""

from __future__ import annotations

import argparse
import json
from types import SimpleNamespace
from typing import Any

import jmespath

from python_reference import CLIError, PIN, load
from removal_reference import reference as removal


def folder_set(force: bool, confirm: bool, query: str, status: int) -> dict[str, Any]:
    """Execute the complete frozen folder SET/GET/API/helper bodies offline."""
    events: list[Any] = []
    outputs: list[str] = []
    cache: list[str] = []
    constants = SimpleNamespace(
        SET_COMMAND_INVALID_QUERIES=["id", "type", "workspaceId", "folderId"],
        ERROR_INVALID_QUERY="InvalidQuery",
    )
    common = load("errors/common.py", {})
    ui = SimpleNamespace(
        prompt_confirm=lambda: events.append("confirm") or confirm,
        print_grey=lambda _: None,
        print_output_format=lambda _, message: outputs.append(message),
    )
    base = dict(
        json=json, FabricCLIError=CLIError, fab_constant=constants,
        CommonErrors=common.CommonErrors, utils_ui=ui,
    )
    jmes = load("utils/fab_jmespath.py", dict(jmespath=jmespath))
    helpers = load("utils/fab_cmd_set_utils.py", dict(
        base, utils_jmespath=jmes,
        fab_logger=SimpleNamespace(log_warning=lambda _: events.append("warning")),
    ))

    def request(args: argparse.Namespace, data: str | None = None) -> Any:
        events.append(dict(method=args.method, uri=args.uri, payload=json.loads(data) if data else None))
        return SimpleNamespace(
            status_code=200 if args.method == "get" else status,
            text=json.dumps(dict(id="f", displayName="Chosen", parentFolderId=None)),
        )

    api = load("client/fab_api_folders.py", dict(
        base, fabric_api=SimpleNamespace(do_request=request),
    ))
    get = load("commands/fs/get/fab_fs_get_folder.py", dict(
        base, folder_api=api,
        utils_get=SimpleNamespace(query_and_export=lambda *_: None),
    ))
    handler = load("commands/fs/set/fab_fs_set_folder.py", dict(
        base, folder_api=api, get_folder=get, utils_set=helpers,
        utils_mem_store=SimpleNamespace(upsert_folder_to_cache=lambda folder: cache.append(folder._name)),
    ))
    folder = SimpleNamespace(id="f", short_name="Chosen", name="Chosen.Folder", workspace=SimpleNamespace(id="w"))
    args = argparse.Namespace(force=force, query=query, input="After")
    error = None
    try:
        handler.exec(folder, args)
    except CLIError as failure:
        error = failure.status_code
    return dict(events=events, outputs=outputs, cache=cache, error=error)


def main() -> None:
    """Capture source evidence before running the independent Rust host cases."""
    assert jmespath.__version__ == "1.0.1"
    captures = {}
    for status in [200, 201, 202, 204]:
        result = folder_set(True, True, "displayName", status)
        assert result["events"] == [
            "warning",
            dict(method="get", uri="workspaces/w/folders/f", payload=None),
            dict(method="patch", uri="workspaces/w/folders/f", payload={"displayName": "After"}),
        ]
        assert result["outputs"] == (["Folder updated"] if status == 200 else [])
        assert result["cache"] == (["After"] if status == 200 else [])
        captures[f"folder_set_{status}"] = result
    declined = folder_set(False, False, "displayName", 200)
    assert declined["events"] == ["warning", "confirm"]
    captures["folder_declined"] = declined
    blocked = folder_set(True, True, "id", 200)
    assert blocked["error"] == "InvalidQuery" and not blocked["events"]
    captures["folder_blocked_query"] = blocked
    tenant = removal(dict(
        command="tenant", force=True, confirm=True,
        workspaces=[dict(name="a", id="a"), dict(name="B", id="b"), dict(name="A", id="A")],
        selected=["a.Workspace", "B.Workspace"], responses=[dict(status=202), dict(status=200)],
    ))
    assert tenant["choices"][0]["names"] == ["A.Workspace", "B.Workspace", "a.Workspace"]
    assert tenant["prompts"] == ["Are you sure?"]
    assert [request["url"].rsplit("/", 1)[-1] for request in tenant["requests"]] == ["b", "a"]
    captures["tenant_force"] = tenant
    batch = removal(dict(
        command="workspace", confirm=True,
        items=[dict(name="A", id="a", type="Notebook"), dict(name="B", id="b", type="Notebook")],
        selected=["B.Notebook", "A.Notebook"], responses=[dict(status=202), dict(status=403)],
    ))
    assert batch["outputs"] == ["'A.Notebook' deleted"] and batch["error"] == "Forbidden"
    assert batch["cache"] == ["A.Notebook"]
    captures["workspace_partial"] = batch
    print(json.dumps(dict(source=PIN, cases=captures, result="passed"), indent=2))


if __name__ == "__main__":
    main()

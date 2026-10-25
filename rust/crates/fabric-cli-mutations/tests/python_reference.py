# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute original pinned Python functions against the offline Rust CLI consumer.

This development-only harness uses standard-library stubs at I/O boundaries.
It never imports the installed CLI, discovers credentials, or makes network calls.
Expected requests/outputs come from git-pinned original function bodies, not Rust.
"""

from __future__ import annotations

import argparse
import ast
import base64
import enum
from functools import cache
import json
import os
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Any

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
ROOT = Path(__file__).resolve().parents[4]

@cache
def source(relative: str) -> str:
    """Read each immutable pinned blob once; never read current Python source."""
    return subprocess.check_output(
        ["git", "show", f"{PIN}:src/fabric_cli/{relative}"],
        cwd=ROOT, text=True, encoding="utf-8"
    )

def load(relative: str, namespace: dict[str, Any]) -> SimpleNamespace:
    """Load unchanged original definitions, replacing imports with explicit stubs."""
    path = f"src/fabric_cli/{relative}"
    tree = ast.parse(source(relative), filename=path)
    tree.body = [
        node for node in tree.body if not isinstance(node, (ast.Import, ast.ImportFrom))
    ]
    tree.body.insert(0, ast.ImportFrom(module="__future__", names=[ast.alias(name="annotations")], level=0))
    ast.fix_missing_locations(tree)
    namespace = dict(namespace, __file__=str(ROOT / path))
    exec(compile(tree, path, "exec"), namespace)
    return SimpleNamespace(**namespace)

def source_wait(args: argparse.Namespace) -> bool:
    """Execute the original client's wait-default assignment, not an inferred flag."""
    tree = ast.parse(source("client/fab_api_client.py"))
    function = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "do_request")
    assignment = next(node for node in function.body if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id == "wait" for target in node.targets))
    namespace = {"args": args}
    exec(compile(ast.Module(body=[assignment], type_ignores=[]), "original-wait-default", "exec"), namespace)
    return namespace["wait"]


class CLIError(Exception):
    def __init__(self, message: str, status_code: str) -> None:
        super().__init__(message)
        self.status_code = status_code


class Resource:
    def __init__(self, name: str, kind: str, identifier: str | None) -> None:
        self.short_name = name
        self.name = f"{name}.{kind}"
        self._id = identifier
        self.ws_type = kind
        self.folder_id = None
        self.item_type: Any = kind
        self.type = SimpleNamespace(value=kind)

    @property
    def id(self) -> str | None:
        return self._id


class WorkspaceResource(Resource):
    pass


class ItemResource(Resource):
    pass


class FolderResource(Resource):
    pass


def reference(case: dict[str, Any]) -> dict[str, Any]:
    """Run original handlers/client/helpers using only synthetic collaborators."""
    requests: list[dict[str, Any]] = []
    outputs: list[dict[str, Any]] = []
    constants = SimpleNamespace(
        ERROR_ALREADY_EXISTS="AlreadyExists", ERROR_INVALID_INPUT="InvalidInput",
        ERROR_NOT_SUPPORTED="NotSupported", FAB_DEFAULT_CAPACITY_ID="default",
        FAB_CAPACITY_NAME_NONE="none", INFO_EXISTS_TRUE="true", INFO_EXISTS_FALSE="false",
        ERROR_ALREADY_RUNNING="AlreadyRunning", ERROR_NOT_RUNNING="NotRunning",
        SQL_DATABASE_CREATION_MODE_NEW="New", SQL_DATABASE_CREATION_MODE_RESTORE="Restore",
        SQL_DATABASE_CREATION_MODE_RESTORE_DELETED="RestoreDeletedDatabase",
    )
    ui = SimpleNamespace(
        print_grey=lambda *_: None, print_warning=lambda *_: None, print=lambda *_: None,
        prompt_confirm=lambda *_: case.get("confirm", True),
        print_output_format=lambda args, message=None, data=None, **_: outputs.append({"message": message, "data": data}),
    )
    cache = SimpleNamespace(**{
        name: lambda *_: None for name in [
            "upsert_workspace_to_cache", "upsert_folder_to_cache", "upsert_item_to_cache",
            "invalidate_item_cache", "invalidate_folder_cache", "delete_item_from_cache", "delete_folder_from_cache",
        ]
    })
    errors = load("errors/mkdir.py", {})
    common = dict(json=json, os=os, base64=base64, Namespace=argparse.Namespace, FabricCLIError=CLIError,
                  fab_constant=constants, constant=constants, utils_ui=ui, utils_mem_store=cache,
                  ErrorMessages=SimpleNamespace(Mkdir=errors.MkdirErrors))
    types = load("core/fab_types.py", dict(common, Enum=enum.Enum, auto=enum.auto, List=list))
    kind = case["kind"]
    ws = WorkspaceResource("Team", "Workspace", case.get("id") if kind == "Workspace" else "ws")
    element = ws if kind == "Workspace" else (FolderResource if kind == "Folder" else ItemResource)("Thing", kind, case.get("id"))
    element.workspace = ws
    element.parent = ws
    if case.get("source_parent","root")!="root":
        parent=FolderResource(case["source_parent"].upper(),"Folder",case["source_parent"])
        parent.workspace,parent.parent=ws,ws
        element.parent=parent
    if kind not in ("Workspace", "Folder"):
        element.item_type = types.ItemType.from_string(kind)
    params = {key.lower(): value for key, value in case.get("params", {}).items()}
    if kind == "Workspace" and case.get("unassigned"):
        params["capacityname"] = "none"
    args = argparse.Namespace(params=params, force=case.get("force", False), hard=case.get("hard", False),
                              query=case.get("query"), input=case.get("input"), request_params={})

    def request(args: argparse.Namespace, data: str | None = None, **_: Any) -> SimpleNamespace:
        uri = args.uri
        request_params = getattr(args, "request_params", {})
        if request_params:
            from urllib.parse import urlencode
            uri += "?" + urlencode(request_params)
        requests.append({"method": args.method, "uri": uri,
                         "payload": json.loads(data) if data else None,
                         "wait": source_wait(args)})
        return SimpleNamespace(status_code=case.get("status", 200), text=json.dumps(case.get("data")))

    api = SimpleNamespace(do_request=request)
    api_utils = load("client/fab_api_utils.py", dict(common, fabric_api=api))
    ws_api = load("client/fab_api_workspace.py", dict(common, fabric_api=api, api_utils=api_utils))
    item_api = load("client/fab_api_item.py", dict(common, fabric_api=api, api_utils=api_utils))
    folder_api = load("client/fab_api_folders.py", dict(common, fabric_api=api, api_utils=api_utils))
    mirroring_api = load("client/fab_api_mirroring.py", dict(common, fabric_api=api, api_utils=api_utils))
    utils = SimpleNamespace(remove_keys_from_dict=lambda value, keys: [value.pop(key, None) for key in keys])
    mkdir_utils = load("utils/fab_cmd_mkdir_utils.py", dict(
        common, ItemType=types.ItemType, fab_logger=SimpleNamespace(log_warning=lambda *_: None)
    ))
    env = dict(common, Workspace=WorkspaceResource, Folder=FolderResource, Item=ItemResource, FabricElement=Resource,
               workspace_api=ws_api, item_api=item_api, folder_api=folder_api, utils=utils,
               format_mapping=types.format_mapping, mkdir_utils=mkdir_utils,
               fab_state_config=SimpleNamespace(get_config=lambda *_: "capacity"))
    error = None
    moved_count = None
    try:
        match case["command"]:
            case "create":
                filename = "workspace" if kind == "Workspace" else "folder" if kind == "Folder" else "item"
                load(f"commands/fs/mkdir/fab_fs_mkdir_{filename}.py", env).exec(element, args)
            case "remove":
                args.ws_id, args.id, args.folder_id, args.name = ws.id, element.id, element.id, element.name
                if kind == "Workspace":
                    ws_api.delete_workspace(args, case.get("force", False))
                elif kind == "Folder":
                    folder_api.delete_folder(args, case.get("force", False))
                else:
                    rm_utils = load("utils/fab_cmd_rm_utils.py", {})
                    load("commands/fs/rm/fab_fs_rm_item.py", dict(env, rm_utils=rm_utils)).exec(element, args, case.get("force", False))
            case "exists":
                load("commands/fs/fab_fs_exists.py", env)._check_if_element_exists(element, args)
            case "start" | "stop":
                verb = case["command"]
                load(f"commands/fs/{verb}/fab_fs_{verb}_item.py", dict(
                    env, mirroring_api=mirroring_api, ItemType=types.ItemType
                )).exec(element, args, case.get("force", False))
            case "assign" | "unassign":
                verb = case["command"]
                capacity = Resource("Chosen", "Capacity", case["capacity_id"])
                capacity.name = case["capacity_name"]
                load(f"commands/fs/{verb}/fab_fs_{verb}_capacity.py", env).exec(capacity, element, args, case.get("force", False))
            case "move_folder":
                destination=case.get("destination","root")
                if destination=="root":
                    parent=ws
                elif destination==case.get("source_parent"):
                    parent=element.parent
                else:
                    parent=FolderResource(destination.upper(),"Folder",destination)
                    parent.workspace,parent.parent=ws,ws
                moved_count=load("commands/fs/mv/fab_fs_mv_folder.py",dict(
                    env,folders_api=folder_api,cp_folder=SimpleNamespace(copy_folder=lambda *_:(_ for _ in ()).throw(AssertionError("cross-workspace out of scope")))
                )).move_folder(element,parent,args)
            case _:
                raise ValueError("unsupported reference case")
    except CLIError as failure:
        error = failure.status_code
    result={"requests": requests, "outputs": outputs, "error": error}
    if moved_count is not None:
        result["moved_count"]=moved_count
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True)
    args = parser.parse_args()
    cases: list[dict[str, Any]] = []
    for kind in ["Workspace", "Folder", "Lakehouse", "Warehouse", "Notebook", "DataPipeline", "GraphQuerySet"]:
        cases.append(dict(kind=kind, command="create", params={"run": "true"}, status=201, data={"id": "new"}))
        for identifier in [None, "id", ""]:
            cases.append(dict(kind=kind, command="exists", id=identifier))
        for force, confirm, status in [(False, False, 200), (False, True, 200), (True, True, 202), (True, True, 204)]:
            if kind != "Workspace" or force:
                cases.append(dict(kind=kind, command="remove", id="id", force=force, confirm=confirm, status=status))
    cases.append(dict(kind="Workspace", command="create", params={"run": "true"}, unassigned=True, status=201, data={"id": "new"}))
    cases.append(dict(kind="Notebook", command="remove", id="id", hard=True, force=True, status=200))
    for params in [
        {"run": "true"},
        {"creationMode": "New"},
        {"creationMode": "nEw", "backupRetentionDays": " +007 ", "collation": ""},
        {"creationMode": "New", "backupRetentionDays": "1_0"},
        {"creationMode": "New", "backupRetentionDays": "1__0"},
        {"creationMode": "New", "backupRetentionDays": "-1"},
        {"creationMode": "New", "backupRetentionDays": "18446744073709551615"},
        {"creationMode": "Restore", "restorePointInTime": "2000-01-01T00:00:00Z", "itemId": "source", "workspaceId": "source-ws"},
        {"creationMode": "Restore", "restorePointInTime": "2000-01-01T00:00:00Z", "itemId": "source", "workspaceId": "source-ws", "referenceType": "ById"},
        {"creationMode": "RestoreDeletedDatabase", "restorableDeletedDatabaseName": "Deleted", "restorePointInTime": "2000-01-01T00:00:00Z"},
        {"backupRetentionDays": "ignored-without-mode"},
        {"creationMode": "Unsupported"},
        {"creationMode": "New", "backupRetentionDays": "not-an-integer"},
        {"creationMode": "Restore"},
        {"creationMode": "RestoreDeletedDatabase", "restorableDeletedDatabaseName": ""},
    ]:
        cases.append(dict(kind="SQLDatabase", command="create", params=params, status=201, data={"id": "new"}))
    for verb, states in [("start", ["Stopped", "Initialized", "Running"]), ("stop", ["Running", "Run", "", "Stopped"])]:
        for state in states:
            for force, confirm in [(True, True), (False, False)]:
                cases.append(dict(kind="MirroredDatabase", command=verb, id="id", force=force, confirm=confirm, status=200, data={"status": state}))
    for verb in ["assign", "unassign"]:
        for force, confirm in [(True, True), (False, False), (False, True)]:
            cases.append(dict(kind="Workspace", command=verb, id="ws", capacity_id="cap", capacity_name="Chosen.Capacity",
                              force=force, confirm=confirm, status=200, data={"capacityId":"cap"}))
    cases.append(dict(kind="Workspace", command="unassign", id="ws", capacity_id="other", capacity_name="Other.Capacity",
                      force=True, status=200, data={"capacityId":"cap"}))
    for verb in ["assign", "unassign"]:
        cases.append(dict(kind="Notebook", command=verb, id="item", capacity_id="cap", capacity_name="Chosen.Capacity",
                          force=True, status=200, data={"capacityId":"cap"}))
    cases.append(dict(kind="MountedDataFactory", command="create", status=201, data={"id":"new"}, params={
        "subscriptionId":"11111111-1111-1111-1111-111111111111", "resourceGroup":"group", "factoryName":"factory",
    }))
    for source_parent,destination in [("root","root"),("root","b"),("a","root"),("a","a"),("a","b")]:
        for status in [200,201,202,204]:
            cases.append(dict(kind="Folder",command="move_folder",id="folder",source_parent=source_parent,destination=destination,status=status))
    expected = [reference(case) for case in cases]
    result = subprocess.run([args.probe], input="".join(json.dumps(case) + "\n" for case in cases),
                            text=True, encoding="utf-8", capture_output=True, check=True)
    actual = [json.loads(line) for line in result.stdout.splitlines()]
    if len(actual) != len(expected):
        raise AssertionError(f"Expected {len(expected)} outputs, received {len(actual)}")
    for case, old, new in zip(cases, expected, actual, strict=True):
        if old != new:
            raise AssertionError(json.dumps({"case": case, "python": old, "rust": new}, indent=2))
    print(json.dumps({"source": PIN, "cases": len(cases), "result": "passed"}))


if __name__ == "__main__":
    main()

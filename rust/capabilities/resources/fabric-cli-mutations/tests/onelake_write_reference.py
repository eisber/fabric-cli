# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Original OneLake write/copy handlers and API helpers vs real Rust CLI/SDK/HTTP."""

from __future__ import annotations

import argparse
import enum
import html
import json
import re
import subprocess
from collections import deque
from types import SimpleNamespace
from typing import Any
from urllib.parse import parse_qsl, quote, urlencode

from error_reference import constants
from operation_reference import functions
from python_reference import PIN, load


def reference(case: dict[str, Any]) -> dict[str, Any]:
    """Run unchanged source handlers against explicit synthetic collaborators."""
    requests, notices, prompts, outputs = [], [], [], []
    replies = deque(case["replies"])
    const = SimpleNamespace(**constants({
        "ERROR_INVALID_INPUT", "ERROR_NOT_SUPPORTED", "ERROR_INVALID_PATH",
        "WARNING_MKDIR_INVALID_ONELAKE", "ERROR_UNAUTHORIZED", "ERROR_FORBIDDEN", "ERROR_NOT_FOUND",
    }))
    errors = load("core/fab_exceptions.py", dict(json=json, html=html, re=re))
    common = load("errors/common.py", {})
    types = load("core/fab_types.py", dict(
        Enum=enum.Enum, auto=enum.auto, List=list, fab_constant=const,
        FabricCLIError=errors.FabricCLIError, ErrorMessages=SimpleNamespace(Common=common.CommonErrors),
    ))

    class Context:
        def __init__(self, name: str, identifier: str | None, parent: Any, kind: Any) -> None:
            self.name = self.short_name = name
            self.id = identifier
            self.parent = parent
            self.nested_type = kind
            self.workspace = parent.workspace
            self.item = parent.item
            self.root_folder = parent.root_folder
            self.local_path = f"{parent.local_path}/{name}"
            self.path_id = f"{parent.path_id}/{name}"
            self.path = f"{parent.path}/{name}"
            self.full_name = name
            self.type = types.FabricElementType.ONELAKE

    def context(spec: dict[str, Any]) -> Context:
        value = object.__new__(Context)
        value.name = value.short_name = spec["path"].rsplit("/", 1)[-1]
        value.id = None if spec.get("kind") == "undefined" else "resolved"
        value.nested_type = {
            "file":types.OneLakeItemType.FILE, "directory":types.OneLakeItemType.FOLDER,
            "shortcut":types.OneLakeItemType.SHORTCUT, "table":types.OneLakeItemType.TABLE,
            "undefined":types.OneLakeItemType.UNDEFINED,
        }[spec.get("kind", "file")]
        value.workspace = SimpleNamespace(id=spec.get("workspace", "workspace"))
        value.item = SimpleNamespace(id=spec.get("item", "item"), item_type=types.ItemType.from_string(spec.get("item_type", "Lakehouse")))
        value.root_folder = spec["path"].split("/")[0]
        value.local_path = spec["path"]
        value.path_id = f"/{value.workspace.id}/{value.item.id}/{spec['path']}"
        value.path = f"/Team.Workspace/Lake.Lakehouse/{spec['path']}"
        value.full_name = value.name
        value.type = types.FabricElementType.ONELAKE
        return value

    def notice(kind: str, message: str) -> None:
        notices.append(dict(kind=kind, message=message))

    def confirm(message: str = "Are you sure?") -> bool:
        prompts.append(message)
        return case.get("confirm", True)

    ui = SimpleNamespace(
        print_grey=lambda message:notice("progress", message),
        print_warning=lambda message:notice("warning", message),
        print=lambda message:notice("help", message),
        prompt_confirm=confirm,
        print_output_format=lambda args, message=None, **_:outputs.append(message),
    )

    def request(args: argparse.Namespace, data: bytes | None = None, **_: Any) -> SimpleNamespace:
        path, _, query = args.uri.partition("?")
        url = "https://onelake.dfs.fabric.microsoft.com/" + quote(path.lstrip("/"), safe="/")
        if query:
            url += "?" + urlencode(dict(parse_qsl(query, keep_blank_values=True)))
        headers = {"content-type":"application/json"}
        headers.update({key.lower():value for key, value in getattr(args, "headers", {}).items() if key.lower() != "content-length"})
        requests.append(dict(method=args.method, url=url, headers=headers, body=list(data or b"")))
        reply = replies.popleft()
        status = reply["status"]
        if status in (401, 403, 404):
            code, message = {
                401:(const.ERROR_UNAUTHORIZED, common.CommonErrors.unauthorized()),
                403:(const.ERROR_FORBIDDEN, common.CommonErrors.forbidden()),
                404:(const.ERROR_NOT_FOUND, common.CommonErrors.resource_not_found()),
            }[status]
            raise errors.FabricCLIError(message, code)
        return SimpleNamespace(status_code=status, content=bytes(reply.get("body", [])), headers=reply.get("headers", {}))

    base = dict(
        json=json, Namespace=argparse.Namespace, utils_ui=ui, fab_constant=const, constant=const,
        FabricCLIError=errors.FabricCLIError, ErrorMessages=SimpleNamespace(Common=common.CommonErrors),
    )
    api_utils = load("client/fab_api_utils.py", dict(base, fabric_api=SimpleNamespace(do_request=request)))
    api = load("client/fab_api_onelake.py", dict(base, fabric_api=SimpleNamespace(do_request=request), api_utils=api_utils))
    mkdir_utils = functions("utils/fab_cmd_mkdir_utils.py", {"show_params_desc"}, base)
    util = functions("utils/fab_util.py", {"remove_dot_suffix", "process_nargs"}, {})
    item_util = functions("utils/fab_item_util.py", {"extract_paths", "obtain_id_names_for_onelake"}, {})
    cp_utils = load("utils/fab_cmd_cp_utils.py", dict(
        base, onelake_api=api, OneLakeItem=Context, OneLakeItemType=types.OneLakeItemType,
        ItemOnelakeWritableFoldersMap=types.ItemOnelakeWritableFoldersMap,
    ))
    env = dict(
        base, onelake_api=api, OneLakeItem=Context, ItemType=types.ItemType,
        OneLakeItemType=types.OneLakeItemType, ItemOnelakeWritableFoldersMap=types.ItemOnelakeWritableFoldersMap,
        utils=SimpleNamespace(**util), mkdir_utils=SimpleNamespace(**mkdir_utils),
        cp_utils=cp_utils, item_utils=SimpleNamespace(**item_util), re=re,
    )
    target = context(case["target"])
    args = argparse.Namespace(
        path=target.path, params={"run":"true"} if case.get("params", True) else {},
        force=case.get("force", False), request_params={},
    )
    failure = None
    try:
        match case["command"]:
            case "mkdir":
                load("commands/fs/mkdir/fab_fs_mkdir_onelake.py", env).create_directory(target, args)
            case "rm":
                load("commands/fs/rm/fab_fs_rm_onelake.py", env).shortcut_file_or_folder(target, args, args.force)
            case "mv":
                load("commands/fs/mv/fab_fs_mv_onelake.py", env).move_onelake_file(context(case["source"]), target, args)
            case "cp":
                load("commands/fs/cp/fab_fs_cp_onelake.py", env).copy_onelake_2_onelake(context(case["source"]), target, args)
            case "upload":
                cp_utils.check_onelake_destination(target)
                target = cp_utils.get_onelake_file_destination(target, case.get("source_name", "source"))
                if not case["bytes"]:
                    raise errors.FabricCLIError("Invalid copy, source is empty", const.ERROR_INVALID_INPUT)
                args.to_path = target.path_id
                cp_utils.upload_file_onelake(args, bytes(case["bytes"]))
    except errors.FabricCLIError as error:
        failure = dict(code=error.status_code, message=error.message)
    if replies:
        raise AssertionError(f"Unused original replies: {len(replies)}")
    return dict(requests=requests, notices=notices, prompts=prompts, outputs=outputs, error=failure)


def main() -> None:
    """Compare all original write/copy traces with the offline Rust consumer."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True)
    args = parser.parse_args()
    cases = []
    file = dict(path="Files/data.txt", kind="file")
    for item_type, roots in [
        ("Lakehouse", ["Files", "Tables"]),
        ("Warehouse", ["Files", "Tables"]),
        ("SparkJobDefinition", ["Libs", "Main", "Snapshots"]),
        ("SemanticModel", ["Tables"]),
        ("KQLDatabase", ["Tables"]),
        ("SQLDatabase", ["Files", "Code"]),
        ("MirroredDatabase", ["Files", "Tables"]),
    ]:
        for root in roots:
            for params in [False, True]:
                allowed = root in types_writable(item_type)
                cases.append(dict(command="mkdir", target=dict(path=f"{root}/dir", kind="directory", item_type=item_type),
                                  params=params, replies=[dict(status=201)] if params and allowed else []))
    for status in [200, 201, 202, 204, 403, 404]:
        for force, confirm in [(False, False), (False, True), (True, True)]:
            cases.append(dict(command="rm", target=file, force=force, confirm=confirm,
                              replies=[] if not force and not confirm else [dict(status=status)]))
    for source, target in [
        (file, dict(path="Files/renamed.txt")),
        (dict(path="Files/a b.txt"), dict(path="Files/renamed.txt")),
        (dict(path="Tables/table"), dict(path="Files/file")),
        (dict(path="Files/file", item_type="Warehouse"), dict(path="Files/target")),
        (dict(path="files/file"), dict(path="Files/target")),
    ]:
        for force, confirm in [(False, False), (False, True), (True, True)]:
            allowed = source.get("item_type", "Lakehouse") == "Lakehouse" and source["path"].startswith("Files")
            cases.append(dict(command="mv", source=source, target=target, force=force, confirm=confirm,
                              replies=[dict(status=200)] if allowed and (force or confirm) else []))
    for target in [file, dict(path="Files", kind="directory"), dict(path="Files/link", kind="shortcut")]:
        for body in [[], [0, 255, 195, 169, 10], list(b"hello")]:
            cases.append(dict(command="upload", target=target, source_name="data.csv", bytes=body,
                              replies=[dict(status=201), dict(status=202), dict(status=200)] if body else []))
    for status in [200, 202, 204]:
        cases.append(dict(command="upload", target=file, bytes=[1, 2], replies=[dict(status=status)]))
    for kind in ["directory", "table", "shortcut"]:
        cases.append(dict(command="cp", source=dict(path="Files/source", kind=kind), target=file, replies=[]))
    for length, body in [("0", []), ("5", list(b"hello")), ("1", []), (" +1_0 ", list(b"hello"))]:
        replies = [dict(status=200, body=body, headers={"Content-Length":length,"Content-Type":"text/plain"})]
        if length != "0":
            replies += [dict(status=201), dict(status=202), dict(status=200)]
        cases.append(dict(command="cp", source=dict(path="Files/source", kind="file"), target=dict(path="Files/target", kind="file"), replies=replies))
    expected = [reference(case) for case in cases]
    run = subprocess.run([args.probe], input="".join(json.dumps(case)+"\n" for case in cases),
                         text=True, encoding="utf-8", capture_output=True, check=True)
    actual = [json.loads(line) for line in run.stdout.splitlines()]
    if len(actual) != len(expected):
        raise AssertionError("Wrong number of Rust OneLake write outputs")
    for case, old, new in zip(cases, expected, actual, strict=True):
        if old != new:
            raise AssertionError(json.dumps(dict(case=case, python=old, rust=new), indent=2))
    print(json.dumps(dict(source=PIN, onelake_write_cases=len(cases), result="passed")))


def types_writable(item_type: str) -> list[str]:
    """Construct cases only; expected validation/output runs original source."""
    return ["Libs", "Main"] if item_type == "SparkJobDefinition" else [] if item_type in ("SemanticModel", "KQLDatabase") else ["Files"]


if __name__ == "__main__":
    main()

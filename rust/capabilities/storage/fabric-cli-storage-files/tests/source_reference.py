# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Compare immutable original handlers with Rust CLI/SDK/HTTP and real local files.

Only synthetic HTTP/UI and deterministic injected I/O faults are substituted.
No installed product, credentials, network, or current Python source is imported.
"""

from __future__ import annotations

import argparse
from collections import deque
import enum
import html
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from types import SimpleNamespace
from typing import Any
from urllib.parse import parse_qsl, quote, urlencode

sys.path.insert(
    0,
    str(
        Path(__file__).resolve().parents[3]
        / "resources"
        / "fabric-cli-mutations"
        / "tests"
    ),
)
from error_reference import constants
from python_reference import PIN, load


def reference(case: dict[str, Any]) -> dict[str, Any]:
    """Execute unchanged pinned cp dispatch, file handlers, LocalPath and helpers."""
    requests: list[dict[str, Any]] = []
    notices: list[dict[str, str]] = []
    outputs: list[str] = []
    replies = deque(case["replies"])
    const = SimpleNamespace(**constants({
        "ERROR_INVALID_INPUT", "ERROR_NOT_SUPPORTED", "ERROR_INVALID_PATH",
        "WARNING_INVALID_PATHS", "ERROR_UNAUTHORIZED", "ERROR_FORBIDDEN", "ERROR_NOT_FOUND",
    }))
    errors = load("core/fab_exceptions.py", dict(json=json, html=html, re=re))
    common = load("errors/common.py", {})
    types = load("core/fab_types.py", dict(
        Enum=enum.Enum, auto=enum.auto, List=list, fab_constant=const,
        FabricCLIError=errors.FabricCLIError,
        ErrorMessages=SimpleNamespace(Common=common.CommonErrors),
    ))
    # Only the base's constructor is needed by the original LocalPath predicates.
    class Base:
        def __init__(self, name: str, identifier: str, kind: Any) -> None:
            self._name, self._id, self.type = name, identifier, kind

    local_class = load("core/hiearchy/fab_local_path.py", dict(
        os=os, FabricElement=Base, FabricElementType=types.FabricElementType,
    )).LocalPath

    class Remote:
        def __init__(self, name: str, identifier: str, parent: Any, kind: Any) -> None:
            self.id, self.nested_type = identifier, kind
            self.name, self.item = name, parent.item
            self.root_folder = parent.root_folder
            self.path_id = f"{parent.path_id}/{name}"
            self.path = f"{parent.path}/{name}"
            self.type = types.FabricElementType.ONELAKE

    remote = object.__new__(Remote)
    remote.nested_type = {
        "file": types.OneLakeItemType.FILE, "directory": types.OneLakeItemType.FOLDER,
        "shortcut": types.OneLakeItemType.SHORTCUT, "table": types.OneLakeItemType.TABLE,
        "undefined": types.OneLakeItemType.UNDEFINED,
    }[case.get("kind", "file")]
    remote.id = None if case.get("kind") == "undefined" else "resolved"
    remote.item = SimpleNamespace(item_type=types.ItemType.from_string(case.get("item_type", "Lakehouse")))
    remote.name = case["source"].rsplit("/", 1)[-1]
    remote.root_folder = case["source"].split("/")[0]
    remote.path_id = f"/workspace/item/{case['source']}"
    remote.path = f"/Team.Workspace/Lake.Lakehouse/{case['source']}"
    remote.type = types.FabricElementType.ONELAKE
    target = local_class(case["target"])
    upload = case.get("direction") == "upload"
    selected = os.path.join(case["target"], remote.name) if not upload and target.is_directory() and case.get("kind", "file") == "file" else case["target"]

    ui = SimpleNamespace(
        print_grey=lambda text: notices.append(dict(kind="progress", message=text)),
        print_output_format=lambda args, message=None, **_: outputs.append(message),
    )

    def request(args: argparse.Namespace, data: bytes | None = None, **_: Any) -> Any:
        path, _, query = args.uri.partition("?")
        url = "https://onelake.dfs.fabric.microsoft.com/" + quote(path.lstrip("/"), safe="/")
        if query:
            url += "?" + urlencode(dict(parse_qsl(query, keep_blank_values=True)))
        headers = {"content-type": "application/json"}
        headers.update({k.lower(): v for k, v in getattr(args, "headers", {}).items() if k.lower() != "content-length"})
        requests.append(dict(method=args.method, url=url, headers=headers, body=list(data or b"")))
        reply = replies.popleft()
        if reply["status"] in (401, 403, 404):
            code, message = {
                401: (const.ERROR_UNAUTHORIZED, common.CommonErrors.unauthorized()),
                403: (const.ERROR_FORBIDDEN, common.CommonErrors.forbidden()),
                404: (const.ERROR_NOT_FOUND, common.CommonErrors.resource_not_found()),
            }[reply["status"]]
            raise errors.FabricCLIError(message, code)
        return SimpleNamespace(status_code=reply["status"], headers={}, content=bytes(reply["body"]))

    class FaultFile:
        def __init__(self, path: str, mode: str) -> None:
            self.file = open(path, mode)

        def __enter__(self) -> FaultFile:
            return self

        def __exit__(self, *args: Any) -> None:
            self.file.close()

        def write(self, content: bytes) -> int:
            if "write_after" in case:
                self.file.write(content[:case["write_after"]])
                raise OSError("injected write failure")
            return self.file.write(content)

        def read(self) -> bytes:
            if case.get("read_failure"):
                raise OSError("injected read failure")
            return self.file.read()

    base = dict(
        Namespace=argparse.Namespace, fab_constant=const, FabricCLIError=errors.FabricCLIError,
        ErrorMessages=SimpleNamespace(Common=common.CommonErrors), utils_ui=ui,
    )
    api = load("client/fab_api_onelake.py", dict(base, fabric_api=SimpleNamespace(do_request=request)))
    helpers = load("utils/fab_cmd_cp_utils.py", dict(
        base, onelake_api=api, OneLakeItem=Remote, OneLakeItemType=types.OneLakeItemType,
        ItemOnelakeWritableFoldersMap=types.ItemOnelakeWritableFoldersMap,
    ))
    handler = load("commands/fs/cp/fab_fs_cp_onelake.py", dict(
        base, os=os, open=FaultFile, LocalPath=local_class, OneLakeItem=Remote,
        OneLakeItemType=types.OneLakeItemType, cp_utils=helpers,
    ))
    dispatch = load("commands/fs/fab_fs_cp.py", dict(
        base, cp_onelake=handler, OneLakeItem=Remote, LocalPath=local_class,
        Workspace=type("Workspace", (), {}), Folder=type("Folder", (), {}),
        Item=type("Item", (), {}),
    ))
    args = argparse.Namespace(force=case["force"], recursive=case["recursive"], block_on_path_collision=case["collision"], fab_mode=case["mode"])
    failure = None
    try:
        if upload:
            dispatch.exec_command(args, target, remote)
        else:
            dispatch.exec_command(args, remote, target)
    except errors.FabricCLIError as error:
        failure = dict(code=error.status_code, message=error.message)
    if replies:
        raise AssertionError("unused original responses")
    return dict(requests=requests, notices=notices, prompts=[], outputs=outputs, error=failure, local_path=selected)


def main() -> None:
    """Run actual temporary OS fixtures; the caller chooses their private parent."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True)
    parser.add_argument("--fixtures", required=True)
    parser.add_argument("--only-empty-path", action="store_true")
    args = parser.parse_args()
    count = 0
    upload_count = 0
    safety_count = 0
    with tempfile.TemporaryDirectory(prefix="source-copy-", dir=args.fixtures) as root:
        cases = []
        for kind in ["file", "directory", "shortcut", "table", "undefined"]:
            for force, recursive, collision, mode in [
                (False, False, False, "interactive"),
                (True, True, True, "commandline"),
            ]:
                for body in [[], [0, 255, 195, 169, 13, 10, 123, 125]]:
                    cases.append(dict(kind=kind, force=force, recursive=recursive, collision=collision, mode=mode,
                                      body=body, target_kind="directory"))
        for target_kind in ["new", "existing", "directory", "missing_parent", "child_directory"]:
            for body in [[], [0, 255, 195, 169, 10]]:
                cases.append(dict(target_kind=target_kind, body=body))
        for status in [401, 403, 404]:
            cases.append(dict(target_kind="existing", body=[], status=status))
        cases.append(dict(target_kind="new", body=list(b"created-response"), status=201))
        for after in [0, 1, 3]:
            cases.append(dict(target_kind="existing", body=list(b"complete"), write_after=after))
        cases.append(dict(target_kind="directory", body=list(b"hello"), source="Files/a b \u00e9.bin"))
        for status in [202, 204]:
            cases.append(dict(target_kind="existing", body=list(b"nonempty"), status=status, strict_read_status_exception=True))
        if os.name == "nt":
            cases.append(dict(target_kind="readonly", body=list(b"new"), native_write_denied=True))
        for target_spec in [
            dict(source="Files/out.txt", kind="file"),
            dict(source="Files", kind="directory"),
            dict(source="Files/link", kind="shortcut"),
            dict(source="Files/new.bin", kind="undefined"),
            dict(source="Files/table", kind="table"),
            dict(source="Tables/out.txt", kind="file"),
            dict(source="Main", kind="directory", item_type="SparkJobDefinition"),
            dict(source="Files/out", kind="file", item_type="SemanticModel"),
        ]:
            for local_kind in ["local_file", "local_directory", "local_missing"]:
                for force, recursive, collision, mode in [
                    (False, False, False, "interactive"),
                    (True, True, True, "commandline"),
                ]:
                    allowed = local_kind == "local_file" and target_spec["kind"] != "table" and not target_spec["source"].startswith("Tables/") and target_spec.get("item_type") != "SemanticModel"
                    cases.append(dict(target_spec, direction="upload", target_kind=local_kind,
                                      body=[0, 255, 195, 169, 13, 10], force=force, recursive=recursive,
                                      collision=collision, mode=mode,
                                      replies=[dict(status=201, body=[]), dict(status=202, body=[]), dict(status=200, body=[])] if allowed else []))
        for suffix in [".json", ".csv", ".txt", ".JSON", ".TXT", ".bin"]:
            cases.append(dict(direction="upload", target_kind="local_file", source=f"Files/destination{suffix}",
                              body=list(b"not-json\r\n"), replies=[dict(status=201, body=[]), dict(status=202, body=[]), dict(status=200, body=[])]))
        for create in [200, 202, 204]:
            cases.append(dict(direction="upload", target_kind="local_file", source="Files/existing",
                              body=list(b"content"), replies=[dict(status=create, body=[])]))
        for append in [200, 202, 204]:
            for flush in [200, 202, 204]:
                cases.append(dict(direction="upload", target_kind="local_file", source="Files/existing",
                                  body=list(b"content"), replies=[dict(status=201, body=[]), dict(status=append, body=[]), dict(status=flush, body=[])]))
        for stage in range(3):
            for status in [401, 403, 404]:
                replies = [dict(status=201 if index == 0 else 202, body=[]) for index in range(stage)]
                cases.append(dict(direction="upload", target_kind="local_file", source="Files/existing",
                                  body=list(b"content"), replies=replies + [dict(status=status, body=[])]))
        for body, read_failure in [([], False), (list(b"content"), True)]:
            cases.append(dict(direction="upload", target_kind="local_file", source="Files/out",
                              body=body, read_failure=read_failure, replies=[]))
        cases.append(dict(direction="upload", target_kind="local_file", source="Files",
                          kind="directory", local_name="a b \u00e9.csv", body=list(b"a,b\n"),
                          replies=[dict(status=201, body=[]), dict(status=202, body=[]), dict(status=200, body=[])]))
        for body in [[], list(b"must not create a file under the local root")]:
            cases.append(dict(target_kind="empty_path", body=body))
        cases.append(dict(direction="upload", target_kind="empty_path", source="Files/out",
                          body=list(b"invalid source"), replies=[]))
        if args.only_empty_path:
            cases = [case for case in cases if case["target_kind"] == "empty_path"]
        with subprocess.Popen([args.probe], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8") as process:
            assert process.stdin is not None and process.stdout is not None
            for index, spec in enumerate(cases):
                fixture = Path(root) / str(index)
                fixture.mkdir()
                target_kind = spec["target_kind"]
                target = fixture if target_kind in ("directory", "child_directory") else fixture / "target"
                if spec.get("direction") == "upload":
                    target = fixture / spec.get("local_name", "source.txt")
                    if target_kind == "local_file":
                        target.write_bytes(bytes(spec["body"]))
                    elif target_kind == "local_directory":
                        target.mkdir()
                        (target / "untraversed").write_bytes(b"do not copy")
                if target_kind == "missing_parent":
                    target = fixture / "missing" / "target"
                if target_kind in ("existing", "readonly"):
                    target.write_bytes(b"old-longer-file")
                if target_kind == "readonly":
                    target.chmod(0o444)
                if target_kind == "child_directory":
                    (fixture / "source.txt").mkdir()
                (fixture / "unrelated").write_bytes(b"keep")
                kind = spec.get("kind", "file")
                case = dict(spec, root=str(fixture), target="" if target_kind == "empty_path" else str(target),
                            source=spec.get("source", "Files/source.txt"),
                            force=spec.get("force", False), recursive=spec.get("recursive", False),
                            collision=spec.get("collision", False), mode=spec.get("mode", "interactive"),
                            replies=spec.get("replies", [dict(status=spec.get("status", 200), body=spec["body"])] if kind == "file" else []))
                expected = reference(case)
                selected = Path(expected["local_path"])
                expected_bytes = selected.read_bytes() if selected.is_file() else None
                # Reset just the named fixture file; preserve directories/unrelated files.
                if selected.is_file() and target_kind != "readonly" and spec.get("direction") != "upload":
                    selected.unlink()
                if target_kind == "existing":
                    target.write_bytes(b"old-longer-file")
                process.stdin.write(json.dumps(case) + "\n")
                process.stdin.flush()
                line = process.stdout.readline()
                if not line:
                    raise AssertionError(process.stderr.read() if process.stderr else "probe terminated")
                actual = json.loads(line)
                # OS-specific native error prose is not portable between Python and Rust.
                if ((target_kind in ("missing_parent", "child_directory", "empty_path") and spec.get("direction") != "upload") or spec.get("native_write_denied")) and spec["body"]:
                    for result in [expected, actual]:
                        assert result["error"]["code"] == "InvalidPath"
                        assert result["error"]["message"].startswith("Error writing file: ")
                        result["error"]["message"] = "Error writing file: <native OS diagnostic>"
                actual_bytes = selected.read_bytes() if selected.is_file() else None
                if spec.get("strict_read_status_exception"):
                    # Separate existing SDK safety behavior; never relabel it parity.
                    assert expected["error"] is None and expected["outputs"] == ["Done"]
                    assert expected_bytes == b"nonempty"
                    assert actual["error"]["code"] == "InvalidResponse" and actual["outputs"] == []
                    assert actual_bytes == b"old-longer-file"
                    assert actual["requests"] == expected["requests"]
                    assert actual["notices"] == expected["notices"]
                    safety_count += 1
                else:
                    if actual != expected:
                        raise AssertionError(json.dumps(dict(case=case, python=expected, rust=actual), indent=2))
                    assert actual_bytes == expected_bytes, (index, actual_bytes, expected_bytes)
                    count += 1
                    if spec.get("direction") == "upload":
                        upload_count += 1
                assert (fixture / "unrelated").read_bytes() == b"keep"
                assert not (fixture / "missing").exists()
                if target_kind == "local_directory":
                    assert (target / "untraversed").read_bytes() == b"do not copy"
                if target_kind == "empty_path":
                    assert list(fixture.iterdir()) == [fixture / "unrelated"]
                if target_kind == "readonly":
                    target.chmod(0o666)
            process.stdin.close()
            if process.wait(timeout=30) != 0:
                raise AssertionError(process.stderr.read() if process.stderr else "probe failed")
    print(json.dumps(dict(source=PIN, local_copy_cases=count, physical_upload_cases=upload_count, explicit_sdk_safety_cases=safety_count, result="passed")))


if __name__ == "__main__":
    main()

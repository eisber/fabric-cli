# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Compare original OneLake list/exists/helper/API bodies to a real Rust HTTP consumer."""

from __future__ import annotations

import argparse
import json
import subprocess
from collections import deque
from types import SimpleNamespace
from urllib.parse import parse_qsl, urlencode

from python_reference import CLIError, PIN, load
from operation_reference import functions


def reference(case: dict) -> dict:
    """Use original functions with synthetic HTTP, UI and already-resolved context."""
    requests = []
    result = dict(data=None, message=None, show_headers=False, error=None)
    replies = deque(case["replies"])
    constants = SimpleNamespace(INFO_EXISTS_TRUE="true", INFO_EXISTS_FALSE="false", ERROR_NOT_FOUND="NotFound")

    def request(args: argparse.Namespace, **_) -> SimpleNamespace:
        path, _, query = args.uri.partition("?")
        uri = "https://onelake.dfs.fabric.microsoft.com/" + path.lstrip("/")
        if query:
            uri += "?" + urlencode(dict(parse_qsl(query, keep_blank_values=True)))
        requests.append(dict(method=args.method, url=uri))
        reply = replies.popleft()
        if reply["status"] not in (200, 201):
            status = reply["status"]
            code = {401: "Unauthorized", 403: "Forbidden", 404: "NotFound"}.get(status, "UnknownError")
            headers = {key.lower(): value for key, value in reply.get("headers", {}).items()}
            if status in (401, 403):
                code = headers.get("x-ms-public-api-error-code") or headers.get("x-ms-error-code") or code
            raise CLIError("synthetic OneLake failure", code)
        return SimpleNamespace(status_code=reply["status"], text=json.dumps(reply["data"]))

    def output(args, message=None, data=None, show_headers=False, **_):
        result.update(data=data, message=message, show_headers=show_headers)

    ui = SimpleNamespace(print_output_format=output)
    util = functions("utils/fab_util.py", {"remove_dot_suffix"}, {})
    api = load("client/fab_api_onelake.py", dict(fabric_api=SimpleNamespace(do_request=request)))
    ls_helpers = functions("utils/fab_cmd_ls_utils.py", {"update_entry_name_and_type", "format_and_print_output"}, dict(
        utils_ui=ui, utils_jmespath=SimpleNamespace(search=lambda *_: (_ for _ in ()).throw(AssertionError("query requires independent evaluator"))),
    ))
    context = SimpleNamespace(local_path=case["path"], workspace=SimpleNamespace(id="workspace"), item=SimpleNamespace(id="item"))
    args = argparse.Namespace(long=case.get("long", False), query=None)
    try:
        if case["command"] == "ls":
            load("commands/fs/ls/fab_fs_ls_onelake.py", dict(json=json, onelake_api=api, utils=SimpleNamespace(**util), utils_ls=SimpleNamespace(**ls_helpers))).list_onelake(context, args)
        else:
            load("commands/fs/fab_fs_exists.py", dict(onelake_api=api, utils=SimpleNamespace(**util), utils_ui=ui,
                 fab_constant=constants, FabricCLIError=CLIError))._check_if_onelake_file_or_directory_exists(context, args)
    except CLIError as error:
        result["error"] = error.status_code
    if replies:
        raise AssertionError("Unexpected unconsumed original-source replies")
    return dict(requests=requests, **result)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True)
    args = parser.parse_args()
    cases = []
    for path in ["Files", "Files/a.Shortcut/child", "Files/a.shortcut/child", "Files/a.SHORTCUT", "Tables/schema.table"]:
        local = path.replace(".Shortcut", "").replace(".shortcut", "")
        for rows in [
            [],
            [{"name": f"item/{local}/plain.txt"}],
            [{"name": f"item/{local}/folder", "isDirectory": False}, {"name": f"item/{local}/folder", "isDirectory": "false"}],
            [{"name": f"item/{local}/linked", "isShortcut": True, "isDirectory": True}],
            [{"name": f"item/{local}/linked", "isShortcut": "false", "isDirectory": False}],
            [{"name": f"item/{local}/x/{local}/tail", "permissions": "rwx", "lastModified": "synthetic", "future": {"keep": 1}}],
            [{"name": f"item/{local}/\u65e5.txt"}, {"name": f"item/{local}/\u65e5.txt"}],
        ]:
            for long in [False, True]:
                cases.append(dict(command="ls", path=path, long=long, replies=[dict(status=200, data={"paths": rows})]))
    for path in ["Files", "Files/link.Shortcut", "Tables/schema.table"]:
        for status in [200, 401, 403, 404]:
            cases.append(dict(command="exists", path=path, replies=[dict(status=status, data={"paths":[]})]))
    for status in [401, 403]:
        for headers in [
            {"x-ms-public-api-error-code":"PublicCode", "x-ms-error-code":"SecondaryCode"},
            {"x-ms-public-api-error-code":"", "x-ms-error-code":"SecondaryCode"},
        ]:
            cases.append(dict(command="exists", path="Files", replies=[dict(status=status, data={}, headers=headers)]))
    expected = [reference(case) for case in cases]
    run = subprocess.run([args.probe], input="".join(json.dumps(case) + "\n" for case in cases),
                         text=True, encoding="utf-8", capture_output=True, check=True)
    actual = [json.loads(line) for line in run.stdout.splitlines()]
    if len(actual) != len(expected):
        raise AssertionError("Wrong number of Rust OneLake probe outputs")
    for case, old, new in zip(cases, expected, actual, strict=True):
        if old != new:
            raise AssertionError(json.dumps(dict(case=case, python=old, rust=new), indent=2))
    print(json.dumps(dict(source=PIN, onelake_command_cases=len(cases), result="passed")))


if __name__ == "__main__":
    main()

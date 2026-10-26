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
import jmespath

from python_reference import CLIError, PIN, load
from operation_reference import functions
from operation_reference import Headers


def reference(case: dict) -> dict:
    """Use original functions with synthetic HTTP, UI and already-resolved context."""
    if case["command"] in ("get","roots") or len(case["replies"])>1:
        return get_reference(case)
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


def get_reference(case):
    requests=[]
    replies=deque(case["replies"])
    result=dict(data=None,message=None,show_headers=False,error=None)
    errors=load("core/fab_exceptions.py",dict(json=json))
    common=load("errors/common.py",{})
    const=SimpleNamespace(DEBUG=False)
    types=load("client/fab_api_types.py",dict(json=json,FabricCLIError=errors.FabricCLIError,ErrorMessages=SimpleNamespace(Common=common.CommonErrors),fab_constant=const))
    def request(args,continuation_token=None):
        path,_,query=args.uri.partition("?")
        params=dict(parse_qsl(query,keep_blank_values=True))
        if continuation_token:params["continuationToken"]=continuation_token
        url="https://onelake.dfs.fabric.microsoft.com/"+path.lstrip("/")
        if params:url+="?"+urlencode(params)
        requests.append(dict(method=args.method,url=url))
        reply=replies.popleft()
        body=json.dumps(reply["data"])
        response=types.ApiResponse(reply["status"],body,body.encode(),Headers(reply.get("headers",{})))
        return pagination["_handle_successful_response"](args,response)
    pagination=functions("client/fab_api_client.py",{"_handle_successful_response"},dict(
        json=json,fab_constant=const,do_request=request
    ))
    api=load("client/fab_api_onelake.py",dict(fabric_api=SimpleNamespace(do_request=request)))
    original_query=load("utils/fab_jmespath.py",dict(json=json,jmespath=jmespath,FabricCLIError=errors.FabricCLIError,ErrorMessages=SimpleNamespace(Common=common.CommonErrors),fab_constant=SimpleNamespace(ERROR_INVALID_INPUT="InvalidInput")))
    def search(data,query,deep=False):
        projected=original_query.search(data,query,deep)
        if case["command"]=="get":result["projected"]=projected
        return projected
    utils_get=load("utils/fab_cmd_get_utils.py",dict(
        json=json,utils_jmespath=SimpleNamespace(search=search),
        utils_ui=SimpleNamespace(print_output_format=lambda args,data=None,**_:result.update(data=data)),
    ))
    context=SimpleNamespace(local_path=case["path"],workspace=SimpleNamespace(id="workspace"),item=SimpleNamespace(id="item"),path_id="/workspace/item")
    args=argparse.Namespace(query=case.get("query"),output=None,deep_traversal=case.get("deep",False))
    if case["command"]=="get":
        metadata=load("commands/fs/get/fab_fs_get_onelake.py",dict(json=json,os=__import__("os"),onelake_api=api,utils_get=utils_get)).onelake_resource(context,args)
        result["metadata"]=metadata
    else:
        args.long=case.get("long",False)
        util=functions("utils/fab_util.py",{"remove_dot_suffix","process_nargs"},{})
        def output(args,data=None,show_headers=False,**_):result.update(data=data,show_headers=show_headers)
        helpers=functions("utils/fab_cmd_ls_utils.py",{"update_entry_name_and_type","format_and_print_output"},dict(
            utils_ui=SimpleNamespace(print_output_format=output),utils_jmespath=SimpleNamespace(search=search),fab_util=SimpleNamespace(**util)
        ))
        source_ls=load("commands/fs/ls/fab_fs_ls_onelake.py",dict(json=json,onelake_api=api,utils=SimpleNamespace(**util),utils_ls=SimpleNamespace(**helpers)))
        if case["command"]=="roots":source_ls.list_item_folders(context,args)
        else:source_ls.list_onelake(context,args)
    if replies:raise AssertionError("Unused source GET pages")
    return dict(requests=requests,**result)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True)
    args = parser.parse_args()
    if jmespath.__version__!="1.0.1":raise AssertionError("Original GET query evaluator must be jmespath 1.0.1")
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
    first={"paths":[{"name":"item/Files/a","unknown":True}],"annotations":[0],"scalar":"first","ContinuationToken":"upper removed","meta":{"a":{"b":{"c":{"d":1}}}}}
    for query in [None,".","paths","length(paths)","missing","meta"]:
        for deep in [False,True]:
            cases.append(dict(command="get",path="Files",query=query,deep=deep,replies=[dict(status=200,data=first)]))
    for statuses in [[200,200],[200,201],[201,200],[201,201],[200,201,200]]:
        replies=[]
        for index,status in enumerate(statuses):
            data=dict(first,paths=[{"name":f"item/Files/{index}"}],annotations=[index],scalar=f"page{index}")
            if index+1<len(statuses):
                data["continuationToken"]=f"token{index}"
                data["continuationUri"]="https://outside.invalid/not-followed"
            replies.append(dict(status=status,data=data))
        cases.append(dict(command="get",path="Files/kept.Shortcut",query=".",replies=replies))
        cases.append(dict(command="ls",path="Files",long=True,replies=replies))
    cases.append(dict(command="get",path="Files",query="paths[?unknown == `false`]",replies=[dict(status=200,data=first)]))
    root_rows=[
        {"name":"item/Files","permissions":"rwx","lastModified":"synthetic","future":True},
        {"name":"item/Tables/ignored-third","isDirectory":True},
        {"name":"/item/Files"},
        {"name":"PlainRoot"},
        {"name":"item/"},
    ]
    for long in [False,True]:
        for query in [None,".","[].name","length(@)"]:
            cases.append(dict(command="roots",path="",long=long,query=query,replies=[dict(status=200,data={"paths":root_rows})]))
    cases.append(dict(command="roots",path="",long=True,replies=[
        dict(status=200,data={"paths":[root_rows[0]],"continuationToken":"root-next"}),
        dict(status=200,data={"paths":[root_rows[1]]}),
    ]))
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

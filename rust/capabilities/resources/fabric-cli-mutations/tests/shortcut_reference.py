# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pinned original ln/shortcut-delete/API/input/name helpers vs real Rust consumer."""

from __future__ import annotations

import argparse
import enum
import html
import json
import os
import re
import subprocess
from collections import deque
from types import SimpleNamespace
from typing import Any
from urllib.parse import parse_qsl, quote, urlencode
from xml.sax.saxutils import escape
import jmespath

from error_reference import constants
from python_reference import PIN, load
from operation_reference import functions


def reference(case: dict[str, Any]) -> dict[str, Any]:
    """Execute pinned shortcut handlers with synthetic context, I/O and UI."""
    replies = deque(case["replies"])
    requests, notices, prompts, outputs = [], [], [], []
    const = SimpleNamespace(**constants({
        "ERROR_INVALID_INPUT", "ERROR_INVALID_PATH", "ERROR_INVALID_JSON",
        "WARNING_INVALID_JSON_FORMAT", "WARNING_ONLY_SUPPORTED_WITHIN_LAKEHOUSE",
        "WARNING_ONLY_SUPPORTED_WITHIN_FILES_AND_TABLES",
        "SET_COMMAND_INVALID_QUERIES",
        "ERROR_INVALID_QUERY",
    }))
    errors = load("core/fab_exceptions.py", dict(json=json,html=html,re=re))
    common = load("errors/common.py", {})
    types = load("core/fab_types.py", dict(Enum=enum.Enum,auto=enum.auto,List=list,fab_constant=const,FabricCLIError=errors.FabricCLIError,ErrorMessages=SimpleNamespace(Common=common.CommonErrors)))

    class Context:
        pass

    context = Context()
    context.local_path = case["path"]
    context.path = case.get("display_path", case["path"])
    context.workspace = SimpleNamespace(id="workspace")
    context.item = SimpleNamespace(id="item",item_type=types.ItemType.from_string(case.get("item_type","Lakehouse")))
    context.nested_type=types.OneLakeItemType.SHORTCUT
    context.full_name="Chosen.Shortcut"
    target = Context()
    target.workspace = SimpleNamespace(id="target-workspace")
    target.item = SimpleNamespace(id="target-item")
    target.local_path = case.get("target")
    target_json = case.get("input")
    args = argparse.Namespace(
        force=case.get("force",False),target="selected-target" if case.get("target") is not None else None,
        input=target_json,type=case.get("type","oneLake"),request_params={},
    )

    def confirm(message: str = "Are you sure?") -> bool:
        prompts.append(message)
        return case.get("confirm",True)

    ui = SimpleNamespace(
        print_grey=lambda message:notices.append(dict(kind="progress",message=message)),
        print_warning=lambda message:notices.append(dict(kind="warning",message=message)),
        prompt_confirm=confirm,
        print_output_format=lambda args,message=None,**_:outputs.append(message),
    )

    def request(args: argparse.Namespace, data: Any = None, json: Any = None) -> SimpleNamespace:
        path, _, query = args.uri.partition("?")
        url = "https://api.fabric.microsoft.com/v1/" + quote(path,safe="/")
        if query:
            url += "?" + urlencode(dict(parse_qsl(query,keep_blank_values=True)))
        requests.append(dict(method=args.method,url=url,payload=json))
        reply = replies.popleft()
        if reply["status"] in (401,403,404):
            code,message={401:("Unauthorized",common.CommonErrors.unauthorized()),403:("Forbidden",common.CommonErrors.forbidden()),404:("NotFound",common.CommonErrors.resource_not_found())}[reply["status"]]
            raise errors.FabricCLIError(message,code)
        return SimpleNamespace(status_code=reply["status"],text=__import__("json").dumps(reply.get("data")))

    base = dict(Namespace=argparse.Namespace,json=json,os=os,re=re,escape=escape,fab_constant=const,constant=const,FabricCLIError=errors.FabricCLIError,ErrorMessages=SimpleNamespace(Common=common.CommonErrors),utils_ui=ui)
    api_utils = load("client/fab_api_utils.py",dict(base,fabric_api=SimpleNamespace(do_request=request)))
    api = load("client/fab_api_shortcuts.py",dict(base,fabric_api=SimpleNamespace(do_request=request),api_utils=api_utils))
    utils = load("utils/fab_cmd_ln_utils.py",base)
    env = dict(base,shortcut_api=api,handle_context=SimpleNamespace(get_command_context=lambda _:target),
               ItemType=types.ItemType,OneLakeItem=Context,utils_ln=utils)
    failure = None
    try:
        if case.get("command","ln")=="ln":
            load("commands/fs/fab_fs_ln.py",env).exec_command(args,context)
        elif case["command"]=="rm":
            args.ws_id,args.id,args.path,args.sc_name,args.name = "workspace","item",case["folder"],case["name"],case.get("display_name","Chosen.Shortcut")
            api.delete_shortcut(args,args.force,case.get("verbose",True))
        else:
            args.query,args.input=case["query"],case["value"]
            util=functions("utils/fab_util.py",{"remove_dot_suffix"},{})
            jmes=load("utils/fab_jmespath.py",dict(base,jmespath=jmespath))
            logger=SimpleNamespace(log_warning=lambda message:notices.append(dict(kind="warning",message=message)))
            set_utils=load("utils/fab_cmd_set_utils.py",dict(base,utils_jmespath=jmes,fab_logger=logger,CommonErrors=common.CommonErrors))
            get=load("commands/fs/get/fab_fs_get_onelake.py",dict(base,shortcut_api=api,utils=SimpleNamespace(**util),utils_get=SimpleNamespace(query_and_export=lambda *_:None)))
            rm=load("commands/fs/rm/fab_fs_rm_onelake.py",dict(base,shortcut_api=api,OneLakeItemType=types.OneLakeItemType))
            load("commands/fs/set/fab_fs_set_onelake.py",dict(base,shortcut_api=api,get_onelake=get,rm_onelake=rm,utils_set=set_utils)).onelake_shortcut(context,args)
    except errors.FabricCLIError as error:
        failure = dict(code=error.status_code,message=error.message)
    if replies:
        raise AssertionError("Unused original shortcut responses")
    return dict(requests=requests,notices=notices,prompts=prompts,outputs=outputs,error=failure)


def main() -> None:
    """Compare original shortcut/query traces with the real Rust composition."""
    parser=argparse.ArgumentParser()
    parser.add_argument("--probe",required=True)
    args=parser.parse_args()
    cases=[]
    if jmespath.__version__!="1.0.1":raise AssertionError("The development-only original query evaluator must be jmespath 1.0.1")
    for path in ["Files/Chosen.Shortcut","Tables/Chosen.Shortcut","files/a b.Shortcut","Files/\u65e5.Shortcut","Files/file.weird"]:
        for force,confirm in [(False,False),(False,True),(True,True)]:
            allowed=path.lower().endswith(".shortcut")
            cases.append(dict(path=path,target="Files/source",force=force,confirm=confirm,replies=[dict(status=201,data={"name":"Created"})] if allowed and (force or confirm) else []))
    for path in ["Files/a.b.Shortcut","Files/a+.Shortcut","Files/a?.Shortcut","Files/.Shortcut","Files/a%.Shortcut"]:
        cases.append(dict(path=path,target="Files/source",force=True,replies=[]))
    for spec in [
        dict(path="Files/Chosen.Shortcut",item_type="Warehouse",target="Files/source"),
        dict(path="Code/Chosen.Shortcut",target="Files/source"),
        dict(path="Files/Chosen.Shortcut"),
        dict(path="Files/Chosen.Shortcut",target="Files/source",input=["{}"]),
        dict(path="Files/Chosen.Shortcut",target="Files/source",type="amazonS3"),
        dict(path="Files/Chosen.Shortcut",input=["not JSON"]),
    ]:
        cases.append(dict(spec,force=True,replies=[]))
    for tokens in [
        ["{'workspaceId':'other','itemId':'target','path':'Files'}"],
        ['"{\'connectionId\': \'connection\', \'location\': \'bucket/path\'}"'],
        ["{'nested':", "[true,false,null,1]}"],
        ["[]"],
        ["null"],
    ]:
        cases.append(dict(path="Files/Chosen.Shortcut",type="amazonS3",input=tokens,force=True,replies=[dict(status=200,data={"name":"Chosen"})]))
    for status in [200,201,202,204]:
        cases.append(dict(path="Files/Chosen.Shortcut",target="Files/source",force=True,replies=[dict(status=status,data={"name":"Created"})]))
    cases.append(dict(path="Files/Chosen.Shortcut",target="Files/source",force=True,replies=[dict(status=201,data={})]))
    for status in [200,201,202,204]:
        for force,confirm,verbose in [(False,False,True),(False,True,True),(True,True,True),(True,True,False)]:
            cases.append(dict(command="rm",path="Files/Chosen.Shortcut",folder="Files/a b",name="\u65e5",
                              force=force,confirm=confirm,verbose=verbose,replies=[dict(status=status)] if force or confirm else []))
    metadata={"name":"Chosen","path":"Files","target":{"type":"oneLake","oneLake":{"workspaceId":"other","itemId":"other-item","path":"Files/source"}},"extra":[1,2]}
    for query,value in [("description","hello"),("target.oneLake.path","Files/new"),("name","Renamed"),("target","{\"oneLake\":{\"path\":\"Files/new\"}}"),("extra[-1]","null")]:
        for force,confirm in [(False,False),(False,True),(True,True)]:
            replies=[dict(status=200,data=metadata)]
            if force or confirm:
                replies+=[dict(status=200,data={})]
                if query=="name":replies+=[dict(status=200)]
            cases.append(dict(command="set",path="Files/Chosen",folder="Files",name="Chosen",query=query,value=value,force=force,confirm=confirm,replies=replies))
    for query in ["id","type","target.*"]:
        cases.append(dict(command="set",path="Files/Chosen",query=query,value="x",force=True,replies=[]))
    cases.append(dict(command="set",path="Files/Chosen",query="name",value="Renamed",force=True,
                      replies=[dict(status=200,data=metadata),dict(status=202,data={}),dict(status=200)]))
    cases.append(dict(command="set",path="Files/Chosen",query="name",value="Renamed",force=True,
                      replies=[dict(status=200,data=metadata),dict(status=200,data={}),dict(status=403)]))
    expected=[reference(case) for case in cases]
    run=subprocess.run([args.probe],input="".join(json.dumps(case)+"\n" for case in cases),text=True,encoding="utf-8",capture_output=True,check=True)
    actual=[json.loads(line) for line in run.stdout.splitlines()]
    if len(actual)!=len(expected):raise AssertionError("Wrong shortcut output count")
    for case,old,new in zip(cases,expected,actual,strict=True):
        if old!=new:raise AssertionError(json.dumps(dict(case=case,python=old,rust=new),indent=2))
    print(json.dumps(dict(source=PIN,shortcut_cases=len(cases),result="passed")))


if __name__=="__main__":
    main()

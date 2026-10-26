# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Actual pinned raw API wrapper/client with synthetic identity and wire boundaries."""

import argparse
import copy
import json
import sys
from pathlib import Path
from types import ModuleType,SimpleNamespace
from unittest.mock import patch
from urllib.parse import urlencode
from typing import Any

sys.path.insert(0,str(Path(__file__).resolve().parents[1] / "access"))
from reference import SOURCE,SourceError,source_module
from label_reference import output_source

HERE=Path(__file__).resolve().parent


def inputs()->list[dict]:
    """Source verbs/statuses/audiences/input/query/header/parameter variations."""
    cases=[]
    for method in ("get","post","put","patch","delete"):
        cases.append({"name":method,"method":method,"status":200,"response":{"ok":True},"input":{"x":1} if method!="get" else None})
    for audience in ("fabric","storage","azure","powerbi"):
        cases.append({"name":audience,"audience":audience,"method":"get","status":200,"response":{"value":[1,2]}})
    cases.extend([
        {"name":"empty","method":"delete","status":204,"raw_body":" \n"},
        {"name":"error-data","status":403,"response":{"errorCode":"Denied","message":"No"}},
        {"name":"headers","status":200,"response":{"value":[]},"show_headers":True,"response_headers":{"x-test":"value"}},
        {"name":"params","status":200,"response":{},"endpoint":"resources?x=uri%20value","params":"x=original,p= *~"},
        {"name":"query","status":200,"response":{"value":[1,2]},"query":"text.value"},
        {"name":"query-empty-fallback","status":200,"response":{"value":[]},"query":"text.value"},
        {"name":"custom-header","status":200,"response":{},"headers":"x-correlation=synthetic"},
        {"name":"invalid-params","status":200,"response":{},"params":"invalid"},
    ])
    return cases


def execute(case:dict)->dict:
    """Execute real source build-pairs, request wrapper and do_request functions."""
    requests,scopes,outputs=[],[],[]
    raw=case.get("raw_body",json.dumps(case.get("response")))
    response_headers=case.get("response_headers",{})
    args=argparse.Namespace(endpoint=case.get("endpoint","resources"),audience=case.get("audience"),
        method=case.get("method","get"),input=case.get("input"),file_path=None,
        params=case.get("params"),headers=case.get("headers"),query=case.get("query"),
        show_headers=case.get("show_headers",False),command="api",output_format="json")
    constants=SimpleNamespace(
        SCOPE_FABRIC_DEFAULT=["https://api.fabric.microsoft.com/.default"],
        SCOPE_ONELAKE_DEFAULT=["https://storage.azure.com/.default"],
        SCOPE_AZURE_DEFAULT=["https://management.azure.com/.default"],
        API_ENDPOINT_FABRIC="api.fabric.microsoft.com",API_ENDPOINT_ONELAKE="onelake.dfs.fabric.microsoft.com",
        API_ENDPOINT_AZURE="management.azure.com",API_ENDPOINT_POWER_BI="api.powerbi.com/v1.0/myorg",
        API_VERSION_FABRIC="v1",FABRIC_SKILL_HEADER="x-fabric-skill",ERROR_INVALID_INPUT="InvalidInput")

    class Auth:
        def get_access_token(self, scope: list[str]) -> str:
            scopes.append(scope)
            return "offline-only"

    auth_module=ModuleType("fabric_cli.core.fab_auth")
    auth_module.FabAuth=Auth
    context_module=ModuleType("fabric_cli.core.fab_context")
    context_module.Context=lambda:SimpleNamespace(command="api",fabric_skill=None)

    class Session:
        def request(self, method: str, url: str, **kwargs: Any) -> SimpleNamespace:
            requests.append({"method":method.upper(),"url":url,
                "body":kwargs.get("json"),"headers":{k.lower():v for k,v in kwargs["headers"].items() if k not in ("Authorization","User-Agent")}})
            return SimpleNamespace(status_code=case["status"],text=raw,content=raw.encode(),headers=response_headers)

    common={"json":json,"FabricCLIError":SourceError,"fab_constant":constants,
        "fab_logger":SimpleNamespace(log_debug_http_request=lambda *a:None,log_debug_http_response=lambda *a:None),
        "time":SimpleNamespace(time=lambda:0),
        "requests":SimpleNamespace(compat=SimpleNamespace(urlencode=urlencode),exceptions=SimpleNamespace(RequestException=RuntimeError)),
        "_get_session":lambda:Session(),"_build_user_agent":lambda command:"oracle",
        "_transform_workspace_url_for_private_link_if_needed":lambda url,uri,**kwargs:url,
        "ApiResponse":lambda **kw:SimpleNamespace(**kw)}
    client=source_module("client/fab_api_client.py",common)
    # The two host-owned boundaries are injected after loading all source functions.
    client["_get_session"]=lambda:Session()
    client["_build_user_agent"]=lambda command:"oracle"
    client["_transform_workspace_url_for_private_link_if_needed"]=lambda url,uri,**kwargs:url
    import jmespath
    query=source_module("utils/fab_jmespath.py",{**common,"jmespath":jmespath})
    util=SimpleNamespace(process_nargs=lambda arg:" ".join(arg).strip("\"'") if isinstance(arg,list) else arg)
    request=source_module("commands/api/fab_api_request.py",{
        "json":json,"fabric_api":SimpleNamespace(do_request=client["do_request"]),"utils":util,
        "utils_jmespath":SimpleNamespace(search=query["search"]),
        "fab_ui":SimpleNamespace(print_output_format=lambda args,**kw:outputs.append(kw["data"]))})
    wrapper=source_module("commands/api/fab_api.py",{"FabricCLIError":SourceError,"fab_constant":constants})
    error=None
    with patch.dict(sys.modules,{"fabric_cli.core.fab_auth":auth_module,"fabric_cli.core.fab_context":context_module}):
        try:
            wrapper["_build_query_parameters"](args)
            wrapper["_build_headers"](args)
            request["exec_command"](args)
        except SourceError as exception:
            error={"code":exception.status_code,"message":exception.message}
    stdout=""
    if outputs:
        output=output_source()["FabricCLIOutput"](command="api",data=outputs[0])
        output._timestamp="2000-01-01T00:00:00Z"
        stdout=json.dumps(output._to_dict(),indent=4,ensure_ascii=False)+"\n"
    return {"requests":requests,"scopes":scopes,"data":outputs[0] if outputs else None,"error":error,"stdout":stdout,"stderr":"","exit":0 if error is None else 1}


def main()->None:
    """Check source-only immutable expectations, with a non-overwriting first capture."""
    document={"source_commit":SOURCE,"cases":[{**case,"expected":execute(case)} for case in inputs()]}
    parser=argparse.ArgumentParser()
    parser.add_argument("--initial-capture",action="store_true")
    args=parser.parse_args()
    path=HERE / "cases.json"
    if args.initial_capture:
        if path.exists():raise SystemExit("Refusing to overwrite existing raw API oracle")
        path.write_text(json.dumps(document,indent=2)+"\n",encoding="utf-8")
    else:
        assert document==json.loads(path.read_text(encoding="utf-8")),"Raw API source expectations changed"
    print(f"raw API source: {len(document['cases'])} cases passed")


if __name__=="__main__":
    main()

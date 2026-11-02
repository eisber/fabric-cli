# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute exact Python find handlers and hidden read routes offline."""

import argparse
import builtins
import copy
import json
import re
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch
from typing import Any

sys.path.insert(0,str(Path(__file__).resolve().parents[1] / "access"))
from reference import ROOT, SOURCE, SourceError, source_module

HERE = Path(__file__).resolve().parent


def inputs() -> list[dict]:
    """Independent search projection/filter/paging/error matrix."""
    rows = [
        {"id":"a","displayName":"Alpha","type":"Report","description":"Details","workspaceName":"flat","workspaceId":"flat-id","hierarchy":{"workspace":{"displayName":"Hierarchy","id":"hierarchy-id"}}},
        {"id":"b","name":"Beta","type":"Lakehouse","workspaceName":"Flat"},
    ]
    cases=[]
    for name,params,long,query in [
        ("default",None,False,None),("long",None,True,None),("eq","type=report",False,None),
        ("multiple","type=[report,lakehouse]",False,None),("ne","type!=[Dashboard,Report]",False,None),
        ("unsupported","type=Dashboard",False,None),("unknown","type=NotAType",False,None),
        ("empty-types","type=[]",False,None),("parameter","other=value",False,None),
        ("invalid-params","invalid",False,None),("query-scalar",None,False,"length(@)"),
        ("query-empty",None,False,"[?name=='Missing']"),("query",None,True,"[].{name:name,workspace:workspace}"),
    ]:
        cases.append({"name":name,"params":params,"long":long,"query":query,"responses":[{"status":200,"body":{"value":copy.deepcopy(rows)}}]})
    cases.append({"name":"paged","params":"type=report","responses":[{"status":200,"body":{"value":[rows[0]],"continuationToken":"next"}},{"status":200,"body":{"value":[rows[1]]}}]})
    cases.append({"name":"empty","responses":[{"status":200,"body":{"value":[]}}]})
    cases.append({"name":"service-error","responses":[{"status":400,"body":{"errorCode":"InvalidSearch","message":"Bad search"}}]})
    return cases


def execute(case:dict,trace:bool=False)->dict:
    """Execute actual source command, without importing product/runtime/identity."""
    requests,outputs,diagnostics=[],[],[]
    replies=iter(case["responses"])
    args=argparse.Namespace(search_text="needle",params=case.get("params"),long=case.get("long",False),query=case.get("query"),fab_mode="interactive" if case.get("interactive") else "cli",output_format="json")
    errors=SimpleNamespace(Common=SimpleNamespace(
        unsupported_parameter=lambda key:f"'{key}' isn't a supported parameter",
        type_not_supported=lambda name:f"The type '{name}' is not supported",
        invalid_parameter_format=lambda p:f"Invalid parameter format: '{p}'. Use key=value or key!=value.",
        invalid_jmespath_query=lambda:"Invalid jmespath query (https://jmespath.org)",
        invalid_json_format=lambda:"Invalid JSON format"),
        Find=SimpleNamespace(unrecognized_type=lambda t,h:f"'{t}' isn't a recognized item type.{h}",search_failed=lambda m:f"Catalog search failed: {m}"))
    common={"json":json,"re":re,"FabricCLIError":SourceError,"ErrorMessages":errors,
        "fab_constant":SimpleNamespace(ERROR_INVALID_INPUT="InvalidInput",ERROR_UNSUPPORTED_ITEM_TYPE="UnsupportedItemType",ERROR_INVALID_ITEM_TYPE="InvalidItemType",ERROR_INVALID_JSON="InvalidJson",ERROR_UNEXPECTED_ERROR="UnexpectedError",FAB_MODE_INTERACTIVE="interactive"),
        "utils_ui":SimpleNamespace(print_grey=diagnostics.append,print_output_format=lambda args,**kw:outputs.append(kw["data"])),
        "fab_logger":SimpleNamespace(log_debug=lambda *a:None)}
    util=source_module("utils/fab_util.py",common)
    common["utils"]=SimpleNamespace(**util)
    import jmespath
    query=source_module("utils/fab_jmespath.py",{**common,"jmespath":jmespath})
    common["utils_jmespath"]=SimpleNamespace(search=query["search"])

    def do_request(args: argparse.Namespace, json: Any = None) -> SimpleNamespace:
        requests.append({"method":args.method.upper(),"url":f"https://api.fabric.microsoft.com/v1/{args.uri}","body":copy.deepcopy(json)})
        reply=next(replies)
        return SimpleNamespace(status_code=reply["status"],text=__import__("json").dumps(reply["body"]))

    api=source_module("client/fab_api_catalog.py",{"fabric_api":SimpleNamespace(do_request=do_request)})
    common["catalog_api"]=SimpleNamespace(search=api["search"])
    types=subprocess.run(["git","show",f"{SOURCE}:src/fabric_cli/commands/find/type_supported.yaml"],cwd=ROOT,check=True,capture_output=True,text=True).stdout
    common["ALL_ITEM_TYPES"]=[line.strip()[2:] for line in types.splitlines() if line.strip().startswith("- ")]
    common["UNSUPPORTED_ITEM_TYPES"]=["Dashboard"]
    handler=source_module("commands/find/fab_find.py",common)
    error=None
    questions=[]
    answers=iter(case.get("confirmations",[]))
    def prompt(question: str) -> str:
        questions.append(question)
        if not next(answers): raise EOFError("synthetic source EOF")
        return ""
    with patch.object(builtins,"input",prompt):
        try:
            handler["find_command"](args)
        except SourceError as exception:
            error={"code":exception.status_code,"message":exception.message}
    result={"requests":requests,"data":outputs[0] if outputs else None,"diagnostics":diagnostics,"error":error}
    if trace: result.update(outputs=outputs,questions=questions)
    return result


def interactive_inputs()->list[dict]:
    """Additional source cases without altering the original command-line fixture."""
    page={"id":"a","displayName":"A","type":"Report"}
    return [
        {"name":"interactive-empty-then-result","interactive":True,"confirmations":[],"responses":[
            {"status":200,"body":{"value":[],"continuationToken":"next"}},{"status":200,"body":{"value":[page]}}]},
        {"name":"interactive-continue","interactive":True,"confirmations":[True],"responses":[
            {"status":200,"body":{"value":[page],"continuationToken":"next"}},{"status":200,"body":{"value":[page]}}]},
        {"name":"interactive-eof","interactive":True,"confirmations":[False],"responses":[
            {"status":200,"body":{"value":[page],"continuationToken":"next"}}]},
        {"name":"repeated-type","params":"type=Report,type!=Dashboard,type=Lakehouse","responses":[{"status":200,"body":{"value":[page]}}]},
        {"name":"mixed-parameter","params":"type=Report,other=bad","responses":[{"status":200,"body":{"value":[]}}]},
        {"name":"raw-auth-service-code","responses":[{"status":401,"body":{"errorCode":"ServiceDenied","message":"Selected identity denied"}}]},
    ]


def hidden_routes()->list[dict]:
    """Route expectations come from the actual source API functions."""
    result=[]
    for name,file,function,extra in [
        ("connections","fab_api_connection.py","list_connections",None),
        ("gateways","fab_api_gateway.py","list_gateways",None),
        ("domains","fab_api_domain.py","list_domains",None),
        ("connection-types","fab_api_connection.py","list_supported_connection_types",None),
        ("spark-pools","fab_api_workspace.py","ls_workspace_spark_pools","workspace"),
        ("private-endpoints","fab_api_workspace.py","ls_workspace_managed_private_endpoints",None),
        ("shares","fab_api_item.py","list_item_external_data_shares",None),
        ("domain-workspaces","fab_api_domain.py","list_domain_workspaces",None),
    ]:
        observed=[]
        api=source_module(f"client/{file}",{"fabric_api":SimpleNamespace(do_request=lambda args:observed.append({"method":args.method.upper(),"url":f"https://api.fabric.microsoft.com/v1/{args.uri}"}))})
        args=argparse.Namespace(ws_id="workspace",item_id="item",id="domain")
        api[function](args,*([extra] if extra else []))
        result.append({"name":name,"request":observed[0]})
    return result


def main()->None:
    """Compare an immutable additive capture against pinned source execution."""
    document={"source_commit":SOURCE,"cases":[{**case,"expected":execute(case)} for case in inputs()],"hidden":hidden_routes()}
    parser=argparse.ArgumentParser()
    parser.add_argument("--initial-capture",action="store_true")
    parser.add_argument("--interactive-capture",action="store_true")
    args=parser.parse_args()
    path=HERE / "cases.json"
    if args.initial_capture:
        if path.exists(): raise SystemExit("Refusing to overwrite existing catalog oracle")
        path.write_text(json.dumps(document,indent=2)+"\n",encoding="utf-8")
    else:
        assert document==json.loads(path.read_text(encoding="utf-8")),"Catalog source expectations changed"
    extra={"source_commit":SOURCE,"cases":[{**case,"expected":execute(case,True)} for case in interactive_inputs()]}
    extra_path=HERE / "interactive_cases.json"
    if args.interactive_capture:
        if extra_path.exists():raise SystemExit("Refusing to overwrite extra catalog oracle")
        extra_path.write_text(json.dumps(extra,indent=2)+"\n",encoding="utf-8")
    elif extra_path.exists():
        assert extra==json.loads(extra_path.read_text(encoding="utf-8")),"Interactive/additional source expectations changed"
    print(f"catalog source: {len(document['cases'])} find cases and {len(document['hidden'])} hidden routes passed")


if __name__=="__main__":
    main()

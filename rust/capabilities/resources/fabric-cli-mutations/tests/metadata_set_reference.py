# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pinned item metadata SET/command policy vs public mutation consumer."""

from __future__ import annotations

import argparse
import ast
import enum
import html
import json
import re
import subprocess
from types import SimpleNamespace
from typing import Any

import jmespath

from creation_policy_reference import support_checker
from error_reference import constants
from python_reference import CLIError, PIN, load, source, source_wait


def item_constants()->dict[str,Any]:
    """Execute original ordered item constants, including their name references."""
    nodes=[node for node in ast.parse(source("core/fab_constant.py")).body
        if isinstance(node,ast.Assign) and all(isinstance(target,ast.Name)
            and (target.id.startswith("ITEM_QUERY_") or target.id=="ITEM_SET_ALLOWED_METADATA_KEYS") for target in node.targets)]
    values:dict[str,Any]={}
    exec(compile(ast.Module(body=nodes,type_ignores=[]),f"{PIN}:item-constant-assignments","exec"),values)
    return {key:value for key,value in values.items() if key!="__builtins__"}


def reference(case:dict[str,Any])->dict[str,Any]:
    """Execute original policy, metadata handler, replacement, API and cache bodies."""
    result:dict[str,Any]=dict(requests=[],outputs=[],error=None,metadata_reads=0,prompts=[],notices=[],renamed=[],error_message=None)
    try:support_checker("set" if case["command"]=="set" else "rm")(case["kind"])
    except CLIError as error:
        result.update(error=error.status_code,error_message=error.args[0]);return result
    const=SimpleNamespace(**item_constants(),**constants({
        "SET_COMMAND_INVALID_QUERIES","ERROR_INVALID_QUERY","ERROR_UNSUPPORTED_COMMAND"}))
    exceptions=load("core/fab_exceptions.py",dict(json=json,html=html,re=re))
    common=load("errors/common.py",{})
    types=load("core/fab_types.py",dict(Enum=enum.Enum,auto=enum.auto,List=list))
    item=SimpleNamespace(id="id",name=f"Thing.{case['kind']}",short_name="Thing",workspace=SimpleNamespace(id="ws"),
        item_type=types.ItemType.from_string(case["kind"]),extract_friendly_name_path_or_default=lambda query:query)
    def confirm(prompt:str="Are you sure?")->bool:
        result["prompts"].append(prompt);return case.get("confirm",True)
    ui=SimpleNamespace(prompt_confirm=confirm,
        print_grey=lambda message:result["notices"].append(dict(kind="progress",message=message)),
        print_warning=lambda message:result["notices"].append(dict(kind="warning",message=message)),
        print_output_format=lambda args,message=None,**_:result["outputs"].append(dict(message=message,data=None)),
    )
    def request(args:argparse.Namespace,data:Any=None,**_:Any)->Any:
        if args.method=="get":
            result["metadata_reads"]+=1
            return SimpleNamespace(status_code=200,text=json.dumps(case["metadata"]))
        result["requests"].append(dict(method=args.method,uri=args.uri,payload=json.loads(data) if data else None,wait=source_wait(args)))
        return SimpleNamespace(status_code=case.get("status",200))
    base=dict(Namespace=argparse.Namespace,json=json,fab_constant=const,constant=const,utils_ui=ui,
        FabricCLIError=exceptions.FabricCLIError,CommonErrors=common.CommonErrors)
    api_utils=load("client/fab_api_utils.py",dict(base,fabric_api=SimpleNamespace(do_request=request)))
    api=load("client/fab_api_item.py",dict(base,fabric_api=SimpleNamespace(do_request=request),api_utils=api_utils))
    jmes=load("utils/fab_jmespath.py",dict(jmespath=jmespath))
    helpers=load("utils/fab_cmd_set_utils.py",dict(base,utils_jmespath=jmes,
        fab_logger=SimpleNamespace(log_warning=lambda message:result["notices"].append(dict(kind="warning",message=message)))))
    cache=SimpleNamespace(upsert_item_to_cache=lambda value:result["renamed"].append(value._name))
    module=load("commands/fs/set/fab_fs_set_item.py",dict(base,item_api=api,format_mapping=types.format_mapping,
        definition_format_mapping=types.definition_format_mapping,utils_set=helpers,utils_mem_store=cache))
    args=argparse.Namespace(force=case.get("force",False),query=case["query"],input=case["input"],request_params={})
    try:module.exec(item,args)
    except exceptions.FabricCLIError as error:
        result.update(error=error.status_code,error_message=error.message)
    return result


def main()->None:
    """Compare full source item policy and actual item metadata path/status/UX cases."""
    parser=argparse.ArgumentParser();parser.add_argument("--probe",required=True);args=parser.parse_args()
    if jmespath.__version__!="1.0.1":raise AssertionError("Pinned original query evaluator must be 1.0.1")
    types=load("core/fab_types.py",dict(Enum=enum.Enum,auto=enum.auto,List=list))
    metadata={"displayName":"Thing","description":"before","properties":{"values":[1,2],"scalar":"string","keep":True}}
    cases=[]
    for kind in types.ItemType:
        cases.append(dict(kind=kind.value,command="set",id="id",query="displayName",input="After",metadata=metadata,force=True,capture_set=True,status=200))
        # A rejected delete is still a command-policy result, not a generic
        # endpoint fallback, so its exact alias diagnostic is checked separately.
        try:support_checker("rm")(kind.value)
        except CLIError:
            cases.append(dict(kind=kind.value,command="remove",id="id",query="",input="",metadata=metadata,force=True,capture_set=True))
    for query,value in [
        ("displayName","After"),("description","null"),("properties.new",'{"a":[1,true,null]}'),
        ("properties.values[0]","3"),("properties.values[-1]","false"),
        ("properties.values[9]","3"),("properties.missing.child","x"),("properties.scalar.child","x"),
        ("invalid","x"),("id","x"),("properties[0]","x"),("properties.*","x"),("properties.values[*]","x"),
        ("properties.","x"),
    ]:
        for force,confirm in [(False,False),(False,True),(True,True)]:
            for status in [200,201,202,204]:
                cases.append(dict(kind="Notebook",command="set",id="id",query=query,input=value,metadata=metadata,
                    force=force,confirm=confirm,status=status,capture_set=True))
    expected=[reference(case) for case in cases]
    run=subprocess.run([args.probe],input="".join(json.dumps(case)+"\n" for case in cases),
        text=True,encoding="utf-8",capture_output=True,check=True)
    actual=[json.loads(line) for line in run.stdout.splitlines()]
    for case,old,new in zip(cases,expected,actual,strict=True):
        if old!=new:raise AssertionError(json.dumps(dict(case=case,python=old,rust=new),indent=2))
    print(json.dumps(dict(source=PIN,item_metadata_set_cases=len(cases),source_item_types=len(types.ItemType),result="passed")))


if __name__=="__main__":
    main()

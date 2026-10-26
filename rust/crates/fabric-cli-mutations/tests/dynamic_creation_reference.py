# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Original dynamic Report/mirror/mounted definitions, compared at real requests."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from creation_reference import verify_templates
from operation_reference import functions
from python_reference import CLIError, PIN, reference


def main() -> None:
    """Execute original helpers, asserting exact base64 JSON, order and effects."""
    parser=argparse.ArgumentParser()
    parser.add_argument("--probe",required=True)
    parser.add_argument("--source-export",required=True)
    args=parser.parse_args()
    root=Path(args.source_export).resolve()
    verify_templates(root)
    os.environ["FABRIC_PARITY_TEMPLATE_SOURCE"]=str(root)
    cases:list[dict[str,Any]]=[]
    for parent in ["root","folder"]:
        for model in ["model-id",'model-\\日\n"😀',True,123]:
            for status in [201,202]:
                cases.append(dict(kind="Report",command="create",params={"semanticModelId":model,"Description":"report"},
                                  source_parent=parent,capture_creation=True,responses=[dict(status=status,data={"id":"root-id"})]))
        for model in [None,"",False,0]:
            for child,reply in [
                (dict(status=201,data={"id":"dependent"}),dict(status=201,data={"id":"root-id"})),
                (dict(status=202,data={}),dict(status=201,data={"id":"root-id"})),
                (dict(status=200,data={}),dict(status=200,data={})),
                (dict(status=201,data={"id":"dependent"}),dict(status=202,data={})),
                (dict(status=201,data={"id":"dependent"}),dict(status=403,error="Forbidden")),
                (dict(status=202,data={}),dict(status=403,error="Forbidden")),
            ]:
                cases.append(dict(kind="Report",command="create",params={"semanticModelId":model,"Description":"inherited"},
                                  source_parent=parent,capture_creation=True,responses=[child,reply]))
    for kind in ["AzureSQL","azuresqlmi","sNoWfLaKe","CosmosDB","unknown","genericmirror","",None,False]:
        for params in [
            {},
            {"connectionId":"synthetic", "database":"db", "defaultSchema":"schema", "mountedTables":"sales.order,dbo.customer"},
            {"connectionId":None,"database":False,"defaultSchema":None,"mountedTables":""},
            {"connectionId":'quote\\日"\n😀',"database":"DB\\文","defaultSchema":"","mountedTables":".table,schema."},
            {"connectionId":False,"database":[1,True,None],"defaultSchema":False,"mountedTables":" s . t "},
        ]:
            cases.append(dict(kind="MirroredDatabase",command="create",params=dict(params,mirrorType=kind),
                              source_parent="folder",capture_creation=True,responses=[dict(status=201,data={"id":"mirror"})]))
    for values in [
        ["sub","group","factory"],
        ['sub"\\日',"group\n\t","factory😀"],
        ["","",""],
        [False,0,True],
        [1.0,-0.0,1e-7],
        [["nested",True,False,None],{"key":"value"},123],
        [{"quote":"single'and\"double","unicode":"日"},["x",1],False],
    ]:
        params=dict(zip(["subscriptionId","resourceGroup","factoryName"],values,strict=True))
        cases.append(dict(kind="MountedDataFactory",command="create",params=params,capture_creation=True,
                          responses=[dict(status=201,data={"id":"mounted"})]))
    expected=[reference(case) for case in cases]
    run=subprocess.run([args.probe],input="".join(json.dumps(case)+"\n" for case in cases),
                       text=True,encoding="utf-8",capture_output=True,check=True)
    actual=[json.loads(line) for line in run.stdout.splitlines()]
    if len(actual)!=len(expected):raise AssertionError("Wrong dynamic creation output count")
    for case,old,new in zip(cases,expected,actual,strict=True):
        if old!=new:raise AssertionError(json.dumps(dict(case=case,python=old,rust=new),indent=2))
    malformed=[]
    for params in [
        {"mirrorType":True},{"mirrorType":12},{"mirrorType":1.0},{"mirrorType":[1]},{"mirrorType":{"x":1}},
        {"mirrorType":"snowflake","mountedTables":[1]},
        {"mirrorType":"azuresql","mountedTables":True},
        {"mirrorType":"cosmosdb","mountedTables":1},
        *[{"mirrorType":"unknown","mountedTables":value} for value in ["missing","too.many.parts","ok.table,",",",""]],
    ]:
        if params.get("mountedTables")=="":
            continue
        malformed.append(dict(kind="MirroredDatabase",command="create",params=params,capture_error=True,responses=[]))
    errors=[]
    for case in malformed:
        try:
            reference(case)
        except (AttributeError,ValueError) as failure:
            record:dict[str,Any]={}
            def print_error(error:CLIError,**_:Any)->None:
                record.update(requests=[],outputs=[],error=error.status_code,error_message=error.args[0].rstrip("."))
            def exit_error(code:int)->None:
                if code!=2:raise AssertionError("Wrong original unexpected-error exit")
            original=functions("main.py",{"_handle_unexpected_error"},dict(
                FabricCLIError=CLIError,fab_constant=SimpleNamespace(ERROR_UNEXPECTED_ERROR="UnexpectedError",EXIT_CODE_ERROR=2),
                fab_ui=SimpleNamespace(print_output_error=print_error),sys=SimpleNamespace(exit=exit_error),
            ))
            original["_handle_unexpected_error"](failure,SimpleNamespace(output_format="json"))
            errors.append(record)
        else:
            raise AssertionError("Malformed original definition unexpectedly succeeded")
    run=subprocess.run([args.probe],input="".join(json.dumps(case)+"\n" for case in malformed),
                       text=True,encoding="utf-8",capture_output=True,check=True)
    actual=[json.loads(line) for line in run.stdout.splitlines()]
    for case,old,new in zip(malformed,errors,actual,strict=True):
        if old!=new:raise AssertionError(json.dumps(dict(case=case,python=old,rust=new),indent=2))
    print(json.dumps(dict(source=PIN,dynamic_creation_cases=len(cases),malformed_cases=len(malformed),result="passed")))


if __name__=="__main__":
    main()

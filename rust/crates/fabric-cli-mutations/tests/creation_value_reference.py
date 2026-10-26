# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Original finite creation value/type/short-circuit and Unicode decimal contracts."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import unicodedata
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import python_reference
from creation_reference import verify_templates
from operation_reference import functions
from python_reference import CLIError, PIN, load

LAST_ERROR:list[str]=[]


class CapturedError(CLIError):
    """Observe injected exception construction without changing original bodies."""

    def __init__(self,message:str,status_code:str)->None:
        super().__init__(message,status_code)
        LAST_ERROR.append(message.rstrip("."))


def expected(case:dict[str,Any])->dict[str,Any]:
    """Run original handlers and actual outer UnexpectedError conversion."""
    LAST_ERROR.clear()
    try:
        result=python_reference.reference(case)
    except AttributeError as failure:
        record:dict[str,Any]={}
        def output(error:CapturedError,**_:Any)->None:
            record.update(requests=[],outputs=[],error=error.status_code,error_message=error.args[0].rstrip("."))
        def exit_error(code:int)->None:
            if code!=2:raise AssertionError("Wrong source unexpected-error exit")
        original=functions("main.py",{"_handle_unexpected_error"},dict(
            FabricCLIError=CapturedError,fab_constant=SimpleNamespace(ERROR_UNEXPECTED_ERROR="UnexpectedError",EXIT_CODE_ERROR=2),
            fab_ui=SimpleNamespace(print_output_error=output),sys=SimpleNamespace(exit=exit_error)))
        original["_handle_unexpected_error"](failure,SimpleNamespace(output_format="json"))
        return record
    if result["error"] is not None:
        result["error_message"]=LAST_ERROR[-1]
    return result


def main()->None:
    """Compare parsed native values without silently converting them to text."""
    parser=argparse.ArgumentParser();parser.add_argument("--probe",required=True)
    parser.add_argument("--source-export",required=True);args=parser.parse_args()
    root=Path(args.source_export).resolve();verify_templates(root)
    os.environ["FABRIC_PARITY_TEMPLATE_SOURCE"]=str(root)
    python_reference.CLIError=CapturedError
    lower=load("utils/fab_cmd_mkdir_utils.py",{}).lowercase_keys
    cases:list[dict[str,Any]]=[]
    def add(kind:str,params:dict[str,Any])->None:
        cases.append(dict(kind=kind,command="create",params=lower(params),capture_error=True,status=201,data={"id":"created"}))
    for kind,key in [("Lakehouse","enableSchemas"),("Warehouse","enableCaseInsensitive")]:
        for value in [None,True,False,0,1,1.0,[],[1],{},{"Nested":"value"},"true","TrUe","FALSE"," false ",""]:
            add(kind,{key:value})
    for eventhouse in [None,"",False,0,"eventhouse",True,1,{"ID":"eventhouse"}]:
        for dbtype in [None,False,1,{},[],"ReadWrite","SHORTCUT","other",""]:
            add("KQLDatabase",dict(eventhouseId=eventhouse,dbType=dbtype,clusterUri="cluster",databaseName="database"))
    for value in [True,1,[],{},[{"X":"value"}],""]:
        add("KQLDatabase",dict(eventhouseId=True,dbType="shortcut",clusterUri=value,databaseName=value))
    for mode in [True,False,1,1.0,{},[],[{"X":1}],"new ","","Unsupported",None]:
        add("SQLDatabase",dict(creationMode=mode))
    for value in [True,False,0,-1,3.9,-3.9,0.0,-0.0,9007199254740991,9007199254740992.0,
        18446744073709549568.0,18446744073709551615," +1_0 ","-0","1__0","",[],{},[1],"2.5",None,
        "١_٢","𝟙𝟚","²","一"]:
        add("SQLDatabase",dict(creationMode="New",backupRetentionDays=value))
    # Source Python's decimal support is Nd, not char.is_numeric or ASCII-only.
    if unicodedata.unidata_version!="15.1.0":raise AssertionError("Decimal table oracle version changed")
    decimal_blocks=0
    for code in range(0x110000):
        character=chr(code)
        if unicodedata.category(character)=="Nd" and unicodedata.decimal(character)==0:
            add("SQLDatabase",dict(creationMode="New",backupRetentionDays=f" +{chr(code+1)}_{chr(code+2)} "))
            decimal_blocks+=1
    for mode in ["Restore","RestoreDeletedDatabase"]:
        for value in [None,False,0,"",[],{},True,1,{"ID":"value"}]:
            add("SQLDatabase",dict(creationMode=mode,restorePointInTime=value,itemId=value,workspaceId=value,
                                  restorableDeletedDatabaseName=value,referenceType=None))
    expected_results=[expected(case) for case in cases]
    run=subprocess.run([args.probe],input="".join(json.dumps(case)+"\n" for case in cases),
        text=True,encoding="utf-8",capture_output=True,check=True)
    actual=[json.loads(line) for line in run.stdout.splitlines()]
    for case,old,new in zip(cases,expected_results,actual,strict=True):
        if old!=new:raise AssertionError(json.dumps(dict(case=case,python=old,rust=new),indent=2))
    print(json.dumps(dict(source=PIN,creation_value_cases=len(cases),unicode_decimal_blocks=decimal_blocks,result="passed")))


if __name__=="__main__":
    main()

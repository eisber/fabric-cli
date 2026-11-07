# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Original source preflight order vs public pure helpers with synthetic local I/O."""

from __future__ import annotations

import argparse
import enum
import io
import json
import subprocess
from types import SimpleNamespace
from typing import Any

from python_reference import CLIError, PIN, load


def reference(case:dict[str,Any])->dict[str,Any]:
    """Execute unchanged local-copy/check/destination bodies; no real local reads."""
    trace:list[Any]=[]
    resolved=None
    constants=SimpleNamespace(ERROR_INVALID_PATH="InvalidPath",ERROR_NOT_SUPPORTED="NotSupported",ERROR_INVALID_INPUT="InvalidInput")
    errors=load("errors/common.py",{})
    types=load("core/fab_types.py",dict(Enum=enum.Enum,auto=enum.auto,List=list))
    class Context:
        def __init__(self,name:str,identifier:Any,parent:Any,kind:Any)->None:
            self.name=name;self.id=identifier;self.nested_type=kind
            self.path=f"{parent.path}/{name}";self.path_id=f"{parent.path_id}/{name}"
    spec=case["target"]
    target=object.__new__(Context)
    target.nested_type=getattr(types.OneLakeItemType,dict(file="FILE",directory="FOLDER",shortcut="SHORTCUT",table="TABLE",undefined="UNDEFINED")[spec["kind"]])
    target.id=None if spec["kind"]=="undefined" else "resolved"
    target.item=SimpleNamespace(item_type=types.ItemType.from_string(spec["item_type"]))
    target.root_folder=spec["path"].split("/")[0]
    target.path=f"/Team.Workspace/Lake.Lakehouse/{spec['path']}"
    target.path_id=f"/workspace/item/{spec['path']}"
    def is_directory()->bool:
        trace.append("is_directory");return case["source"]=="directory"
    def is_file()->bool:
        trace.append("is_file");return case["source"]!="missing"
    def open_file(path:str,mode:str)->io.BytesIO:
        if (path,mode)!=("local-source","rb"):raise AssertionError("Unexpected local I/O")
        trace.append("read")
        if case["source"]=="read_error":raise OSError("synthetic denied")
        return io.BytesIO(b"" if case["source"]=="empty" else b"synthetic")
    def upload(*_:Any)->None:
        trace.append("upload")
    common=dict(FabricCLIError=CLIError,fab_constant=constants,
        ErrorMessages=SimpleNamespace(Common=errors.CommonErrors),
        OneLakeItem=Context,OneLakeItemType=types.OneLakeItemType,
        ItemOnelakeWritableFoldersMap=types.ItemOnelakeWritableFoldersMap)
    helpers=load("utils/fab_cmd_cp_utils.py",common)
    def destination(target:Any,name:str)->Any:
        nonlocal resolved
        value=helpers.get_onelake_file_destination(target,name)
        resolved=value.path_id.removeprefix("/workspace/item/")
        return value
    source=SimpleNamespace(name=case["source_name"],path="local-source",is_directory=is_directory,is_file=is_file)
    module=load("commands/fs/cp/fab_fs_cp_onelake.py",dict(common,
        cp_utils=SimpleNamespace(check_onelake_destination=helpers.check_onelake_destination,
            get_onelake_file_destination=destination,upload_file_onelake=upload),
        utils_ui=SimpleNamespace(print_grey=lambda message:trace.append(dict(progress=message))),open=open_file,
    ))
    error=None
    try:module.copy_local_2_onelake(source,target,argparse.Namespace())
    except CLIError as failure:error=dict(code=failure.status_code,message=failure.args[0])
    return dict(trace=trace,destination=resolved,error=error)


def main()->None:
    """Assert source precedence over local directory/missing/read/empty failures."""
    parser=argparse.ArgumentParser();parser.add_argument("--probe",required=True);args=parser.parse_args()
    cases=[]
    for item_type,roots in [
        ("Lakehouse",["Files","Tables","files"]),
        ("Warehouse",["Files","Tables"]),("SQLDatabase",["Files","Code"]),
        ("MirroredDatabase",["Files","Tables"]),("MirroredWarehouse",["Files","Tables"]),
        ("SparkJobDefinition",["Libs","Main","Files"]),("SemanticModel",["Tables"]),("KQLDatabase",["Tables"]),
    ]:
        for root in roots:
            for kind in ["file","directory","shortcut","undefined","table"]:
                for source in ["missing","directory","read_error","empty","file"]:
                    cases.append(dict(target=dict(item_type=item_type,path=f"{root}/target",kind=kind),
                        source=source,source_name="source.csv"))
    expected=[reference(case) for case in cases]
    run=subprocess.run([args.probe],input="".join(json.dumps(case)+"\n" for case in cases),
        text=True,encoding="utf-8",capture_output=True,check=True)
    actual=[json.loads(line) for line in run.stdout.splitlines()]
    for case,old,new in zip(cases,expected,actual,strict=True):
        if old!=new:raise AssertionError(json.dumps(dict(case=case,python=old,rust=new),indent=2))
    print(json.dumps(dict(source=PIN,copy_preflight_cases=len(cases),result="passed")))


if __name__=="__main__":
    main()

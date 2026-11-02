# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute original workspace/tenant removal, support policy, sorting and API helpers."""

from __future__ import annotations

import argparse
import ast
import enum
import json
import subprocess
from collections import deque
from types import SimpleNamespace
from typing import Any
from urllib.parse import urlencode

from branch_ledger import support_rows
from operation_reference import functions
from python_reference import CLIError, PIN, load, source


def policy_classes(namespace: dict[str, Any]) -> tuple[type, type]:
    """Compile original hierarchy support methods in their real super call chain."""
    def method(path: str) -> ast.FunctionDef:
        tree=ast.parse(source(path))
        return next(node for node in ast.walk(tree) if isinstance(node,ast.FunctionDef) and node.name=="check_command_support")
    classes=[
        ast.ClassDef(name="Base",bases=[],keywords=[],body=[method("core/hiearchy/fab_element.py")],decorator_list=[]),
        ast.ClassDef(name="Item",bases=[ast.Name(id="Base",ctx=ast.Load())],keywords=[],body=[method("core/hiearchy/fab_base_item.py")],decorator_list=[]),
    ]
    tree=ast.Module(body=[ast.ImportFrom(module="__future__",names=[ast.alias(name="annotations")],level=0),*classes],type_ignores=[])
    ast.fix_missing_locations(tree)
    exec(compile(tree,f"{PIN}:original-support-methods","exec"),namespace)
    return namespace["Base"],namespace["Item"]


def reference(case: dict[str, Any]) -> dict[str, Any]:
    """Run original destructive orchestration against synthetic complete catalog/I/O."""
    responses=deque(case["responses"])
    requests:list[dict[str, Any]]=[]
    notices:list[dict[str, Any]]=[]
    prompts:list[str]=[]
    choices:list[dict[str, Any]]=[]
    outputs:list[str]=[]
    cache:list[str]=[]
    reads:list[str]=[]
    constants=SimpleNamespace(ERROR_ALREADY_EXISTS="AlreadyExists",ERROR_NOT_SUPPORTED="NotSupported",
        ERROR_UNSUPPORTED_COMMAND="UnsupportedCommand",FAB_OUTPUT_ITEM_SORT_CRITERIA="sort",ERROR_INVALID_ITEM_TYPE="InvalidItemType")
    common=load("errors/common.py",{})
    types=load("core/fab_types.py",dict(Enum=enum.Enum,auto=enum.auto,List=list,FabricCLIError=CLIError,
        fab_constant=constants,ErrorMessages=SimpleNamespace(Common=common.CommonErrors)))
    gates=[row for row in support_rows() if row["command"]=="rm"]
    support_dictionary={"commands":{"fs":{"subcommands":{"rm":{
        key:[row["resource"] for row in gates if row["gate"]==key]
        for key in ("unsupported_items","supported_items","supported_elements")
    }}}}}
    command=SimpleNamespace(FS_RM=SimpleNamespace(value="del",name="FS_RM"))
    support=functions("core/fab_commands.py",{
        "_snake_to_camel","_get_property_from_commands_and_subcommands","_get_items",
        "get_supported_elements","get_supported_items","get_unsupported_items",
    },dict(get_command_support_dict=lambda:support_dictionary,FabricCLIError=CLIError,
        ItemType=types.ItemType,VirtualItemType=types.VirtualItemType,VirtualWorkspaceItemType=types.VirtualWorkspaceItemType,
        FabricElementType=types.FabricElementType))
    namespace=dict(
        FabricCLIError=CLIError,fab_constant=constants,
        ErrorMessages=SimpleNamespace(Hierarchy=SimpleNamespace(command_not_supported=lambda command:f"Not supported: {command}")),
        cmd=SimpleNamespace(**support),
    )
    Base,Item=policy_classes(namespace)
    class Workspace(Base):
        pass
    class Folder(Base):
        pass
    def item(spec: dict[str, Any]) -> Any:
        value=object.__new__(Folder if spec["type"]=="Folder" else Item)
        value.id=spec["id"];value.short_name=spec["name"]
        if isinstance(value,Folder):
            value.name=f"{spec['name']}.Folder";value.type=types.FabricElementType.FOLDER
        else:
            value.item_type=types.ItemType.from_string(spec["type"])
            value.name=f"{spec['name']}.{value.item_type}";value.type=types.FabricElementType.ITEM
        return value
    workspace=object.__new__(Workspace)
    workspace.id="workspace";workspace.name="Team.Workspace"
    items=[item(spec) for spec in case.get("items",[])]
    workspaces=[]
    for spec in case.get("workspaces",[]):
        value=object.__new__(Workspace);value.id=spec["id"];value.name=f"{spec['name']}.Workspace"
        workspaces.append(value)
    def confirm(text: str="Are you sure?") -> bool:
        prompts.append(text);return case.get("confirm",True)
    def select(prompt: str,names: list[str]) -> list[str]:
        choices.append(dict(prompt=prompt,names=names));return case["selected"]
    ui=SimpleNamespace(
        print_grey=lambda text:notices.append(dict(kind="progress",message=text)),
        print_warning=lambda text:notices.append(dict(kind="warning",message=text)),
        print=lambda text:notices.append(dict(kind="progress",message=text)),
        prompt_confirm=confirm,prompt_select_items=select,
        print_output_format=lambda args,message=None,**_:outputs.append(message),
    )
    def request(args: argparse.Namespace, **_: Any) -> SimpleNamespace:
        query=getattr(args,"request_params",{}) or {}
        url="https://api.fabric.microsoft.com/v1/"+args.uri
        if query:url+="?"+urlencode(query)
        requests.append(dict(method=args.method,url=url))
        reply=responses.popleft()
        if reply["status"]==403:raise CLIError("denied","Forbidden")
        return SimpleNamespace(status_code=reply["status"])
    base=dict(Namespace=argparse.Namespace,json=json,FabricCLIError=CLIError,constant=constants,
        fab_constant=constants,utils_ui=ui,Command=command,Workspace=Workspace,Item=Item,Folder=Folder)
    api_utils=load("client/fab_api_utils.py",dict(base,fabric_api=SimpleNamespace(do_request=request)))
    ws_api=load("client/fab_api_workspace.py",dict(base,fabric_api=SimpleNamespace(do_request=request),api_utils=api_utils))
    item_api=load("client/fab_api_item.py",dict(base,fabric_api=SimpleNamespace(do_request=request),api_utils=api_utils))
    sorting=functions("utils/fab_item_util.py",{"_get_elem_type","sort_ws_elems_by_config"},dict(
        Item=Item,fab_constant=constants,fab_state_config=SimpleNamespace(get_config=lambda _:case.get("sort","byname"))
    ))
    def catalog_items(_: Any) -> list[Any]:
        reads.append("items");return items
    def catalog_workspaces(_: Any) -> list[Any]:
        reads.append("workspaces");return workspaces
    module=load("commands/fs/rm/fab_fs_rm_workspace.py",dict(
        base,workspace_api=ws_api,item_api=item_api,rm_utils=load("utils/fab_cmd_rm_utils.py",{}),
        item_utils=SimpleNamespace(**sorting),utils_mem_store=SimpleNamespace(
            get_workspace_items=catalog_items,get_workspaces=catalog_workspaces,
            delete_workspace_from_cache=lambda item:cache.append(item.name),
            delete_item_from_cache=lambda item:cache.append(item.name),
        ),
    ))
    args=argparse.Namespace(force=case.get("force",False),hard=case.get("hard",False),request_params={})
    error=None
    try:
        if case["command"]=="tenant":module.bulk(SimpleNamespace(),args,args.force)
        else:module.single(workspace,args,args.force)
    except CLIError as failure:error=failure.status_code
    if responses:raise AssertionError(json.dumps({"unused_original_responses":len(responses),"case":case,"choices":choices,"error":error},indent=2))
    return dict(requests=requests,notices=notices,prompts=prompts,choices=choices,
                outputs=outputs,cache=cache,catalog_reads=reads,error=error)


def main() -> None:
    """Compare original support/selection/remove traces with actual Rust HTTP output."""
    parser=argparse.ArgumentParser();parser.add_argument("--probe",required=True);args=parser.parse_args()
    cases:list[dict[str,Any]]=[]
    entries=[
        dict(name="zeta",type="Notebook",id="n"),
        dict(name="Alpha",type="Report",id="r"),
        dict(name="alpha",type="CopyJob",id="c"),
        dict(name="alpha",type="GraphQLApi",id="g"),
        dict(name="selectable",type="Dashboard",id="d"),
        dict(name="Folder",type="Folder",id="f"),
    ]
    selected=["zeta.Notebook","Alpha.Report","alpha.CopyJob","alpha.GraphQLApi","selectable.Dashboard","Folder.Folder"]
    for sort in ["byname","bytype"]:
        for hard in [False,True]:
            for selection,confirm in [([],True),(selected,False),(selected,True),(["Folder.Folder"],True)]:
                count=4 if selection==selected and confirm else 0
                cases.append(dict(command="workspace",items=entries,selected=selection,confirm=confirm,sort=sort,hard=hard,
                                  responses=[dict(status=status) for status in [200,202,204,201][:count]]))
    for force in [False,True]:
        cases.append(dict(command="workspace",items=[],selected=[],force=force,responses=[dict(status=202)] if force else []))
    cases.append(dict(command="workspace",items=[entries[4]],selected=selected,responses=[]))
    for index,kind in enumerate(load("core/fab_types.py",dict(Enum=enum.Enum,auto=enum.auto,List=list)).ItemType):
        rejected=kind.value.lower() in {row["resource"].replace("_","").lower() for row in support_rows() if row["command"]=="rm" and row["gate"]=="unsupported_items"}
        cases.append(dict(command="workspace",items=[dict(name="Chosen",type=kind.value,id=f"item{index}")],selected=[f"Chosen.{kind.value}"],
                          responses=[] if rejected else [dict(status=200)]))
    cases.append(dict(command="workspace",items=entries[:2],selected=selected,responses=[dict(status=202),dict(status=403)]))
    for force in [False,True]:
        for selection,confirm in [([],True),(["B.Workspace"],False),(["B.Workspace","a.Workspace"],True)]:
            cases.append(dict(command="tenant",workspaces=[dict(name="a",id="a"),dict(name="B",id="b"),dict(name="A",id="A")],
                              selected=selection,confirm=confirm,force=force,responses=[dict(status=202),dict(status=200)] if selection and confirm else []))
    cases.append(dict(command="tenant",workspaces=[dict(name="A",id="A"),dict(name="B",id="B")],selected=["A.Workspace","B.Workspace"],
                      responses=[dict(status=202),dict(status=403)]))
    expected=[reference(case) for case in cases]
    run=subprocess.run([args.probe],input="".join(json.dumps(case)+"\n" for case in cases),text=True,encoding="utf-8",capture_output=True,check=True)
    actual=[json.loads(line) for line in run.stdout.splitlines()]
    if len(actual)!=len(expected):raise AssertionError("Wrong removal consumer count")
    for case,old,new in zip(cases,expected,actual,strict=True):
        if old!=new:raise AssertionError(json.dumps(dict(case=case,python=old,rust=new),indent=2))
    print(json.dumps(dict(source=PIN,removal_cases=len(cases),result="passed")))


if __name__=="__main__":main()

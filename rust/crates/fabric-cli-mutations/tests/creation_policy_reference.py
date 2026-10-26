# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Full pinned item-type create policy/help/collection matrix, not name coverage."""

from __future__ import annotations

import argparse
import enum
import json
import os
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from branch_ledger import support_rows
from creation_reference import verify_templates
from operation_reference import functions
from python_reference import CLIError, PIN, load, reference
from removal_reference import policy_classes


def support_checker()->Any:
    """Execute original support conversion and hierarchy methods, never Rust lists."""
    constants=SimpleNamespace(ERROR_INVALID_ITEM_TYPE="InvalidItemType",ERROR_UNSUPPORTED_COMMAND="UnsupportedCommand")
    common=load("errors/common.py",{})
    hierarchy=load("errors/hierarchy.py",{})
    types=load("core/fab_types.py",dict(Enum=enum.Enum,auto=enum.auto,List=list,FabricCLIError=CLIError,
        fab_constant=constants,ErrorMessages=SimpleNamespace(Common=common.CommonErrors)))
    gates=[row for row in support_rows() if row["command"]=="mkdir"]
    dictionary={"commands":{"fs":{"subcommands":{"mkdir":{
        key:[row["resource"] for row in gates if row["gate"]==key]
        for key in ("unsupported_items","supported_items","supported_elements")
    }}}}}
    support=functions("core/fab_commands.py",{
        "_snake_to_camel","_get_property_from_commands_and_subcommands","_get_items",
        "get_supported_elements","get_supported_items","get_unsupported_items",
    },dict(get_command_support_dict=lambda:dictionary,FabricCLIError=CLIError,
        ItemType=types.ItemType,VirtualItemType=types.VirtualItemType,VirtualWorkspaceItemType=types.VirtualWorkspaceItemType,
        FabricElementType=types.FabricElementType))
    namespace=dict(FabricCLIError=CLIError,fab_constant=constants,
        ErrorMessages=SimpleNamespace(Hierarchy=hierarchy.HierarchyErrors),cmd=SimpleNamespace(**support))
    _,item_class=policy_classes(namespace)
    def check(kind:str)->None:
        item=object.__new__(item_class)
        item.item_type=types.ItemType.from_string(kind)
        item.type=types.FabricElementType.ITEM
        item.check_command_support(SimpleNamespace(name="FS_MKDIR",value="mkdir"))
    return check


def main()->None:
    """Compare every source type, help/duplicate priority, status and nested params."""
    parser=argparse.ArgumentParser();parser.add_argument("--probe",required=True)
    parser.add_argument("--source-export",required=True);args=parser.parse_args()
    root=Path(args.source_export).resolve();verify_templates(root)
    os.environ["FABRIC_PARITY_TEMPLATE_SOURCE"]=str(root)
    types=load("core/fab_types.py",dict(Enum=enum.Enum,auto=enum.auto,List=list))
    check=support_checker()
    lower=load("utils/fab_cmd_mkdir_utils.py",{}).lowercase_keys
    cases:list[dict[str,Any]]=[]
    extra={"DisplayName":"ignored","Type":"ignored","Description":"desc","Custom":[{"UPPER":{"MixedKey":1}},[{"DeepKey":True}]]}
    for kind in types.ItemType:
        base={"command":"create","kind":kind.value,"capture_creation":True}
        cases.append(dict(base,params={}))
        cases.append(dict(base,params={},id="existing"))
        params=dict(extra,run="true")
        if kind.value=="KQLDatabase":params["eventhouseId"]="eventhouse"
        if kind.value=="DigitalTwinBuilderFlow":params["digitalTwinBuilderId"]="builder"
        if kind.value=="Report":params["semanticModelId"]="semantic-model"
        if kind.value=="MountedDataFactory":
            params.update(subscriptionId="sub",resourceGroup="group",factoryName="factory")
        for status in [200,201,202,204]:
            cases.append(dict(base,params=params,responses=[dict(status=status,data={"id":"created"})],source_parent="folder"))
        cases.append(dict(base,params=params,id="existing",responses=[]))
    for key in ["subscriptionId","resourceGroup","factoryName"]:
        for value in [None,"",False,0,[],{}]:
            params=dict(subscriptionId="sub",resourceGroup="group",factoryName="factory")
            params[key]=value
            cases.append(dict(command="create",kind="MountedDataFactory",params=params,capture_creation=True,
                responses=[] if value is None else [dict(status=201,data={"id":"created"})]))
    # Reach source lowercase recursion through actual payload values, not a detached
    # pure-helper equality that would miss the command's request/output consumer.
    cases.extend([
        dict(command="create",kind="Workspace",params={"run":"true","CUSTOM":extra["Custom"]},capture_creation=True,
             responses=[dict(status=201,data={"id":"created"})]),
        dict(command="create",kind="MirroredDatabase",params={"mirrorType":"snowflake","database":extra["Custom"]},
             capture_creation=True,responses=[dict(status=201,data={"id":"created"})]),
        dict(command="create",kind="Report",params={"semanticModelId":extra["Custom"]},capture_creation=True,
             responses=[dict(status=201,data={"id":"created"})]),
        dict(command="create",kind="MountedDataFactory",params={"subscriptionId":extra["Custom"],"resourceGroup":"group","factoryName":"factory"},
             capture_creation=True,responses=[dict(status=201,data={"id":"created"})]),
    ])
    expected=[]
    for case in cases:
        try:
            if case["kind"]!="Workspace":check(case["kind"])
        except CLIError as failure:
            expected.append(dict(requests=[],outputs=[],error=failure.status_code,cache=[],notices=[]))
        else:
            normalized=dict(case,params=lower(case["params"]))
            expected.append(reference(normalized))
    run=subprocess.run([args.probe],input="".join(json.dumps(case)+"\n" for case in cases),
        text=True,encoding="utf-8",capture_output=True,check=True)
    actual=[json.loads(line) for line in run.stdout.splitlines()]
    for case,old,new in zip(cases,expected,actual,strict=True):
        if old!=new:raise AssertionError(json.dumps(dict(case=case,python=old,rust=new),indent=2))
    print(json.dumps(dict(source=PIN,creation_policy_cases=len(cases),source_item_types=len(types.ItemType),result="passed")))


if __name__=="__main__":
    main()

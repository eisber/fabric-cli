# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Additional ACL export intents and OneLake handled-error source boundaries."""

import argparse
import json
from types import SimpleNamespace

from reference import HERE,SOURCE,SourceError,execute,inputs,source_module


class ServiceError(SourceError):
    """Synthetic structured Fabric service error, distinct from CLI auth errors."""


def disabled(command:str,code:str)->dict:
    """Execute the actual root dispatch and OneLake error branch."""
    warnings,diagnostics,printed,requests=[],[],[],[]
    workspace=type("Workspace",(SimpleNamespace,),{})
    item=type("Item",(SimpleNamespace,),{})
    virtual=type("VirtualWorkspaceItem",(SimpleNamespace,),{})
    onelake=type("OneLakeItem",(SimpleNamespace,),{})
    context=onelake(workspace=workspace(id="workspace",name="Workspace.Workspace"),
        item=item(id="resource",name="Resource.Lakehouse",item_type="Lakehouse"))
    args=argparse.Namespace(long=False,query=".",output=None,command_path=f"acl {command}",output_format="json")

    def request(args:argparse.Namespace)->None:
        requests.append({"method":args.method.upper(),"url":f"https://api.fabric.microsoft.com/v1/{args.uri}"})
        if code=="BadRequest":raise ServiceError("source service message",code)
        raise SourceError("Access is forbidden. You do not have permission to access this resource",code)

    constants=SimpleNamespace(WARNING_FABRIC_ADMINISTRATOR="Requires Fabric administrator",
        WARNING_ONELAKE_RBAC_ENABLED="Requires data access roles enabled",ERROR_UNIVERSAL_SECURITY_DISABLED="UniversalSecurityDisabled")
    common={"json":json,"Workspace":workspace,"Item":item,"VirtualWorkspaceItem":virtual,"OneLakeItem":onelake,
        "ItemType":SimpleNamespace(LAKEHOUSE="Lakehouse"),"FabricCLIError":SourceError,"FabricAPIError":ServiceError,
        "fab_constant":constants,"fab_logger":SimpleNamespace(log_warning=warnings.append),
        "utils":SimpleNamespace(process_nargs=lambda v:v),
        "ErrorMessages":SimpleNamespace(Common=SimpleNamespace(universal_security_disabled=lambda name:f"Universal security is disabled for '{name}'")),
        "fab_ui":SimpleNamespace(print_grey=diagnostics.append,print_output_error=lambda error,*a,**kw:printed.append({"code":error.status_code,"message":error.message})),
        "fabric_api":SimpleNamespace(do_request=request)}
    api=source_module("client/fab_api_onelake.py",common)
    common["api_onelake"]=SimpleNamespace(**api)
    handler=source_module(f"commands/acls/fab_acls_{command}.py",common)
    raised=None
    try:handler["exec_command"](args,context)
    except SourceError as error:raised={"code":error.status_code,"message":error.message}
    return {"requests":requests,"error":printed[0] if printed else raised,"handled":bool(printed),"warnings":warnings,"diagnostics":diagnostics}


def main()->None:
    """Capture additive boundaries without changing earlier protected expectations."""
    cases=[]
    base=next(c for c in inputs() if c["name"]=="workspace-get-.")
    for name,query,body in [
        ("raw",".",base["body"]),("paths",None,base["body"]),("scalar","[0].role",base["body"]),
        ("empty",".",{"value":[]}),("falsey-projection","`[]`",base["body"]),
    ]:
        case={**base,"name":f"export-{name}","query":query,"body":body,"output":"explicit-destination"}
        cases.append({**case,"expected":execute(case)})
    document={"source_commit":SOURCE,"exports":cases,"errors":[
        {"command":command,"code":code,"expected":disabled(command,code)}
        for command in ("ls","get") for code in ("BadRequest","Forbidden")]}
    parser=argparse.ArgumentParser()
    parser.add_argument("--initial-capture",action="store_true")
    args=parser.parse_args()
    path=HERE / "boundaries.json"
    if args.initial_capture:
        if path.exists():raise SystemExit("Refusing to overwrite ACL boundary oracle")
        path.write_text(json.dumps(document,indent=2)+"\n",encoding="utf-8")
    else:assert document==json.loads(path.read_text(encoding="utf-8")),"ACL boundary expectations changed"
    print("ACL source boundaries: 5 export intents, 4 OneLake error branches passed")


if __name__=="__main__":main()

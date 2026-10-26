# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Original do_request query encoding vs public resource/shared-operation consumer."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from urllib.parse import urlencode,urlparse

# Reuse development-only original-source loaders, not a production CLI dependency.
sys.path.insert(0,str(Path(__file__).resolve().parents[2] / "fabric-cli-mutations" / "tests"))
from error_reference import constants,selected
from operation_reference import Headers
from python_reference import PIN,load


def reference(case:dict[str,Any])->dict[str,Any]:
    """Execute the original request construction/auth/request bodies at the I/O seam."""
    requests:list[dict[str,Any]]=[]
    values=constants({"API_ENDPOINT_FABRIC","API_VERSION_FABRIC","SCOPE_FABRIC_DEFAULT"})
    const=SimpleNamespace(**values)
    class Credential:
        def get_access_token(self,scopes:list[str])->str:
            if scopes!=values["SCOPE_FABRIC_DEFAULT"]:raise AssertionError("Wrong original audience")
            return "synthetic-query-oracle"
    def request(method:str,url:str,**kwargs:Any)->Any:
        requests.append(dict(method=method,url=url,body=json.loads(kwargs["data"]) if "data" in kwargs else None))
        return SimpleNamespace(status_code=case["status"],text="",content=b"",headers=Headers({}))
    api_type=load("client/fab_api_types.py",dict(json=json,CaseInsensitiveDict=Headers)).ApiResponse
    logger=SimpleNamespace(**{key:lambda *_:None for key in [
        "log_debug","log_debug_http_request","log_debug_http_response","log_debug_http_request_exception",
    ]})
    original=selected("client/fab_api_client.py",{"do_request"},dict(
        Namespace=argparse.Namespace,json=json,urlparse=urlparse,fab_constant=const,fab_logger=logger,
        requests=SimpleNamespace(compat=SimpleNamespace(urlencode=urlencode),RequestException=type("SyntheticRequestError",(Exception,),{})),
        time=SimpleNamespace(time=lambda:0),
        _get_session=lambda:SimpleNamespace(request=request),
        _transform_workspace_url_for_private_link_if_needed=lambda url,**_:url,
        _build_user_agent=lambda _:"synthetic",FabAuth=Credential,
        FabContext=lambda:SimpleNamespace(command="set",fabric_skill=None),ApiResponse=api_type,
        _handle_fab_long_running_op=lambda response:response,
        _handle_successful_response=lambda args,response:response,
    ))
    query=dict(case["query"])
    if case.get("hard_first"):query["hardDelete"]="true"
    # Reproduce explicit builder updates as repeated original dispatches on the
    # same dictionary; only the final request is compared, not fabricated paging.
    tokens=case["tokens"] or [None]
    for token in tokens:
        args=argparse.Namespace(uri="/".join(case["segments"]),method=case["method"],request_params=query)
        original["do_request"](args,data=json.dumps(case["body"]) if case["body"] is not None else None,
                               continuation_token=token)
    if case.get("hard_last"):
        args.uri+="?hardDelete=true"
        original["do_request"](args,data=json.dumps(case["body"]) if case["body"] is not None else None)
    return dict(requests=[requests[-1]],status=case["status"])


def main()->None:
    """Compare opaque, repeated/overwritten query and actual domain PATCH/POST routes."""
    parser=argparse.ArgumentParser();parser.add_argument("--probe",required=True);args=parser.parse_args()
    cases=[]
    for method,segments,body in [
        ("patch",["admin","domains","domain"],{"displayName":"Updated"}),
        ("post",["admin","domains","domain","unassignWorkspaces"],{"workspacesIds":["workspace"]}),
        ("get",["admin","domains","domain"],None),
        ("delete",["workspaces","workspace","items","item"],None),
    ]:
        for tokens in [[""],["normal"],[" *~+/%2A?&#=日😀"],["https://other.invalid/a?x=y#fragment"],
            ["first","last"],["first",""],["%25literal%2F"],["a"*8192]]:
            for query in [[],[["continuationToken","existing"]],[["before","value"],["continuationToken","old"],["before","last"],
                ["continuationToken","duplicate"],["other","literal%25"],["ContinuationToken","case-sensitive"]]]:
                for status in [200,202,204]:
                    cases.append(dict(method=method,segments=segments,body=body,query=query,tokens=tokens,status=status))
    for first,last in [(True,False),(False,True)]:
        cases.append(dict(method="delete",segments=["workspaces","workspace","items","item"],body=None,
            query=[["hardDelete","false"],["existing","unchanged"]],tokens=[" *~+&"],status=202,hard_first=first,hard_last=last))
    expected=[reference(case) for case in cases]
    run=subprocess.run([args.probe],input="".join(json.dumps(case)+"\n" for case in cases),
        text=True,encoding="utf-8",capture_output=True,check=True)
    actual=[json.loads(line) for line in run.stdout.splitlines()]
    for case,old,new in zip(cases,expected,actual,strict=True):
        if old!=new:raise AssertionError(json.dumps(dict(case=case,python=old,rust=new),indent=2))
    print(json.dumps(dict(source=PIN,continuation_request_cases=len(cases),result="passed")))


if __name__=="__main__":
    main()

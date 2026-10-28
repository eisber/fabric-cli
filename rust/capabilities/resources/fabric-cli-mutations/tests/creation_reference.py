# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute pinned dependency/template create handlers; compare real public Rust consumer."""

from __future__ import annotations

import argparse
import os
import subprocess
import json
from pathlib import Path
from typing import Any

from python_reference import PIN, ROOT, reference
from template_listing_inputs import bind_listings


def verify_templates(root: Path) -> None:
    """Verify the original helper's template input bytes against the exact git pin."""
    prefix="src/fabric_cli/commands/fs/payloads"
    paths=subprocess.check_output(["git","ls-tree","-r","--name-only",PIN,"--",prefix],cwd=ROOT,text=True).splitlines()
    embedded=ROOT / "rust" / "crates" / "fabric-cli-mutations" / "templates"
    checked=0
    for path in paths:
        source=subprocess.check_output(["git","show",f"{PIN}:{path}"],cwd=ROOT)
        if (root / path).read_bytes()!=source:
            raise AssertionError(f"Original template export differs from pin: {path}")
        copied=embedded / path[len(prefix)+1:]
        if copied.exists():
            if copied.read_bytes()!=source:
                raise AssertionError(f"Native embedded template differs from pin: {path}")
            checked+=1
    if not checked:
        raise AssertionError("No native templates were checked")


def main() -> None:
    """Compare creation dependencies, raw definition bytes, payloads, UI and cache effects."""
    parser=argparse.ArgumentParser()
    parser.add_argument("--probe",required=True)
    parser.add_argument("--source-export",required=True)
    args=parser.parse_args()
    root=Path(args.source_export).resolve()
    verify_templates(root)
    os.environ["FABRIC_PARITY_TEMPLATE_SOURCE"]=str(root)
    cases:list[dict[str,Any]]=[]
    for item_type,params in [
        ("SemanticModel",{"run":"true"}),
        ("MirroredDatabase",{"run":"true"}),
        ("MirroredDatabase",{"mirrorType":"genericmirror","connectionId":"ignored","defaultSchema":"ignored"}),
        ("Lakehouse",{"enableSchemas":"TRUE"}),
        ("Warehouse",{"enableCaseInsensitive":"true"}),
        ("KQLDatabase",{"eventhouseId":"eh"}),
        ("KQLDatabase",{"eventhouseId":"eh","dbType":"sHoRtCuT","clusterUri":"https://synthetic.example","databaseName":"database"}),
        ("DigitalTwinBuilderFlow",{"digitalTwinBuilderId":"builder"}),
    ]:
        for parent in ["root","folder"]:
            cases.append(dict(kind=item_type,command="create",params=dict(params,Description="notes"),source_parent=parent,
                              capture_creation=True,responses=[dict(status=201,data={"id":"root-id"})]))
    for item_type,params in [
        ("KQLDatabase",{"run":"true"}),
        ("KQLDatabase",{"eventhouseId":"ignored","dbType":"unknown"}),
        ("KQLDatabase",{"eventhouseId":"ignored","dbType":""}),
        ("KQLDatabase",{"eventhouseId":"ignored","dbType":"shortcut","clusterUri":"missing database"}),
        ("DigitalTwinBuilderFlow",{"run":"true"}),
        ("DigitalTwinBuilderFlow",{"digitalTwinBuilderId":""}),
    ]:
        for parent in ["root","folder"]:
            for child,root_reply in [
                (dict(status=201,data={"id":"dependency-id"}),dict(status=201,data={"id":"root-id"})),
                (dict(status=200,data={}),dict(status=200,data={})),
                (dict(status=202,data={}),dict(status=201,data={"id":"root-id"})),
                (dict(status=201,data={"id":"dependency-id"}),dict(status=202,data={})),
                (dict(status=201,data={"id":"dependency-id"}),dict(status=403,error="Forbidden")),
            ]:
                cases.append(dict(kind=item_type,command="create",params=dict(params,Description="inherited description"),
                                  source_parent=parent,capture_creation=True,responses=[child,root_reply]))
        cases.append(dict(kind=item_type,command="create",params=params,capture_creation=True,
                          responses=[dict(status=403,error="Forbidden")]))
    for case in cases:
        bind_listings(case)
    expected=[reference(case) for case in cases]
    run=subprocess.run([args.probe],input="".join(json.dumps(case)+"\n" for case in cases),
                       text=True,encoding="utf-8",capture_output=True,check=True)
    actual=[json.loads(line) for line in run.stdout.splitlines()]
    if len(actual)!=len(expected):
        raise AssertionError("Wrong number of creation consumer outputs")
    for case,old,new in zip(cases,expected,actual,strict=True):
        if old!=new:
            raise AssertionError(json.dumps(dict(case=case,python=old,rust=new),indent=2))
    from template_enumeration_reference import compare
    compare(args.probe)
    print(json.dumps(dict(source=PIN,creation_cases=len(cases),result="passed")))


if __name__=="__main__":
    main()

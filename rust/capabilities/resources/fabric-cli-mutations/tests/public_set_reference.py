# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Compare unchanged original SET helper bodies to the public external Rust seam."""

from __future__ import annotations

import argparse
import copy
import json
import subprocess
from typing import Any

import jmespath

from python_reference import PIN, load


def main() -> None:
    """Check raw-key projection, native JSON values, index paths and failure cases."""
    parser=argparse.ArgumentParser()
    parser.add_argument("--probe",required=True)
    args=parser.parse_args()
    if jmespath.__version__!="1.0.1":
        raise AssertionError("Original development query evaluator must be 1.0.1")
    original_jmes=load("utils/fab_jmespath.py",dict(jmespath=jmespath))
    original=load("utils/fab_cmd_set_utils.py",dict(json=json,utils_jmespath=original_jmes))
    cases:list[dict[str,Any]]=[]
    for query in ["displayName","properties.name","properties.values[0]","properties.values[-1]","values[0]"]:
        for value in ["plain input","null","false","1.0",'{"nested":[1,true,null]}','"Unicode \\u65e5"']:
            cases.append(dict(metadata={"displayName":"old","properties":{"name":"old","values":[1,2],"keep":True},"values":[1,2]},query=query,input=value))
    cases.extend([
        dict(metadata={},query="missing.child",input="x"),
        dict(metadata={"values":[1]},query="values[9]",input="x"),
        dict(metadata={"properties":{}},query="properties.new",input="18446744073709551615"),
    ])
    expected=[]
    for case in cases:
        try:
            value=original.update_fabric_element(copy.deepcopy(case["metadata"]),case["query"],case["input"])
            expected.append(dict(value=value,error=None))
        except (ValueError,KeyError,IndexError,TypeError):
            expected.append(dict(value=None,error="InvalidQuery"))
    run=subprocess.run([args.probe],input="".join(json.dumps(case)+"\n" for case in cases),
        text=True,encoding="utf-8",capture_output=True,check=True)
    actual=[json.loads(line) for line in run.stdout.splitlines()]
    if len(actual)!=len(expected):
        raise AssertionError("Wrong public SET consumer output count")
    for case,old,new in zip(cases,expected,actual,strict=True):
        if old!=new:
            raise AssertionError(json.dumps(dict(case=case,python=old,rust=new),indent=2))
    print(json.dumps(dict(source=PIN,public_set_cases=len(cases),result="passed")))


if __name__=="__main__":
    main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Additive source edge observations, including unhandled malformed-row exceptions."""

import argparse
import copy
import json

from reference import HERE,SOURCE
from mutations_reference import execute,inputs


def edge_inputs()->list[dict]:
    """First-match/role/local-label edges, separate from existing expectations."""
    original=inputs()
    remove=copy.deepcopy(next(c for c in original if c["name"]=="workspace-rm-object"))
    first=copy.deepcopy(remove["responses"][0]["body"]["value"][0])
    first["id"]="first"
    second=copy.deepcopy(first)
    second["id"]="second"
    cases=[
        {**copy.deepcopy(remove),"name":"duplicate-first-match","responses":[{"status":200,"body":{"value":[first,second]}},{"status":200,"body":{}}]},
        {**copy.deepcopy(remove),"name":"match-before-malformed","responses":[{"status":200,"body":{"value":[first,{}]}},{"status":200,"body":{}}]},
        {**copy.deepcopy(remove),"name":"missing-principal","responses":[{"status":200,"body":{"value":[{}]}}]},
        {**copy.deepcopy(remove),"name":"null-user-details","responses":[{"status":200,"body":{"value":[{"id":"assignment","principal":{"userDetails":None}}]}}]},
    ]
    role=copy.deepcopy(next(c for c in original if c["name"]=="workspace-set-invalid-role"))
    cases.append({**role,"name":"empty-role","role":""})
    label=copy.deepcopy(next(c for c in original if c["name"]=="label-set-ok"))
    cases.append({**copy.deepcopy(label),"name":"label-missing-labels-key","definitions":{}})
    cases.append({**copy.deepcopy(label),"name":"label-duplicate-first-match","definitions":{"labels":[{"name":"Public","id":"first"},{"name":"Public","id":"second"}]}})
    return cases


def observe(case:dict)->dict:
    """Retain source exceptions as observations, not successful fallback behavior."""
    try:return execute(case)
    except (KeyError,AttributeError,TypeError) as error:return {"python_exception":type(error).__name__}


def main()->None:
    """Compare immutable extra fixtures without rewriting the earlier source corpus."""
    document={"source_commit":SOURCE,"cases":[{**case,"expected":observe(case)} for case in edge_inputs()]}
    parser=argparse.ArgumentParser()
    parser.add_argument("--initial-capture",action="store_true")
    args=parser.parse_args()
    path=HERE / "mutation_edges.json"
    if args.initial_capture:
        if path.exists():raise SystemExit("Refusing to overwrite mutation edge oracle")
        path.write_text(json.dumps(document,indent=2)+"\n",encoding="utf-8")
    else:assert document==json.loads(path.read_text(encoding="utf-8")),"Mutation edge source expectations changed"
    print(f"ACL/label source edges: {len(document['cases'])} passed; malformed Python exceptions retained")


if __name__=="__main__":main()

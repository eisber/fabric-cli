# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independent source ACL/label write requests, confirmations and outputs."""

import argparse
import ast
import builtins
import copy
import json
import subprocess
from io import StringIO
from types import SimpleNamespace
from unittest.mock import patch
from typing import Any

from reference import HERE, ROOT, SOURCE, SourceError, source_module


def inputs() -> list[dict]:
    """Synthetic resource/identity/role/confirmation matrix, not Rust outputs."""
    cases = []
    principal = {"id":"object","displayName":"Alice","userDetails":{"userPrincipalName":"alice@example.invalid"},"servicePrincipalDetails":{"aadAppId":"app"}}
    row = {"id":"assignment","role":"Admin","principal":principal}
    for resource, role in [("workspace","viewer"),("gateway","admin"),("connection","user")]:
        base = {"resource":resource,"role":role,"identity":"new","force":True,"confirmations":[]}
        scenarios = [
            ("add",[{"status":200,"body":{"value":[]}},{"status":201,"body":{}}],{}),
            ("probe",[{"status":200,"body":{"value":[]}},{"status":400,"body":{"errorCode":"PrincipalNotFound"}},{"status":400,"body":{"errorCode":"PrincipalNotFound"}},{"status":200,"body":{}}],{}),
            ("probe-exhausted",[{"status":200,"body":{"value":[]}}]+[{"status":400,"body":{"errorCode":"PrincipalNotFound"}}]*4,{}),
            ("update",[{"status":200,"body":{"value":[row]}},{"status":200,"body":{}}],{"identity":"object"}),
            ("substring-update",[{"status":200,"body":{"value":[row]}},{"status":200,"body":{}}],{"identity":"id"}),
            ("declined",[],{"force":False,"confirmations":[False]}),
            ("overwrite-declined",[{"status":200,"body":{"value":[row]}}],{"identity":"object","force":False,"confirmations":[True,False]}),
            ("invalid-role",[],{"role":"not-a-role"}),
        ]
        for name,responses,extra in scenarios:
            cases.append({**base,"name":f"{resource}-set-{name}","command":"set","responses":copy.deepcopy(responses),**extra})
        for identity in ("object","Alice","alice@example.invalid","app","missing"):
            responses = [{"status":200,"body":{"value":[row]}}]
            if identity != "missing":
                responses.append({"status":200,"body":{}})
            cases.append({**base,"name":f"{resource}-rm-{identity}","command":"rm","identity":identity,"responses":responses})
    for command in ("set","rm"):
        for name,extra in [
            ("ok",{}),("declined",{"force":False,"confirmations":[False]}),
            ("unset",{"configured":False}),("unknown-name",{"label_name":"Unknown"}),
        ]:
            cases.append({"name":f"label-{command}-{name}","resource":"label","command":command,
                "force":True,"confirmations":[],"configured":True,"label_name":"Public",
                "responses":[{"status":200,"body":{}}],"definitions":{"labels":[{"name":"Public","id":"label"}]},**extra})
    return cases


def execute(case: dict) -> dict:
    """Execute actual source route, role validation, identity and handler functions."""
    requests, messages, diagnostics, warnings, questions = [], [], [], [], []
    replies = iter(case["responses"])
    confirmations = iter(case["confirmations"])
    args = argparse.Namespace(identity=case.get("identity"),role=case.get("role"),force=case["force"],name=case.get("label_name"),input="synthetic",output_format="json")

    def do_request(args: argparse.Namespace, data: Any = None) -> SimpleNamespace:
        requests.append({"method":args.method.upper(),"url":f"https://api.fabric.microsoft.com/v1/{args.uri}","body":json.loads(data) if data else None})
        reply = next(replies)
        if reply["status"] >= 400:
            raise SourceError(reply["body"].get("message","An error occurred while processing the operation"),reply["body"].get("errorCode","UnknownError"))
        return SimpleNamespace(status_code=reply["status"],text=json.dumps(reply["body"]))

    def prompt(question: str = "Are you sure?") -> bool:
        questions.append(question)
        return next(confirmations)

    constants = SimpleNamespace(ERROR_INVALID_INPUT="InvalidInput",ERROR_NOT_FOUND="NotFound",ERROR_FORBIDDEN="Forbidden")
    ui = SimpleNamespace(prompt_confirm=prompt,print_grey=diagnostics.append,print_output_format=lambda args,**kw:messages.append(kw["message"]))
    common = {"json":json,"FabricCLIError":SourceError,"fab_constant":constants,"constant":constants,
        "fab_logger":SimpleNamespace(log_warning=warnings.append),"utils_ui":ui,
        "fabric_api":SimpleNamespace(do_request=do_request)}
    for binding,relative in [("api_workspaces","fab_api_workspace.py"),("api_gateway","fab_api_gateway.py"),("api_connection","fab_api_connection.py"),("api_labels","fab_api_labels.py")]:
        common[binding] = SimpleNamespace(**source_module(f"client/{relative}",common))
    helper = source_module("utils/fab_cmd_label_utils.py",common)
    common["utils_label"] = SimpleNamespace(get_label_id_by_name=helper["get_label_id_by_name"])
    context = SimpleNamespace(id="resource",name="Resource",item_type=SimpleNamespace(value="Notebook"))
    error = None
    try:
        if case["resource"] == "label":
            if case["configured"]:
                handler = source_module(f"commands/labels/fab_labels_{case['command']}.py",common)
                with patch.object(builtins,"open",lambda *a,**k:StringIO(json.dumps(case["definitions"]))):
                    handler[f"_{case['command']}_label_item"](context,args)
            else:
                warnings.append("Label definitions for CLI not set")
                diagnostics.append("\u2192 Run 'config set local_definition_labels <json_path>'")
        else:
            raw = subprocess.run(["git","show",f"{SOURCE}:src/fabric_cli/commands/acls/fab_acls_set.py"],cwd=ROOT,check=True,capture_output=True).stdout
            for node in ast.parse(raw.decode()).body:
                if isinstance(node,ast.Assign) and isinstance(node.targets[0],ast.Name) and node.targets[0].id.endswith(("_ROLES","_TYPES")):
                    common[node.targets[0].id] = ast.literal_eval(node.value)
            handler = source_module(f"commands/acls/fab_acls_{case['command']}.py",common)
            if case["command"] == "set":
                handler["_validate_role_by_type"](args.role,case["resource"].title())
            handler[f"_{case['command']}_acls_{case['resource']}"](context,args)
    except SourceError as exception:
        error = {"code":exception.status_code,"message":exception.message}
    return {"requests":requests,"message":messages[0] if messages else None,"diagnostics":diagnostics,"warnings":warnings,"questions":questions,"error":error}


def main() -> None:
    """Compare actual source behavior; initial capture cannot replace a fixture."""
    document = {"source_commit":SOURCE,"cases":[{**case,"expected":execute(case)} for case in inputs()]}
    parser = argparse.ArgumentParser()
    parser.add_argument("--initial-capture",action="store_true")
    args = parser.parse_args()
    path = HERE / "mutation_cases.json"
    if args.initial_capture:
        if path.exists():
            raise SystemExit("Refusing to overwrite existing mutation oracle")
        path.write_text(json.dumps(document,indent=2)+"\n",encoding="utf-8")
    else:
        assert document == json.loads(path.read_text(encoding="utf-8")),"Mutation source observations changed"
    print(f"ACL/label mutation source: {len(document['cases'])} cases passed")


if __name__ == "__main__":
    main()

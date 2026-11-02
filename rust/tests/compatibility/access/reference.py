# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute pinned Python ACL handlers with synthetic API/UI boundaries.

No product module, credentials, network client or Rust code is imported.
The source modules are read from the authoritative Git object, not mutable main.
The oracle's inputs are shared with Rust; outputs come only from Python handlers.
"""

import __future__
import argparse
import ast
import copy
import hashlib
import json
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import jmespath

SOURCE = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]


def source_module(relative: str, bindings: dict[str, Any]) -> dict[str, Any]:
    """Load function definitions only from an exact Git object."""
    content = subprocess.run(
        ["git", "show", f"{SOURCE}:src/fabric_cli/{relative}"],
        cwd=ROOT, check=True, capture_output=True,
    ).stdout
    tree = ast.parse(content.decode("utf-8"))
    tree.body = [node for node in tree.body if isinstance(node, ast.FunctionDef)]
    for node in tree.body:
        node.decorator_list = []
    namespace = dict(bindings)
    exec(compile(tree, relative, "exec", flags=__future__.annotations.compiler_flag), namespace)
    return namespace


class SourceError(Exception):
    """Injected CLI error with the source output-facing fields."""

    def __init__(self, message: str, status_code: str) -> None:
        super().__init__(message)
        self.message = message.rstrip(".")
        self.status_code = status_code


def execute(case: dict[str, Any]) -> dict[str, Any]:
    """Run one actual source handler through API and output observation seams."""
    requests: list[dict[str, Any]] = []
    output: list[Any] = []
    warnings: list[str] = []
    diagnostics: list[str] = []
    exports: list[Any] = []
    args = argparse.Namespace(
        long=case.get("long", False), query=case.get("query"), output=case.get("output"),
        output_format="json", command_path=f"acl {case['command']}",
    )

    def do_request(args: argparse.Namespace, data: Any = None) -> SimpleNamespace:
        requests.append({"method": args.method.upper(), "url": f"https://api.fabric.microsoft.com/v1/{args.uri}"})
        response = case.get("response", {"status": 200, "body": case.get("body")})
        return SimpleNamespace(status_code=response["status"], text=json.dumps(response["body"]))

    def search(data: Any, expression: str) -> Any:
        return query_namespace["search"](data, expression)

    common = {
        "json": json, "FabricCLIError": SourceError, "FabricAPIError": SourceError,
        "fab_constant": SimpleNamespace(
            WARNING_FABRIC_ADMINISTRATOR="Requires Fabric administrator",
            WARNING_ONELAKE_RBAC_ENABLED="Requires data access roles enabled",
            ERROR_NOT_SUPPORTED="NotSupported", ERROR_INVALID_INPUT="InvalidInput",
            ERROR_UNIVERSAL_SECURITY_DISABLED="UniversalSecurityDisabled",
        ),
        "fab_logger": SimpleNamespace(log_warning=warnings.append),
        "fab_ui": SimpleNamespace(print_output_format=lambda args, **kw: output.append(kw["data"]), print_grey=diagnostics.append),
        "utils_ui": SimpleNamespace(print_output_format=lambda args, **kw: output.append(kw["data"])),
        "utils": SimpleNamespace(process_nargs=lambda value: " ".join(value).strip("\"'") if isinstance(value, list) else value),
        "utils_jmespath": SimpleNamespace(search=search),
        "utils_storage": SimpleNamespace(do_output=lambda **kw: exports.append({"data":kw["data"],"name":kw["file_name"]})),
        "fabric_api": SimpleNamespace(do_request=do_request),
        "ErrorMessages": SimpleNamespace(Common=SimpleNamespace(invalid_jmespath_query=lambda: "Invalid jmespath query (https://jmespath.org)")),
        "jmespath": jmespath,
    }
    query_namespace = source_module("utils/fab_jmespath.py", common)
    formatter = source_module("utils/fab_cmd_ls_utils.py", {**common, "utils_ui": common["utils_ui"], "fab_util":common["utils"]})
    common["utils_ls"] = SimpleNamespace(format_and_print_output=formatter["format_and_print_output"])
    for binding, relative in [
        ("api_workspaces", "fab_api_workspace.py"), ("api_items", "fab_api_item.py"),
        ("api_gateways", "fab_api_gateway.py"), ("api_connections", "fab_api_connection.py"),
        ("api_onelake", "fab_api_onelake.py"),
    ]:
        api = SimpleNamespace(**source_module(f"client/{relative}", common))
        common[binding] = api
        common[binding.removesuffix("s") if binding.endswith(("gateways","connections")) else binding] = api
    namespace = source_module(f"commands/acls/fab_acls_{case['command']}.py", common)
    resource = case["resource"]
    context = SimpleNamespace(id="resource", name="Resource", workspace=SimpleNamespace(id="workspace",name="Workspace"))
    if resource == "workspace":
        function = "_ls_acls_workspace" if case["command"] == "ls" else "_get_acls_workspace"
    elif resource in ("gateway","connection","item"):
        function = f"_{'ls' if case['command']=='ls' else 'get'}_acls_{resource}"
    else:
        context.item = SimpleNamespace(id="resource",name="Resource")
        function = f"_{'ls' if case['command']=='ls' else 'get'}_acls_onelake"
    error = None
    try:
        namespace[function](context, args)
    except SourceError as exception:
        error = {"code":exception.status_code,"message":exception.message}
    return {"requests":requests,"data":output[0] if output else None,"error":error,"exports":exports}


def inputs() -> list[dict[str, Any]]:
    """Synthetic cases independent of Rust implementation."""
    principal = {"id":"object","displayName":"Alice","type":"User","userDetails":{"userPrincipalName":"alice@example.invalid"}}
    group = {"id":"group","displayName":"Team","type":"Group","groupDetails":{"groupType":"SecurityGroup"}}
    workspace = {"value":[{"id":"assignment-b","role":"Viewer","principal":principal},{"id":"assignment-a","role":"Admin","principal":group}]}
    virtual = {"value":[{"id":"assignment-b","role":"User","principal":principal},{"id":"assignment-a","role":"Owner","principal":group}]}
    item = {"accessDetails":[{"principal":principal,"itemAccessDetails":{"permissions":["Read","Write"]}},{"principal":group,"itemAccessDetails":{"permissions":["Read"]}}]}
    onelake = {"value":[{"name":"ReadAll","decisionRules":[{"effect":"Permit"}],"members":{"microsoftEntraMembers":[{"objectId":"object"}],"fabricItemMembers":[{"sourcePath":"/source"}]}}]}
    cases = []
    for resource, body in [("workspace",workspace),("gateway",virtual),("connection",virtual),("item",item),("onelake",onelake)]:
        for command, long, query in [("ls",False,None),("ls",True,None),("ls",False,"."),("get",False,"."),("get",False,None),("get",False,"[0]")]:
            cases.append({"name":f"{resource}-{command}-{'long' if long else query or 'default'}","resource":resource,"command":command,"long":long,"query":query,"body":copy.deepcopy(body)})
        field = "accessDetails" if resource == "item" else "value"
        cases.append({"name":f"{resource}-empty-get","resource":resource,"command":"get","query":".","body":{field:[]}})
        if resource != "onelake":
            cases.append({"name":f"{resource}-empty-ls","resource":resource,"command":"ls","body":{field:[]}})
    return cases


def main() -> None:
    """Compare pinned source observations with the immutable initial capture."""
    document = {"source_commit":SOURCE,"cases":[]}
    for case in inputs():
        document["cases"].append({**case,"expected":execute(case)})
    parser = argparse.ArgumentParser()
    parser.add_argument("--initial-capture", action="store_true")
    args = parser.parse_args()
    if args.initial_capture:
        if (HERE / "cases.json").exists():
            raise SystemExit("Refusing to overwrite an existing oracle")
        (HERE / "cases.json").write_text(json.dumps(document,ensure_ascii=True,indent=2)+"\n",encoding="utf-8")
    else:
        expected = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
        if document != expected:
            raise SystemExit("Access source observations differ from immutable fixtures")
        print(f"access source oracle: {len(document['cases'])} cases passed")
    print("cases SHA256:",hashlib.sha256((HERE / "cases.json").read_bytes()).hexdigest())


if __name__ == "__main__":
    main()

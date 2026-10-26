# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pinned whole hidden GET/name helpers with source pagination and ordered metadata."""

import __future__
import argparse
import ast
import copy
import json
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from urllib.parse import urlencode

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "access"))
from reference import ROOT, SOURCE, SourceError, source_module

HERE = Path(__file__).resolve().parent


def sources() -> dict:
    """Prepare actual source functions/classes before any synthetic observations."""
    common = {"json": json, "FabricCLIError": SourceError}
    raw = subprocess.run(["git", "show", f"{SOURCE}:src/fabric_cli/client/fab_api_types.py"],
        cwd=ROOT, check=True, capture_output=True).stdout
    tree = ast.parse(raw.decode())
    tree.body = [node for node in tree.body if isinstance(node, ast.ClassDef)]
    exec(compile(tree, "fab_api_types.py", "exec", flags=__future__.annotations.compiler_flag), common)
    return common


def inputs() -> list[dict]:
    """Independent source singleton, enrichment, query, warning and name cases."""
    result = []
    for resource in ("domain", "connection", "gateway"):
        for query, verbose in [(".", True), (None, True), (".", False)]:
            replies = [{"status": 200, "body": {"id": "resource", "displayName": "Primary", "opaque": {"z": 1, "a": 2}}}]
            if resource == "domain":
                replies.append({"status": 200, "body": {"value": [{"id": "workspace-a"}]}})
            result.append({"name": f"{resource}-{query or 'paths'}-{verbose}", "resource": resource,
                "query": query, "verbose": verbose, "replies": replies})
    for name, replies in [
        ("membership-paged", [{"status": 200, "body": {"value": [{"id": "a"}], "continuationToken": " +/~"}},
            {"status": 200, "body": {"value": [{"id": "b"}]}}]),
        ("membership-denied", [{"status": 403, "body": {"errorCode": "Forbidden", "message": "denied"}}]),
        ("membership-malformed", [{"status": 200, "body": {"not_value": []}}]),
        ("membership-empty", [{"status": 200, "body": {"value": []}}]),
    ]:
        result.append({"name": name, "resource": "domain", "query": ".", "verbose": True,
            "replies": [{"status": 200, "body": {"id": "resource", "displayName": "Domain"}}] + replies})
    for resource in ("domain", "connection", "gateway"):
        result.append({"name": f"{resource}-primary-error", "resource": resource, "query": ".", "verbose": True,
            "replies": [{"status": 403, "body": {"errorCode": "Forbidden", "message": "denied"}}]})
    return result


def observe(case: dict) -> dict:
    """Run the exact source handler, API routing and recursive response merging."""
    requests, outputs, warnings = [], [], []
    replies = iter(copy.deepcopy(case["replies"]))
    common = sources()
    common.update({"fab_constant": SimpleNamespace(DEBUG=False, WARNING_FABRIC_ADMINISTRATOR="Requires Fabric administrator"),
        "fab_logger": SimpleNamespace(log_warning=warnings.append)})
    client = source_module("client/fab_api_client.py", common)

    def request(args: argparse.Namespace, continuation_token: str | None = None) -> object:
        suffix = f"?{urlencode({'continuationToken': continuation_token})}" if continuation_token else ""
        requests.append({"method": args.method.upper(), "url": f"https://api.fabric.microsoft.com/v1/{args.uri}{suffix}"})
        reply = next(replies)
        if reply["status"] >= 400:
            message = "Access is forbidden. You do not have permission to access this resource" if reply["status"] == 403 else reply["body"].get("message")
            raise SourceError(message, reply["body"].get("errorCode"))
        response = common["ApiResponse"](reply["status"], json.dumps(reply["body"]), b"", {})
        return client["_handle_successful_response"](args, response)

    client["do_request"] = request
    for name in ("domain", "connection", "gateway"):
        common[f"{name}_api"] = SimpleNamespace(**source_module(f"client/fab_api_{name}.py",
            {**common, "fabric_api": SimpleNamespace(do_request=request)}))
    import jmespath
    query = source_module("utils/fab_jmespath.py", {**common, "jmespath": jmespath})
    get = source_module("utils/fab_cmd_get_utils.py", {**common,
        "utils_jmespath": SimpleNamespace(search=query["search"]),
        "utils_ui": SimpleNamespace(print_output_format=lambda args, **kw: outputs.append(kw["data"]))})
    common["utils_get"] = SimpleNamespace(query_and_export=get["query_and_export"])
    handler = source_module(f"commands/fs/get/fab_fs_get_{case['resource']}.py", common)
    value = None
    error = None
    args = argparse.Namespace(query=case["query"], output=None)
    context = SimpleNamespace(id="resource", short_name="Primary", name=f"Primary.{case['resource'].title()}")
    try:
        value = handler["exec"](context, args, case["verbose"])
    except SourceError as exception:
        error = {"code": exception.status_code, "message": exception.message}
    return {"requests": requests, "value": value, "data": outputs[0] if outputs else None,
        "warnings": warnings, "error": error}


def name_cases() -> list[dict]:
    """Exact source first-match/full-suffix, not a case-insensitive SDK redesign."""
    rows = [{"id": "first", "displayName": "Alpha"}, {"id": "second", "displayName": "Alpha"},
        {"id": "other", "displayName": "alpha"}]
    result = []
    for kind in ("Domain", "Connection", "Gateway"):
        for name in (f"Alpha.{kind}", f"/Alpha.{kind}/", f"alpha.{kind}", "Alpha", f"ALPHA.{kind}"):
            common = {"FabricCLIError": SourceError,
                "fab_constant": SimpleNamespace(ERROR_NOT_FOUND="NotFound"),
                "ErrorMessages": SimpleNamespace(Common=SimpleNamespace(resource_not_found=lambda r:
                    f"The {r['type']} '{r['name']}' could not be found"))}
            namespace = source_module("utils/fab_mem_store.py", common)
            namespace[f"get_{kind.lower()}s"] = lambda tenant: [
                SimpleNamespace(id=row["id"], name=f"{row['displayName']}.{kind}") for row in rows]
            error = None
            identifier = None
            try:
                identifier = namespace[f"get_{kind.lower()}_id"](None, name)
            except SourceError as exception:
                error = {"code": exception.status_code, "message": exception.message}
            result.append({"kind": kind, "name": name, "rows": rows, "id": identifier, "error": error})
    return result


def main() -> None:
    """Freeze additive source observations once; normal execution only verifies."""
    document = {"source_commit": SOURCE, "metadata": [{**case, "expected": observe(case)} for case in inputs()],
        "names": name_cases()}
    parser = argparse.ArgumentParser()
    parser.add_argument("--initial-capture", action="store_true")
    args = parser.parse_args()
    path = HERE / "bindings_cases.json"
    if args.initial_capture:
        if path.exists():
            raise SystemExit("Refusing to overwrite metadata bindings oracle")
        path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    else:
        assert document == json.loads(path.read_text(encoding="utf-8")), "Hidden read bindings source drift"
    print(f"hidden source bindings: {len(document['metadata'])} metadata and {len(document['names'])} name cases passed")


if __name__ == "__main__":
    main()

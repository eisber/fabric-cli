# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Whole family handlers and the actual shared Python UI, with offline collaborators."""

import argparse
import importlib.util
import io
import json
import sys
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("access_source_reference", HERE / "reference.py")
access = importlib.util.module_from_spec(spec)
spec.loader.exec_module(access)
sys.path.insert(0, str(HERE.parent / "output"))
from text_reference import TextReference


def inputs() -> list[dict]:
    """Source arguments and service data, independent of native rendering."""
    source = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
    cases = []
    for case in source["cases"]:
        if case["name"].endswith(("-ls-default", "-ls-long", "-get-default", "-get-.", "-empty-ls")):
            cases.append({"kind": "acl", **{key: case[key] for key in ("name", "resource", "command", "body")},
                "long": case.get("long", False), "query": case.get("query")})
    for name, labels in [("label-local", [{"name": "Public", "id": "label"}]), ("label-local-empty", [])]:
        cases.append({"name": name, "kind": "label-local", "labels": labels})
    for long in (False, True):
        cases.append({"name": f"find-{long}", "kind": "find", "long": long,
            "body": {"value": [{"id": "one", "displayName": "Report",
                "type": "Report", "workspaceName": "Workspace",
                "description": "A description long enough to require narrow column truncation"}]}})
    cases.append({"name": "label-remove", "kind": "label-remove"})
    cases.append({"name": "raw-api", "kind": "api", "body": {"value": [{"id": "one"}]}})
    cases.append({"name": "handled-security", "kind": "error", "handled": True,
        "code": "UniversalSecurityDisabled", "message": "Universal security is disabled for 'Lakehouse'",
        "diagnostics": ["\u2192 Run 'open /Workspace/Lakehouse' and enable it"]})
    cases.append({"name": "unauthorized", "kind": "error", "handled": False,
        "code": "Unauthorized", "message": "Access is unauthorized", "diagnostics": ["Searching..."]})
    return [{**case, "name": f"{case['name']}-{format}", "format": format, "width": 48}
        for case in cases for format in ("text", "json")]


def observe(case: dict) -> dict:
    """Execute source handlers and original formatting, recording ordered stream writes."""
    reference = TextReference({"name": case["name"], "width": case["width"], "format": case["format"], "input": {}})
    ns = reference.reference.namespace
    ns["fab_constant"].FAB_MODE_INTERACTIVE = "interactive"
    events = []

    class Stream(io.StringIO):
        def __init__(self, name: str) -> None:
            super().__init__()
            self.name = name

        def write(self, text: str) -> int:
            if text:
                if events and events[-1]["stream"] == self.name:
                    events[-1]["payload"] += text
                else:
                    events.append({"stream": self.name, "payload": text})
            return super().write(text)

    stdout, stderr = Stream("stdout"), Stream("stderr")
    ns["sys"].stdout, ns["sys"].stderr = stdout, stderr
    ui = SimpleNamespace(**{name: ns[name] for name in (
        "print_output_format", "print_output_error", "print_grey")})
    queries = access.source_module("utils/fab_jmespath.py", {
        "json": json, "jmespath": __import__("jmespath"),
        "FabricCLIError": ns["FabricCLIError"], "ErrorMessages": ns["ErrorMessages"],
        "fab_constant": ns["fab_constant"]})
    utils = SimpleNamespace(process_nargs=lambda value:
        " ".join(value).strip("\"'") if isinstance(value, list) else value)
    common = {"json": json, "FabricCLIError": ns["FabricCLIError"], "fab_constant": ns["fab_constant"],
        "ErrorMessages": ns["ErrorMessages"], "utils": utils, "fab_util": utils,
        "utils_jmespath": SimpleNamespace(search=queries["search"]), "fab_ui": ui, "utils_ui": ui,
        "fab_logger": SimpleNamespace(log_warning=lambda *args: None, log_debug=lambda *args: None)}
    args = argparse.Namespace(command="acl", command_path="acl", output_format=case["format"],
        long=case.get("long", False), query=case.get("query"), output=None,
        acl_subcommand=case.get("command"), force=True)
    requests = []

    def request(args: argparse.Namespace, **kwargs: object) -> SimpleNamespace:
        requests.append({"method": args.method.upper(), "url": f"https://api.fabric.microsoft.com/v1/{args.uri}"})
        return SimpleNamespace(status_code=200, text=json.dumps(case.get("body", {})), headers={})

    common["fabric_api"] = SimpleNamespace(do_request=request)
    for binding, file in [("api_workspaces", "workspace"), ("api_items", "item"),
        ("api_gateway", "gateway"), ("api_gateways", "gateway"),
        ("api_connection", "connection"), ("api_connections", "connection"), ("api_onelake", "onelake")]:
        common[binding] = SimpleNamespace(**access.source_module(f"client/fab_api_{file}.py", common))
    formatter = access.source_module("utils/fab_cmd_ls_utils.py", common)
    common["utils_ls"] = SimpleNamespace(format_and_print_output=formatter["format_and_print_output"])
    exit_code = 0
    error = None
    try:
        if case["kind"] == "acl":
            handler = access.source_module(f"commands/acls/fab_acls_{case['command']}.py", common)
            context = SimpleNamespace(id="resource", name="Resource", workspace=SimpleNamespace(id="workspace", name="Workspace"),
                item=SimpleNamespace(id="resource", name="Resource"))
            handler[f"_{case['command']}_acls_{case['resource']}"](context, args)
        elif case["kind"] == "label-local":
            args.command = "label"
            args.input = "synthetic-label-file"
            handler = access.source_module("commands/labels/fab_labels_list_local.py", common)
            with patch("builtins.open", lambda *a, **kw: io.StringIO(json.dumps({"labels": case["labels"]}))):
                handler["exec_command"](args)
        elif case["kind"] == "find":
            args.command = "find"
            args.search_text, args.params, args.fab_mode = "report", None, "cli"
            common["catalog_api"] = SimpleNamespace(**access.source_module("client/fab_api_catalog.py", common))
            handler = access.source_module("commands/find/fab_find.py", common)
            handler["find_command"](args)
        elif case["kind"] == "label-remove":
            args.command = "label"
            common["api_labels"] = SimpleNamespace(**access.source_module("client/fab_api_labels.py", common))
            handler = access.source_module("commands/labels/fab_labels_rm.py", common)
            handler["_rm_label_item"](SimpleNamespace(id="item", item_type=SimpleNamespace(value="Notebook")), args)
        elif case["kind"] == "api":
            args.command, args.endpoint, args.method = "api", "synthetic", "get"
            args.input, args.file_path, args.show_headers, args.query = None, None, False, "."
            handler = access.source_module("commands/api/fab_api_request.py", common)
            handler["exec_command"](args)
        else:
            args.command = "find" if not case["handled"] else "acl"
            if not case["handled"]:
                for line in case["diagnostics"]:
                    ns["print_grey"](line)
            caught = ns["FabricCLIError"](case["message"], case["code"])
            ns["print_output_error"](caught, args.command, case["format"])
            if case["handled"]:
                for line in case["diagnostics"]:
                    ns["print_grey"](line)
            exit_code = 0 if case["handled"] else 4
    except ns["FabricCLIError"] as caught:
        error = {"message": caught.message, "code": caught.status_code}
    return {"requests": requests, "stdout": stdout.getvalue(), "stderr": stderr.getvalue(),
        "events": events, "exit": exit_code, "error": error}


def main() -> None:
    """Freeze new source observations once without regenerating existing expected data."""
    document = {"source_commit": access.SOURCE, "cases": [{**case, "expected": observe(case)} for case in inputs()]}
    parser = argparse.ArgumentParser()
    parser.add_argument("--initial-capture", action="store_true")
    args = parser.parse_args()
    path = HERE / "render_cases.json"
    if args.initial_capture:
        if path.exists():
            raise SystemExit("Refusing to overwrite family output oracle")
        path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    else:
        assert document == json.loads(path.read_text(encoding="utf-8")), "Family text/stream source drift"
    print(f"family whole-handler/source-UI streams: {len(document['cases'])} cases passed")


if __name__ == "__main__":
    main()

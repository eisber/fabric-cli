# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pinned request-dictionary lifetime versus actual dispatched query evidence."""

import __future__
import argparse
import ast
import copy
import json
import subprocess
import sys
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any
from unittest.mock import patch
from urllib.parse import parse_qsl, urlencode, urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "access"))
from reference import ROOT, SOURCE, SourceError, source_module

HERE = Path(__file__).resolve().parent


class Headers(dict):
    """Synthetic requests-style case-insensitive headers, without network I/O."""

    def get(self, key: str, default: Any = None) -> Any:
        return next((value for name, value in self.items() if name.lower() == key.lower()), default)

    def __contains__(self, key: object) -> bool:
        return isinstance(key, str) and any(name.lower() == key.lower() for name in self)

    def __getitem__(self, key: str) -> Any:
        return next(value for name, value in self.items() if name.lower() == key.lower())


def cases() -> list[dict]:
    """Source-only inputs; no native result supplies expected query state."""
    pages = [
        {"status": 200, "body": {"value": [{"id": "a"}], "continuationToken": " +/*~\u96ea"}},
        {"status": 200, "body": {"value": [{"id": "b"}]}},
    ]
    base = {"uri": "admin/domains/domain/workspaces", "initial": {"filter": "all"},
        "pages": pages, "followup": {"uri": "admin/domains/domain/unassignWorkspaces", "reset": "keep"}}
    result = []
    for name, changes in [
        ("preexisting-multipage", {}),
        ("absent-multipage", {"initial": None}),
        ("single-page", {"pages": [pages[1]]}),
        ("empty-header-stops-body-token", {"pages": [
            {"status": 200, "body": pages[0]["body"], "headers": {"x-ms-continuation": ""}}]}),
        ("header-token-before-body", {"pages": [
            {"status": 200, "body": pages[0]["body"], "headers": {"X-MS-Continuation": "header +/*~"}}, pages[1]]}),
        ("page-two-error", {"pages": [pages[0],
            {"status": 403, "body": {"errorCode": "Denied", "message": "denied"},
                "headers": {"x-ms-public-api-error-code": "Denied"}}], "followup": None}),
        ("page-two-credential-error", {"fail_credential_call": 2, "followup": None}),
        ("page-two-interrupt", {"pages": [pages[0], {"interrupt": True}], "followup": None}),
        ("three-pages", {"pages": [pages[0],
            {"status": 200, "body": {"value": [], "continuationToken": "last"}},
            pages[1]]}),
        ("later-uri-overwrite", {"uri": "admin/domains/domain/workspaces?filter=uri&continuationToken=initial",
            "followup": {"uri": "admin/domains/domain/unassignWorkspaces?continuationToken=override +/*~", "reset": "keep"}}),
        ("query-cleared", {"followup": {"uri": "admin/domains/domain/unassignWorkspaces", "reset": "clear"}}),
        ("query-replaced", {"followup": {"uri": "admin/domains/domain/unassignWorkspaces",
            "reset": "replace", "params": {"fresh": "value"}}}),
        ("query-attribute-removed", {"followup": {"uri": "admin/domains/domain/unassignWorkspaces", "reset": "absent"}}),
        ("terminal-unused-uri", {"pages": [
            {"status": 200, "body": {"value": [], "continuationUri": "https://foreign.invalid/unused"}}]}),
    ]:
        result.append(copy.deepcopy({**base, **changes, "name": name}))
    return result


def observe(case: dict) -> dict:
    """Run original do_request and recursive successful-response merging."""
    dispatched, scopes = [], []
    pages = iter(copy.deepcopy(case["pages"]))
    phase = "read"
    args = argparse.Namespace(uri=case["uri"], method="get")
    if case["initial"] is not None:
        args.request_params = copy.deepcopy(case["initial"])
    constants = SimpleNamespace(API_ENDPOINT_FABRIC="api.fabric.microsoft.com",
        API_VERSION_FABRIC="v1", SCOPE_FABRIC_DEFAULT=["https://api.fabric.microsoft.com/.default"],
        SCOPE_AZURE_DEFAULT=["https://management.azure.com/.default"],
        DEBUG=False, ERROR_UNAUTHORIZED="Unauthorized", ERROR_FORBIDDEN="Forbidden",
        ERROR_NOT_FOUND="NotFound")

    class Auth:
        def get_access_token(self, scope: list[str]) -> str:
            scopes.append(scope)
            if len(scopes) == case.get("fail_credential_call"):
                raise SourceError("Access is unauthorized", "Unauthorized")
            return "offline-only"

    auth = ModuleType("fabric_cli.core.fab_auth")
    auth.FabAuth = Auth
    context = ModuleType("fabric_cli.core.fab_context")
    context.Context = lambda: SimpleNamespace(command="synthetic", fabric_skill=None)

    class Session:
        def request(self, method: str, url: str, **kwargs: Any) -> SimpleNamespace:
            dispatched.append({"method": method.upper(), "url": url})
            page = next(pages) if phase == "read" else {"status": 200, "body": {}}
            if page.get("interrupt"):
                raise KeyboardInterrupt("synthetic source cancellation")
            text = json.dumps(page["body"])
            return SimpleNamespace(status_code=page["status"], text=text, content=text.encode(),
                headers=Headers(page.get("headers", {})))

    namespace = {"json": json, "FabricCLIError": SourceError, "fab_constant": constants,
        "fab_logger": SimpleNamespace(log_debug_http_request=lambda *args: None,
            log_debug_http_response=lambda *args: None),
        "time": SimpleNamespace(time=lambda: 0),
        "requests": SimpleNamespace(compat=SimpleNamespace(urlencode=urlencode),
            RequestException=RuntimeError),
        "ErrorMessages": SimpleNamespace(Common=SimpleNamespace(
            forbidden=lambda: "Access is forbidden. You do not have permission to access this resource",
            unauthorized=lambda: "Access is unauthorized"))}
    raw = subprocess.run(["git", "show", f"{SOURCE}:src/fabric_cli/client/fab_api_types.py"],
        cwd=ROOT, check=True, capture_output=True).stdout
    tree = ast.parse(raw.decode())
    tree.body = [node for node in tree.body if isinstance(node, ast.ClassDef)]
    exec(compile(tree, "fab_api_types.py", "exec", flags=__future__.annotations.compiler_flag), namespace)
    client = source_module("client/fab_api_client.py", namespace)
    client["_get_session"] = lambda: Session()
    client["_build_user_agent"] = lambda command: "offline-oracle"
    client["_transform_workspace_url_for_private_link_if_needed"] = lambda url, uri, **kwargs: url
    error = None
    with patch.dict(sys.modules, {"fabric_cli.core.fab_auth": auth, "fabric_cli.core.fab_context": context}):
        try:
            client["do_request"](args)
        except SourceError as exception:
            error = exception.status_code
        except KeyboardInterrupt:
            error = "Cancelled"
        reads = copy.deepcopy(dispatched)
        state = copy.deepcopy(getattr(args, "request_params", None))
        followup = case["followup"]
        if followup is not None:
            reset = followup["reset"]
            if reset == "clear":
                args.request_params.clear()
            elif reset == "replace":
                args.request_params = copy.deepcopy(followup["params"])
            elif reset == "absent":
                del args.request_params
            args.uri = followup["uri"]
            args.method = "post"
            args.raw_response = True
            phase = "followup"
            client["do_request"](args)
    last = reads[-1] if reads else None
    return {"read_requests": reads, "error": error, "source_state_after_read": state,
        "last_dispatched_query": urlsplit(last["url"]).query or None if last else None,
        "last_dispatched_pairs": [list(pair) for pair in parse_qsl(urlsplit(last["url"]).query, keep_blank_values=True)] if last else [],
        "followup_request": dispatched[-1] if followup is not None else None}


def main() -> None:
    """Initial capture refuses overwrite; future runs verify unchanged source."""
    document = {"source_commit": SOURCE, "cases": [{**case, "expected": observe(case)} for case in cases()]}
    parser = argparse.ArgumentParser()
    parser.add_argument("--initial-capture", action="store_true")
    args = parser.parse_args()
    path = HERE / "query_dispatch_cases.json"
    if args.initial_capture:
        if path.exists():
            raise SystemExit("Refusing to overwrite dispatched-query oracle")
        path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    else:
        assert document == json.loads(path.read_text(encoding="utf-8")), "Dispatched query source drift"
    print(f"source dispatched query/dictionary lifetime: {len(document['cases'])} cases passed")


if __name__ == "__main__":
    main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute original HTTP/error/decorator bodies with synthetic auth/network only."""

from __future__ import annotations

import argparse
import ast
import html
import json
import re
import subprocess
from collections import deque
from functools import wraps
from types import SimpleNamespace
from typing import Any
from urllib.parse import urlencode, urlparse

from operation_reference import Headers
from python_reference import PIN, load, source

OP = "22222222-2222-2222-2222-222222222222"


class Imports(ast.NodeTransformer):
    def visit_Import(self, node: ast.Import) -> None:
        return None

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
        return None


def selected(relative: str, names: set[str], namespace: dict[str, Any]) -> dict[str, Any]:
    """Replace import bindings only; execute the original source function bodies."""
    tree = ast.parse(source(relative))
    tree.body = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in names]
    tree = Imports().visit(tree)
    assert not any(isinstance(node, (ast.Import, ast.ImportFrom)) for node in ast.walk(tree))
    tree.body.insert(0, ast.ImportFrom(module="__future__", names=[ast.alias(name="annotations")], level=0))
    ast.fix_missing_locations(tree)
    exec(compile(tree, f"{PIN}:{relative}", "exec"), namespace)
    return namespace


def constants(names: set[str]) -> dict[str, Any]:
    """Read literal constants and explicit public defaults without ambient inputs."""
    result = {}
    for node in ast.parse(source("core/fab_constant.py")).body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id in names:
                    value = node.value
                    if isinstance(value, ast.Call) and isinstance(value.func, ast.Name) and value.func.id == "validate_and_get_env_variable":
                        value = value.args[1]
                    result[target.id] = ast.literal_eval(value)
    if set(result) != names:
        raise AssertionError("Missing original constants")
    return result


def reference(case: dict[str, Any]) -> dict[str, Any]:
    replies = deque(case["replies"])
    requests = []
    captured = {}
    exceptions = load("core/fab_exceptions.py", dict(json=json, html=html, re=re))
    common = load("errors/common.py", {})
    values = constants({
        "SCOPE_FABRIC_DEFAULT", "SCOPE_AZURE_DEFAULT", "SCOPE_ONELAKE_DEFAULT", "API_ENDPOINT_FABRIC", "API_ENDPOINT_ONELAKE", "API_VERSION_FABRIC",
        "ERROR_UNAUTHORIZED", "ERROR_FORBIDDEN", "ERROR_NOT_FOUND", "ERROR_OPERATION_FAILED",
        "ERROR_OPERATION_CANCELLED", "EXIT_CODE_AUTHORIZATION_REQUIRED", "EXIT_CODE_ERROR",
    })
    values["DEBUG"] = False
    values["ERROR_MAX_RETRIES_EXCEEDED"] = "MaxRetriesExceeded"
    values["ERROR_UNEXPECTED_ERROR"] = "UnexpectedError"
    values["ERROR_INVALID_OPERATION"] = "InvalidOperation"
    fab_constant = SimpleNamespace(**values)
    api_type = load("client/fab_api_types.py", dict(
        json=json, CaseInsensitiveDict=Headers, fab_constant=fab_constant,
        FabricCLIError=exceptions.FabricCLIError, ErrorMessages=SimpleNamespace(Common=common.CommonErrors),
    )).ApiResponse
    logger = SimpleNamespace(**{
        name: lambda *_: None for name in [
            "log_debug", "log_progress", "log_debug_http_request",
            "log_debug_http_response", "log_debug_http_request_exception",
        ]
    })
    interval = selected("utils/fab_http_polling_utils.py", {"get_polling_interval"},
                        dict(DEFAULT_POLLING_INTERVAL=10, fab_logger=logger))["get_polling_interval"]

    class SyntheticAuth:
        def get_access_token(self, scope: list[str]) -> str:
            assert scope == values["SCOPE_ONELAKE_DEFAULT"] if case.get("audience")=="storage" else scope == values["SCOPE_FABRIC_DEFAULT"]
            return "synthetic-error-contract-only"

    def request(method: str, url: str, **_: Any) -> Any:
        requests.append(dict(method=method, url=url))
        reply = replies.popleft()
        text = reply.get("text", json.dumps(reply.get("data")))
        return SimpleNamespace(status_code=reply["status"], text=text, content=text.encode(),
                               headers=Headers(reply.get("headers", {})))

    namespace = selected("client/fab_api_client.py", {
        "do_request", "_handle_successful_response", "_handle_fab_long_running_op",
        "_poll_operation", "_fetch_operation_result", "_log_operation_progress",
    }, dict(
        Namespace=argparse.Namespace, json=json, urlparse=urlparse,
        fab_constant=fab_constant, fab_logger=logger, get_polling_interval=interval,
        time=SimpleNamespace(time=lambda: 0, sleep=lambda _: None),
        requests=SimpleNamespace(compat=SimpleNamespace(urlencode=urlencode), RequestException=type("SyntheticRequestError", (Exception,), {})),
        _get_session=lambda: SimpleNamespace(request=request),
        _transform_workspace_url_for_private_link_if_needed=lambda url, uri, hostname=None, **_: hostname or url,
        _build_user_agent=lambda _: "synthetic-source-oracle",
        FabAuth=SyntheticAuth, FabContext=lambda: SimpleNamespace(command="set", fabric_skill=None),
        ApiResponse=api_type, FabricCLIError=exceptions.FabricCLIError, FabricAPIError=exceptions.FabricAPIError,
        AzureAPIError=exceptions.AzureAPIError, OnelakeAPIError=exceptions.OnelakeAPIError,
        ErrorMessages=SimpleNamespace(Common=common.CommonErrors),
    ))

    def output(error: Any, *_: Any, **__: Any) -> None:
        captured.update(code=error.status_code, message=error.message)

    decorators = selected("core/fab_decorators.py", {"handle_exceptions"}, dict(
        wraps=wraps, FabricCLIError=exceptions.FabricCLIError, fab_ui=SimpleNamespace(print_output_error=output),
        ERROR_UNAUTHORIZED=values["ERROR_UNAUTHORIZED"],
        EXIT_CODE_AUTHORIZATION_REQUIRED=values["EXIT_CODE_AUTHORIZATION_REQUIRED"],
        EXIT_CODE_ERROR=values["EXIT_CODE_ERROR"],
    ))
    storage=case.get("audience")=="storage"
    handler = decorators["handle_exceptions"]()(lambda args: namespace["do_request"](args, data=None if storage else "{}"))
    exit_code = handler(argparse.Namespace(uri="workspace/item/Files/error" if storage else "workspaces/ws/items/item", method="get" if storage else "patch",
                                          audience=case.get("audience"),command_path="set", output_format="json", request_params={}))
    if replies or not captured:
        raise AssertionError("Original source did not finish the expected failure script")
    return dict(captured, exit=exit_code, requests=requests)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True)
    args = parser.parse_args()
    cases = []
    for status in [401, 403, 404, 400, 500]:
        cases.append({"replies": [dict(status=status, data={"errorCode": "BodyCode", "message": "Service message...", "requestId": "synthetic"})]})
        cases.append({"replies": [dict(status=status, data={"errorCode": "BodyCode", "message": "Service message"},
                                      headers={"x-ms-public-api-error-code": "HeaderCode"})]})
    cases.extend([
        {"replies": [dict(status=401, data={}, headers={"x-ms-public-api-error-code": "", "x-ms-error-code": "SecondaryHeader"})]},
        {"replies": [dict(status=400, data={"message": "Message without error code."})]},
        {"replies": [dict(status=400, data={"errorCode": "CodeWithoutMessage"})]},
        {"replies": [dict(status=400, data={"errorCode": "", "message": "..."})]},
        {"replies": [dict(status=400, text="not JSON")]},
    ])
    details = [
        {"errorCode": "Synthetic", "message": "Failure."},
        {"z": None, "a": [True, False, 1.0, 1e-7, "x'y", 'x"y', 'x\'y"z', "\u200b", "\u00a0", "\u0001"], "n": 18446744073709551615},
        "already a string",
        None,
    ]
    for state in ["Failed", "Cancelled"]:
        for detail in details:
            cases.append({"replies": [
                dict(status=202, data={}, headers={"x-ms-operation-id": OP}),
                dict(status=200, data={"status": state, "error": detail}),
            ]})
    for status in [401,403,404,400,500]:
        for headers in [{},{"x-ms-public-api-error-code":"HeaderCode","x-ms-error-code":"OtherCode"}]:
            cases.append({"audience":"storage","replies":[dict(status=status,data={"error":{"code":"StorageBodyCode","message":"Service body..."}} ,headers=headers)]})
    for data in [
        {},{"error":{}},{"error":{"code":"OnlyCode"}},{"error":{"message":"OnlyMessage."}},
        {"error":{"code":"","message":"..."}},
        {"error":{"code":"Storage","message":"Failure\nRequestId:abc\nTime:2000-01-01T00:00:00Z"}},
        {"error":{"code":"Storage","message":"Failure RequestId:abc repeated RequestId:abc\nTime:now."}},
        {"error":{"code":"Storage","message":"Failure RequestId: empty RequestId:abc\u001cTime:now"}},
        {"error":{"code":"Storage","message":"\u65e5\u00a0message\nRequestId:request\u00a0Time:now"}},
    ]:
        cases.append({"audience":"storage","replies":[dict(status=400,data=data)]})
    cases.append({"audience":"storage","replies":[dict(status=400,text="not JSON")]})
    cases.append({"audience":"storage","replies":[dict(status=400,data={"error":{"code":"Unauthorized","message":"Code selects source authorization exit"}})]})
    expected = [reference(case) for case in cases]
    result = subprocess.run([args.probe], input="".join(json.dumps(case) + "\n" for case in cases),
                            text=True, encoding="utf-8", capture_output=True, check=True)
    actual = [json.loads(line) for line in result.stdout.splitlines()]
    if len(actual) != len(expected):
        raise AssertionError("Wrong number of Rust error outputs")
    for case, old, new in zip(cases, expected, actual, strict=True):
        if old != new:
            raise AssertionError(json.dumps({"case": case, "python": old, "rust": new}, indent=2))
    print(json.dumps({"source": PIN, "error_cases": len(cases), "result": "passed"}))


if __name__ == "__main__":
    main()

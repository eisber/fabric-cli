# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Compare the actual pinned Python Fabric poller against the shared Rust policy."""

from __future__ import annotations

import argparse
import ast
import json
import subprocess
from collections import deque
from types import SimpleNamespace
from typing import Any
from urllib.parse import urlparse

from python_reference import PIN, source

OP = "22222222-2222-2222-2222-222222222222"
LOCATION = f"https://api.fabric.microsoft.com/v1/operations/{OP}"


def functions(relative: str, names: set[str], namespace: dict[str, Any]) -> dict[str, Any]:
    """Execute selected unmodified source functions with explicit I/O collaborators."""
    tree = ast.parse(source(relative))
    tree.body = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in names]
    tree.body.insert(0, ast.ImportFrom(module="__future__", names=[ast.alias(name="annotations")], level=0))
    ast.fix_missing_locations(tree)
    exec(compile(tree, f"{PIN}:{relative}", "exec"), namespace)
    return namespace


class Headers(dict[str, str]):
    def __init__(self, headers: dict[str, str]) -> None:
        super().__init__((key.lower(), value) for key, value in headers.items())

    def get(self, key: str, default: Any = None) -> Any:
        return super().get(key.lower(), default)


class Response:
    def __init__(self, reply: dict[str, Any]) -> None:
        self.status_code = reply["status"]
        self.text = json.dumps(reply.get("data"))
        self.headers = Headers(reply.get("headers", {}))

    def json(self) -> Any:
        return json.loads(self.text)


class CLIError(Exception):
    def __init__(self, message: str, status_code: str) -> None:
        super().__init__(message)
        self.status_code = status_code


class APIError(CLIError):
    pass


def reference(case: dict[str, Any]) -> dict[str, Any]:
    """Run original _handle/_poll/_fetch bodies with synthetic response/time only."""
    replies = deque(case["replies"])
    original = Response(replies.popleft())
    requests: list[str] = []
    waits: list[int] = []
    fallbacks: list[str] = []

    def debug(message: str) -> None:
        if "cannot convert to integer" in message:
            fallbacks.append(message)

    logger = SimpleNamespace(log_debug=debug, log_progress=lambda *_: None)
    polling = functions("utils/fab_http_polling_utils.py", {"get_polling_interval"}, dict(
        DEFAULT_POLLING_INTERVAL=10, fab_logger=logger,
    ))

    def request(args: argparse.Namespace, **_: Any) -> Response:
        requests.append(args.uri)
        reply = replies.popleft()
        if "error" in reply:
            raise APIError("synthetic result error", reply["error"])
        return Response(reply)

    namespace = functions("client/fab_api_client.py", {
        "_handle_fab_long_running_op", "_poll_operation", "_fetch_operation_result",
        "_log_operation_progress",
    }, dict(
        Namespace=argparse.Namespace, json=json, urlparse=urlparse,
        time=SimpleNamespace(sleep=waits.append), do_request=request,
        get_polling_interval=polling["get_polling_interval"],
        FabricCLIError=CLIError, FabricAPIError=APIError, fab_logger=logger,
        fab_constant=SimpleNamespace(
            API_ENDPOINT_FABRIC="api.fabric.microsoft.com",
            SCOPE_FABRIC_DEFAULT="https://api.fabric.microsoft.com/.default",
            SCOPE_AZURE_DEFAULT="https://management.azure.com/.default",
            ERROR_OPERATION_FAILED="LongRunningOperationFailed",
            ERROR_OPERATION_CANCELLED="LongRunningOperationCancelled",
        ),
        ErrorMessages=SimpleNamespace(
            Common=SimpleNamespace(operation_failed=str, operation_cancelled=str),
            Client=SimpleNamespace(unexpected_error_response=lambda status, text: str(status)),
        ),
        utils_errors=SimpleNamespace(map_http_status_code_to_error_code=lambda status: "Http"),
    ))
    error = None
    status = None
    data = None
    try:
        result = namespace["_handle_fab_long_running_op"](original)
        status, data = result.status_code, result.json()
    except CLIError as failure:
        error = failure.status_code
    if replies:
        raise AssertionError("Original source did not consume the expected response script")
    return dict(status=status, data=data, error=error, requests=requests, waits=waits, fallbacks=len(fallbacks))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True)
    args = parser.parse_args()
    initial = dict(status=202, data={"id": "original"}, headers={"Location": LOCATION, "x-ms-operation-id": OP})
    cases = [
        {"replies": [dict(status=202, data={"id": "accepted"})]},
        {"replies": [initial, dict(status=200, data={"status": "Succeeded"})]},
        {"replies": [initial, dict(status=200, data={"status": "Completed"})]},
        {"replies": [initial, dict(status=200, data={"status": "A-future-running-state"}), dict(status=200, data={"status": "Completed"})]},
        {"replies": [initial, dict(status=200, data={"status": "Succeeded"}, headers={"Location": LOCATION + "/result"}), dict(status=200, data={"id": "result"})]},
        {"replies": [initial, dict(status=200, data={"status": "Completed"}, headers={"Location": "/relative/ignored/path"}), dict(status=201, data={"id": "result"})]},
        {"replies": [initial, dict(status=200, data={"status": "Succeeded"}, headers={"Location": LOCATION + "/result"}), dict(status=400, data={"errorCode": "OperationHasNoResult", "message": "synthetic"}, error="OperationHasNoResult")]},
        {"replies": [dict(status=202, data={"id": "original"}, headers={"Location": LOCATION}), dict(status=200, data={"status": "Completed"})]},
        {"replies": [dict(status=202, data={"id": "original"}, headers={"x-ms-operation-id": OP, "Retry-After": "invalid"}), dict(status=200, data={"status": "Succeeded"})]},
        {"replies": [dict(status=202, data={"id": "original"}, headers={"x-ms-operation-id": OP, "Retry-After": " +1_2 "}), dict(status=200, data={"status": "Succeeded"})]},
        {"replies": [initial, dict(status=200, data={"status": "Failed", "error": {"errorCode": "Synthetic", "message": "failed"}})]},
        {"replies": [initial, dict(status=200, data={"status": "Cancelled", "error": {"errorCode": "Synthetic", "message": "cancelled"}})]},
    ]
    expected = [reference(case) for case in cases]
    result = subprocess.run([args.probe], input="".join(json.dumps(case) + "\n" for case in cases),
                            text=True, encoding="utf-8", capture_output=True, check=True)
    actual = [json.loads(line) for line in result.stdout.splitlines()]
    if len(actual) != len(expected):
        raise AssertionError(f"Expected {len(expected)} outputs, received {len(actual)}")
    for case, old, new in zip(cases, expected, actual, strict=True):
        if old != new:
            raise AssertionError(json.dumps({"case": case, "python": old, "rust": new}, indent=2))
    print(json.dumps({"source": PIN, "operation_cases": len(cases), "result": "passed"}))


if __name__ == "__main__":
    main()

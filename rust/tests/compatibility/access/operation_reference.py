# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Original Python synchronous Fabric completion, with synthetic I/O/time only."""

import argparse
import json
import copy
from types import SimpleNamespace
from urllib.parse import urlparse

from reference import HERE, SOURCE, SourceError, source_module


def cases() -> list[dict]:
    """Complete accepted-response branches independently of Rust execution."""
    headers = {"x-ms-operation-id": "operation", "Retry-After": "0"}
    return [
        {"name": "completed-original", "headers": headers, "replies": [
            {"status": 200, "body": {"status": "Completed"}, "headers": {}}]},
        {"name": "running-then-succeeded", "headers": headers, "replies": [
            {"status": 200, "body": {"status": "Running"}, "headers": {"Retry-After": "0"}},
            {"status": 200, "body": {"status": "Succeeded"}, "headers": {}}]},
        {"name": "result-created", "headers": headers, "replies": [
            {"status": 200, "body": {"status": "Completed"}, "headers": {"Location": "https://api.fabric.microsoft.com/v1/operations/operation/result"}},
            {"status": 201, "body": {"created": True}, "headers": {}}]},
        {"name": "result-empty", "headers": headers, "replies": [
            {"status": 200, "body": {"status": "Succeeded"}, "headers": {"Location": "https://api.fabric.microsoft.com/v1/operations/operation/result"}},
            {"status": 204, "raw_body": "", "headers": {}}]},
        {"name": "result-unavailable", "headers": headers, "replies": [
            {"status": 200, "body": {"status": "Succeeded"}, "headers": {"Location": "https://api.fabric.microsoft.com/v1/operations/operation/result"}},
            {"status": 400, "body": {"errorCode": "OperationHasNoResult", "message": "No result"}, "headers": {}}]},
        {"name": "headerless", "headers": {}, "replies": []},
        {"name": "failed", "headers": headers, "replies": [
            {"status": 200, "body": {"status": "Failed", "error": "failure"}, "headers": {}}]},
        {"name": "cancelled", "headers": headers, "replies": [
            {"status": 200, "body": {"status": "Cancelled", "error": "reason"}, "headers": {}}]},
        {"name": "invalid-retry-fallback", "headers": {"x-ms-operation-id": "operation", "Retry-After": "bad"}, "replies": [
            {"status": 200, "body": {"status": "Completed"}, "headers": {}}]},
    ]


def observe(case: dict) -> dict:
    """Execute the actual source operation bodies and source polling interval."""
    requests, waits = [], []
    replies = iter(case["replies"])
    constants = SimpleNamespace(API_ENDPOINT_FABRIC="api.fabric.microsoft.com",
        SCOPE_FABRIC_DEFAULT=["https://api.fabric.microsoft.com/.default"],
        SCOPE_AZURE_DEFAULT=["https://management.azure.com/.default"],
        ERROR_OPERATION_FAILED="LongRunningOperationFailed",
        ERROR_OPERATION_CANCELLED="LongRunningOperationCancelled")
    polling = source_module("utils/fab_http_polling_utils.py", {"DEFAULT_POLLING_INTERVAL":10,
        "fab_logger": SimpleNamespace(log_warning=lambda *a: None, log_debug=lambda *a: None)})

    def response(value: dict) -> SimpleNamespace:
        text = value.get("raw_body", json.dumps(value.get("body")))
        class Headers(dict):
            def get(self, key: str, default: object = None) -> object:
                return next((v for k,v in self.items() if k.lower()==key.lower()),default)
        return SimpleNamespace(status_code=value["status"], text=text, content=text.encode(),
            headers=Headers(value["headers"]), json=lambda: json.loads(text))

    def request(args: argparse.Namespace, **kwargs: object) -> SimpleNamespace:
        requests.append({"method": args.method.upper(), "url": f"https://api.fabric.microsoft.com/v1/{args.uri}"})
        value = next(replies)
        if value["status"] >= 400:
            raise SourceError(value["body"].get("message"), value["body"]["errorCode"])
        return response(value)

    namespace = source_module("client/fab_api_client.py", {
        "json": json, "Namespace": argparse.Namespace, "urlparse": urlparse,
        "fab_constant": constants, "get_polling_interval": polling["get_polling_interval"],
        "time": SimpleNamespace(sleep=waits.append),
        "fab_logger": SimpleNamespace(log_progress=lambda *a: None),
        "FabricCLIError": SourceError, "FabricAPIError": SourceError,
        "ErrorMessages": SimpleNamespace(Common=SimpleNamespace(
            operation_failed=lambda error: f"The operation failed: {error}",
            operation_cancelled=lambda error: f"The operation was cancelled: {error}")),
    })
    namespace["do_request"] = request
    error = None
    result = None
    try:
        result = namespace["_handle_fab_long_running_op"](response(
            {"status": 202, "body": {"accepted": True}, "headers": case["headers"]}))
    except SourceError as exception:
        error = {"code": exception.status_code, "message": exception.message}
    return {"requests": requests, "waits": waits, "error": error,
        "status": result.status_code if result else None, "text": result.text if result else None}


def main() -> None:
    """Freeze once or verify source-derived expected completion behavior."""
    document = {"source_commit": SOURCE, "cases": [{**case, "expected": observe(case)} for case in cases()]}
    parser = argparse.ArgumentParser()
    parser.add_argument("--initial-capture", action="store_true")
    parser.add_argument("--valid-capture", action="store_true")
    args = parser.parse_args()
    path = HERE / "operation_cases.json"
    if args.initial_capture:
        if path.exists():
            raise SystemExit("Refusing to overwrite operation oracle")
        path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    else:
        assert document == json.loads(path.read_text(encoding="utf-8")), "Operation source drift"
    valid=copy.deepcopy(cases())
    operation="22222222-2222-2222-2222-222222222222"
    for case in valid:
        if "x-ms-operation-id" in case["headers"]:
            case["headers"]["x-ms-operation-id"]=operation
        for reply in case["replies"]:
            if "Location" in reply["headers"]:
                reply["headers"]["Location"]=reply["headers"]["Location"].replace("/operations/operation",f"/operations/{operation}")
    valid_document={"source_commit":SOURCE,"cases":[{**case,"expected":observe(case)} for case in valid]}
    valid_path=HERE / "operation_valid_cases.json"
    if args.valid_capture:
        if valid_path.exists():
            raise SystemExit("Refusing to overwrite valid-operation oracle")
        valid_path.write_text(json.dumps(valid_document,indent=2)+"\n",encoding="utf-8")
    elif valid_path.exists():
        assert valid_document==json.loads(valid_path.read_text(encoding="utf-8")),"Valid-operation source drift"
    print(f"source synchronous Fabric completion: {len(document['cases'])} cases passed")


if __name__ == "__main__":
    main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Check hand-authored retry traces using the pinned Python factory and urllib3."""

import ast
import hashlib
import json
from pathlib import Path
from typing import Any

import requests
from requests.adapters import HTTPAdapter, Retry
from urllib3.exceptions import MaxRetryError
from urllib3.response import HTTPResponse


def check_reference() -> None:
    """Execute the source retry factory without importing the product or using I/O."""
    directory = Path(__file__).resolve().parent
    cases: dict[str, Any] = json.loads(
        (directory / "retry-cases.json").read_text(encoding="utf-8")
    )
    root = directory.parents[3]
    source = root / "src" / "fabric_cli" / "client" / "fab_api_client.py"
    factory = next(
        node
        for node in ast.parse(source.read_text(encoding="utf-8")).body
        if isinstance(node, ast.FunctionDef) and node.name == "_get_session"
    )
    digest = hashlib.sha256(
        ast.dump(factory, include_attributes=False).encode("utf-8")
    ).hexdigest()
    if digest != cases["session_factory_ast_sha256"]:
        raise AssertionError("Python session retry factory drift requires review")
    namespace: dict[str, Any] = {
        "requests": requests,
        "HTTPAdapter": HTTPAdapter,
        "Retry": Retry,
        "_shared_session": None,
    }
    exec(
        compile(ast.Module(body=[factory], type_ignores=[]), str(source), "exec"),
        namespace,
    )
    session = namespace["_get_session"]()
    try:
        policy = session.get_adapter("https://api.fabric.microsoft.com").max_retries
        for case in cases["cases"]:
            retry = policy.new()
            delays: list[int | float] = []
            outcome = "response"
            requests_made = 0
            for status in case["statuses"]:
                requests_made += 1
                headers = {}
                if case["retry_after"] is not None:
                    headers["Retry-After"] = case["retry_after"]
                response = HTTPResponse(status=status, headers=headers)
                if not retry.is_retry(case["method"], status, bool(headers)):
                    break
                try:
                    retry = retry.increment(method=case["method"], response=response)
                except MaxRetryError:
                    outcome = "exhausted"
                    break
                delay = retry.get_retry_after(response) or retry.get_backoff_time()
                if delay:
                    delays.append(delay)
            observed = (requests_made, delays, outcome)
            expected = (
                case["requests"],
                case["wait_seconds"],
                case["outcome"],
            )
            if observed != expected:
                raise AssertionError(f"{case['name']}: {observed!r} != {expected!r}")
            print(f"PASS {case['name']}")
    finally:
        session.close()


if __name__ == "__main__":
    check_reference()

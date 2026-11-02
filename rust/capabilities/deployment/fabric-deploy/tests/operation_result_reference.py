# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual pinned terminal-Location/result behavior before fixture repair."""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import subprocess
from argparse import Namespace
from collections import deque
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from urllib.parse import urlparse

from config_reference import PIN, load

CLI_PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
OPERATION = "33333333-3333-3333-3333-333333333333"
ITEM = "22222222-2222-2222-2222-222222222222"
RESULT = f"https://api.fabric.microsoft.com/v1/operations/{OPERATION}/result"


def response(status: int, body: Any, headers: dict[str, str]) -> SimpleNamespace:
    """Build one synthetic HTTP response without any service or credentials."""
    return SimpleNamespace(
        status_code=status,
        headers=headers,
        text="" if body is None else json.dumps(body),
        json=lambda: body,
    )


def capture(cli_root: Path, cicd_root: Path) -> dict[str, Any]:
    """Execute unchanged original polling/response leaves with bounded callbacks."""

    def git(root: Path, *args: str) -> bytes:
        return subprocess.run(
            ["git", "--no-pager", "-C", str(root), *args],
            check=True,
            capture_output=True,
            timeout=10,
        ).stdout

    if git(cicd_root, "rev-parse", "HEAD").decode().strip() != PIN:
        raise ValueError("Wrong pinned CICD source")
    endpoint = cicd_root / "src" / "fabric_cicd" / "_common" / "_fabric_endpoint.py"
    git(cicd_root, "diff", "--exit-code", PIN, "--", str(endpoint))
    cicd: dict[str, Any] = {}
    load(endpoint, {"_handle_response"}, cicd)
    cli_path = "src/fabric_cli/client/fab_api_client.py"
    raw = git(cli_root, "show", f"{CLI_PIN}:{cli_path}")
    names = {
        "_handle_fab_long_running_op",
        "_poll_operation",
        "_fetch_operation_result",
    }
    nodes = [
        node
        for node in ast.parse(raw.decode("utf-8-sig")).body
        if isinstance(node, ast.FunctionDef) and node.name in names
    ]
    if len(nodes) != len(names):
        raise ValueError("Missing selected CLI source leaves")
    cli: dict[str, Any] = {
        "Namespace": Namespace,
        "urlparse": urlparse,
        "fab_constant": SimpleNamespace(
            API_ENDPOINT_FABRIC="api.fabric.microsoft.com",
            SCOPE_FABRIC_DEFAULT="fabric",
            SCOPE_AZURE_DEFAULT="arm",
        ),
        "fab_logger": SimpleNamespace(log_progress=lambda *args: None),
        "get_polling_interval": lambda headers: 0,
    }
    exec(compile(ast.Module(body=nodes, type_ignores=[]), cli_path, "exec"), cli)
    cases = []
    for advertised in [False, True]:
        headers = {"Location": RESULT} if advertised else {}
        terminal = response(200, {"status": "Succeeded"}, headers)
        transition = cicd["_handle_response"](
            terminal,
            "GET",
            f"https://api.fabric.microsoft.com/v1/operations/{OPERATION}",
            "{}",
            True,
            2,
        )
        accepted = response(
            202, None, {"x-ms-operation-id": OPERATION, "Retry-After": "0"}
        )
        replies = deque(
            [terminal] + ([response(200, {"id": ITEM}, {})] if advertised else [])
        )
        requests = []
        waits = []

        def request(args: Namespace, hostname: str | None = None) -> SimpleNamespace:
            requests.append(
                {"uri": args.uri, "method": args.method, "hostname": hostname}
            )
            if not replies:
                raise AssertionError("Unexpected source request/replay")
            return replies.popleft()

        cli.update({"do_request": request, "time": SimpleNamespace(sleep=waits.append)})
        result = cli["_handle_fab_long_running_op"](accepted)
        if replies:
            raise AssertionError("Source did not consume the expected result response")
        cases.append(
            {
                "id": (
                    "advertised-result"
                    if advertised
                    else "terminal-without-result-location"
                ),
                "terminalHeaders": headers,
                "cicdTransition": list(transition),
                "cliRequests": requests,
                "waits": waits,
                "result": {"status": result.status_code, "body": result.json()},
                "sameOriginalResponse": result is accepted,
            }
        )
    return {
        "sources": {"cli": CLI_PIN, "cicd": PIN},
        "hashes": {
            cli_path: hashlib.sha256(raw).hexdigest(),
            "src/fabric_cicd/_common/_fabric_endpoint.py": hashlib.sha256(
                endpoint.read_bytes()
            ).hexdigest(),
        },
        "boundary": "Actual CICD _handle_response and original CLI _handle_fab_long_running_op/_poll_operation/_fetch_operation_result; synthetic bounded GET responses/time only. Neither accepts terminal success as an item ID when no result Location/body is supplied.",
        "cases": cases,
    }


def main() -> None:
    """Capture once before changing new native input; otherwise compare fixed cases."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--cli-reference-root", type=Path, required=True)
    parser.add_argument("--cicd-reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = capture(args.cli_reference_root, args.cicd_reference_root)
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__)
        .with_name("operation_result_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError(
            "Pinned operation result observations differ from immutable receipt"
        )
    print(
        json.dumps(
            {
                "sources": result["sources"],
                "cases": len(result["cases"]),
                "status": "passed",
            }
        )
    )


if __name__ == "__main__":
    main()

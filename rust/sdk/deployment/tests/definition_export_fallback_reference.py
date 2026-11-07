# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual pinned export fallback and remote serialization independently."""

import ast
import base64
import copy
import json
import os
from argparse import Namespace
from pathlib import Path
import shutil
import subprocess
import sys
from typing import Any, Optional

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
ROOT = Path(__file__).resolve().parents[4]


def load(path: str, names: set[str], scope: dict) -> None:
    """Execute only actual source leaves, without product initialization."""
    source = subprocess.check_output(
        ["git", "show", f"{PIN}:{path}"], cwd=ROOT, encoding="utf-8"
    )
    nodes = [
        node
        for node in ast.parse(source).body
        if isinstance(node, ast.FunctionDef) and node.name in names
    ]
    assert {node.name for node in nodes} == names
    exec(compile(ast.Module(body=nodes, type_ignores=[]), path, "exec"), scope)


def main() -> None:
    """Capture into a new receipt using only a caller-owned project directory."""
    output = Path(sys.argv[1])
    scratch = Path(sys.argv[2])
    if output.exists() or scratch.exists():
        raise RuntimeError("Receipt and scratch paths must be new")
    scratch.mkdir()
    scope = {
        "base64": base64, "json": json, "os": os, "Namespace": Namespace,
        "Any": Any, "Optional": Optional,
    }
    load("src/fabric_cli/utils/fab_cmd_export_utils.py", {"decode_payload"}, scope)
    load(
        "src/fabric_cli/utils/fab_storage.py",
        {"_validate_json", "_write_to_local", "_write_to_lakehouse"},
        scope,
    )
    cases = [
        ("malformed-json", {"path": "bad.json", "payload": base64.b64encode(b"{broken").decode()}),
        ("invalid-base64", {"path": "bad.txt", "payload": "a"}),
        ("non-string-object", {"path": "object.json", "payload": {"z": "\u00e9"}}),
        ("missing-payload", {"path": "missing.txt"}),
        ("empty-payload", {"path": "empty.json", "payload": ""}),
        ("nested-marker", {"path": "outer.txt", "payload": "eA==", "nested_parts": []}),
        ("remote-json", {"path": "object.json", "payload": base64.b64encode('{"z":"\u00e9","a":[1,true]}'.encode()).decode()}),
        ("remote-text", {"path": "code.py", "payload": base64.b64encode(b"a\r\nb\n").decode()}),
        ("remote-binary", {"path": "bytes.bin", "payload": base64.b64encode(b"\xff\r\n\x00").decode()}),
    ]
    records = []
    try:
        for name, part in cases:
            original = {"definition": {"parts": [part]}}
            decoded = scope["decode_payload"](copy.deepcopy(original))
            payload = decoded["definition"]["parts"][0].get("payload", {})
            file = scratch / part["path"]
            scope["_write_to_local"](Namespace(), str(file), payload, True)
            record = {
                "name": name, "input": original, "path": part["path"],
                "local_base64": base64.b64encode(file.read_bytes()).decode(),
            }
            if name.startswith("remote-"):
                calls = []

                class Storage:
                    """Capture actual source create/append/flush arguments."""

                    @staticmethod
                    def touch_file(args: Namespace) -> Namespace:
                        calls.append({"stage": "create", "path": args.to_path})
                        return Namespace(status_code=201)

                    @staticmethod
                    def append_file(args: Namespace, data: Any, position: int) -> None:
                        content = data.encode() if isinstance(data, str) else data
                        calls.append({"stage": "append", "base64": base64.b64encode(content).decode(), "position": position})

                    @staticmethod
                    def flush_file(args: Namespace, position: int) -> None:
                        calls.append({"stage": "flush", "position": position})

                scope["onelake_api"] = Storage
                scope["_write_to_lakehouse"](
                    Namespace(), "Demo.Workspace/Lake.Lakehouse/Files/" + part["path"], payload, True
                )
                record["remote"] = calls
            records.append(record)
        output.write_text(json.dumps({"source": PIN, "platform": sys.platform, "cases": records}, indent=2) + "\n", encoding="utf-8")
    finally:
        shutil.rmtree(scratch)


if __name__ == "__main__":
    main()

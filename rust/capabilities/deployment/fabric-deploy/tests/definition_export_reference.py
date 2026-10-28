# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute pinned export/storage leaves on synthetic data, never product auth."""

import ast
import base64
import copy
import json
import os
from argparse import Namespace
from pathlib import Path
import subprocess
import sys
import tempfile
from typing import Any, Optional

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
ROOT = Path(__file__).resolve().parents[5]


def load_functions(path: str, names: set[str], scope: dict) -> None:
    """Compile actual pinned leaves, excluding imports and product initialization."""
    source = subprocess.check_output(
        ["git", "show", f"{PIN}:{path}"], cwd=ROOT, encoding="utf-8"
    )
    tree = ast.parse(source)
    selected = [
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name in names
    ]
    if {node.name for node in selected} != names:
        raise RuntimeError("Pinned source no longer contains the requested leaves")
    module = ast.Module(body=selected, type_ignores=[])
    exec(compile(module, f"{PIN}:{path}", "exec"), scope)


def main() -> None:
    """Write an additive source receipt before observing the Rust implementation."""
    if Path(sys.argv[1]).exists():
        raise RuntimeError("Use a new receipt path; existing expectations are immutable")
    scope = {
        "base64": base64,
        "json": json,
        "os": os,
        "Namespace": Namespace,
        "Optional": Optional,
        "Any": Any,
    }
    load_functions(
        "src/fabric_cli/utils/fab_cmd_export_utils.py",
        {"decode_payload", "clean_notebook_cells"},
        scope,
    )
    load_functions(
        "src/fabric_cli/utils/fab_storage.py",
        {"_validate_json", "_write_to_local"},
        scope,
    )
    cases = [
        ("unicode-object", "definition.json", '{"z": "é😀", "a": null, "n": 1e-5}'),
        ("plain-json", "data.txt", '{"a": [1, true]}'),
        ("scalar-null", "null.json", "null"),
        ("scalar-boolean", "bool.json", "false"),
        ("scalar-number", "number.json", "1e-5"),
        ("json-string-json", "string.json", '"{\\"x\\": 2}"'),
        ("json-string-plain", "plain.json", '"hello"'),
        ("raw-null", "null.txt", "null"),
        ("empty", "empty.json", ""),
        ("binary", "image.bin", b"\xff\x00\x80"),
        ("text-newlines", "code.py", "a\r\nb\n"),
        (
            "notebook-first-part",
            "notebook.ipynb",
            '{"cells":[{"outputs":[1],"metadata":{"x":null}},{"source":["x"]}]}',
        ),
    ]
    records = []
    with tempfile.TemporaryDirectory(dir=sys.argv[2]) as directory:
        for name, path, content in cases:
            data = content.encode("utf-8") if isinstance(content, str) else content
            item = {
                "definition": {
                    "parts": [
                        {
                            "path": path,
                            "payload": base64.b64encode(data).decode("ascii"),
                            "payloadType": "InlineBase64",
                        }
                    ]
                }
            }
            original = copy.deepcopy(item)
            decoded = scope["decode_payload"](item)
            if name.startswith("notebook"):
                decoded = scope["clean_notebook_cells"](decoded, ["outputs"])
            file = Path(directory) / path
            scope["_write_to_local"](
                Namespace(),
                str(file),
                decoded["definition"]["parts"][0]["payload"],
                export=True,
            )
            records.append(
                {
                    "name": name,
                    "notebook": name.startswith("notebook"),
                    "input": original,
                    "path": path,
                    "expected_base64": base64.b64encode(file.read_bytes()).decode("ascii"),
                }
            )
    receipt = {
        "source": PIN,
        "platform": sys.platform,
        "cases": records,
    }
    Path(sys.argv[1]).write_text(
        json.dumps(receipt, indent=2, ensure_ascii=True) + "\n", encoding="utf-8"
    )
    print(f"{len(records)} pinned source cases captured to {sys.argv[1]}")


if __name__ == "__main__":
    main()

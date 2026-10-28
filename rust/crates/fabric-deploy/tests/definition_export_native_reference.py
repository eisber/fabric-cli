# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual source wide-integer/nonfinite export values without product code."""

import base64
import copy
import json
import os
from argparse import Namespace
from pathlib import Path
import shutil
import sys
from typing import Any, Optional

from definition_export_fallback_reference import PIN, load


def main() -> None:
    """Write a fresh native-domain receipt confined to caller-owned project paths."""
    output, scratch = Path(sys.argv[1]), Path(sys.argv[2])
    if output.exists() or scratch.exists():
        raise RuntimeError("Paths must be new")
    scratch.mkdir()
    scope = {
        "base64": base64, "json": json, "os": os, "Namespace": Namespace,
        "Any": Any, "Optional": Optional,
    }
    load("src/fabric_cli/utils/fab_cmd_export_utils.py", {"decode_payload", "clean_notebook_cells"}, scope)
    load("src/fabric_cli/utils/fab_storage.py", {"_validate_json", "_write_to_local"}, scope)
    cases = [
        ("wide-scalar", "value.json", "184467440737095516160123456789"),
        ("wide-nested", "value.json", '{"wide":-184467440737095516160123456789,"dup":1,"dup":2}'),
        ("nonfinite-nested", "value.json", '{"a":NaN,"b":Infinity,"c":-Infinity,"d":1e999}'),
        ("nonfinite-scalar", "value.json", "NaN"),
        ("overflow-scalar", "value.json", "1e999"),
        ("raw-nonfinite", "value.txt", "NaN"),
        ("raw-wide", "value.txt", "184467440737095516160123456789"),
        ("notebook-wide", "notebook.ipynb", '{"cells":[{"outputs":[1],"metadata":{"n":18446744073709551616}}]}'),
        ("escaped-astral", "value.json", '{"x":"\\ud83d\\ude00"}'),
    ]
    records = []
    try:
        for name, path, text in cases:
            original = {"definition": {"parts": [{"path": path, "payload": base64.b64encode(text.encode()).decode()}]}}
            decoded = scope["decode_payload"](copy.deepcopy(original))
            if name.startswith("notebook"):
                scope["clean_notebook_cells"](decoded, ["outputs"])
            file = scratch / path
            scope["_write_to_local"](Namespace(), str(file), decoded["definition"]["parts"][0]["payload"], True)
            records.append({
                "name": name, "input": original, "path": path, "notebook": name.startswith("notebook"),
                "expected_base64": base64.b64encode(file.read_bytes()).decode(),
            })
        output.write_text(json.dumps({"source": PIN, "platform": sys.platform, "cases": records}, indent=2) + "\n", encoding="utf-8")
    finally:
        shutil.rmtree(scratch)


if __name__ == "__main__":
    main()

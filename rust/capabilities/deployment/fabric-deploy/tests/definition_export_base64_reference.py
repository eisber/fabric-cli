# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture Python's actual base64 domain; native strict rejection is separate policy."""

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
    """Capture immutable source bytes in a fresh caller-confined project root."""
    output, scratch = Path(sys.argv[1]), Path(sys.argv[2])
    if output.exists() or scratch.exists():
        raise RuntimeError("Receipt and scratch must be new")
    scratch.mkdir()
    scope = {
        "base64": base64, "json": json, "os": os, "Namespace": Namespace,
        "Any": Any, "Optional": Optional,
    }
    load("src/fabric_cli/utils/fab_cmd_export_utils.py", {"decode_payload"}, scope)
    load("src/fabric_cli/utils/fab_storage.py", {"_validate_json", "_write_to_local"}, scope)
    cases = []
    try:
        for name, encoded in [
            ("canonical", "eA=="),
            ("percent-after-padding", "eA==%"),
            ("percent-within-data", "e%A=="),
            ("whitespace-after-padding", "eA==\n"),
            ("nonzero-pad-bits", "Zh=="),
            ("excess-padding", "eA==="),
            ("source-error-original-payload", "a"),
        ]:
            original = {"definition": {"parts": [{"path": "part.txt", "payload": encoded}]}}
            decoded = scope["decode_payload"](copy.deepcopy(original))
            part = decoded["definition"]["parts"][0]
            path = scratch / "part.txt"
            scope["_write_to_local"](Namespace(), str(path), part["payload"], True)
            cases.append({
                "name": name, "input": original,
                "source_part_error": part.get("error"),
                "expected_base64": base64.b64encode(path.read_bytes()).decode("ascii"),
            })
        output.write_text(json.dumps({"source": PIN, "cases": cases}, indent=2) + "\n", encoding="utf-8")
    finally:
        shutil.rmtree(scratch)


if __name__ == "__main__":
    main()

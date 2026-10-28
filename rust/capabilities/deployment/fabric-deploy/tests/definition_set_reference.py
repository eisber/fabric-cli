# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture new SET codec cases from immutable source, not Rust observations."""

import base64
import copy
import json
from pathlib import Path
import sys

from definition_export_reference import PIN, load_functions


def main() -> None:
    """Execute the actual source codec around a caller-supplied decoded edit."""
    if Path(sys.argv[1]).exists():
        raise RuntimeError("Use a new receipt path; existing expectations are immutable")
    scope = {"base64": base64, "json": json}
    load_functions(
        "src/fabric_cli/utils/fab_cmd_set_utils.py",
        {"_decode_payload", "_encode_payload", "ensure_notebook_dependency"},
        scope,
    )
    cases = []
    for name, path, payload, replacement in [
        ("object", "part.json", {"x": 1}, {"x": "é😀", "n": 1e-5, "null": None}),
        ("ipynb", "notebook.ipynb", {"cells": []}, {"cells": [], "n": 1.0}),
        ("string", "part.json", {"x": 1}, "plain text"),
        ("array", "part.json", {"x": 1}, [1, None, False]),
        ("boolean", "part.json", {"x": 1}, False),
        ("null", "part.json", {"x": 1}, None),
    ]:
        item = {
            "definition": {
                "parts": [
                    {
                        "path": path,
                        "payload": base64.b64encode(
                            json.dumps(payload).encode("utf-8")
                        ).decode("ascii"),
                        "payloadType": "InlineBase64",
                    }
                ]
            }
        }
        decoded = scope["_decode_payload"](copy.deepcopy(item))
        edited = copy.deepcopy(decoded)
        edited["definition"]["parts"][0]["payload"] = replacement
        encoded = scope["_encode_payload"](copy.deepcopy(edited))
        cases.append(
            {
                "name": name,
                "input": item,
                "decoded": decoded,
                "edited": edited,
                "encoded": encoded,
                "body": json.dumps(encoded),
            }
        )
    Path(sys.argv[1]).write_text(
        json.dumps({"source": PIN, "cases": cases}, indent=2) + "\n",
        encoding="utf-8",
    )
    print(f"{len(cases)} new pinned SET cases captured to {sys.argv[1]}")


if __name__ == "__main__":
    main()

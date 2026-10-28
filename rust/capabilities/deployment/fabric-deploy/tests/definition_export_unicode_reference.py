# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual Lakehouse export's Unicode character-count flush position."""

import base64
import json
from argparse import Namespace
from pathlib import Path
import sys
from typing import Any, Optional

from definition_export_fallback_reference import PIN, load


def main() -> None:
    """Capture only injected source storage calls, without any service access."""
    output = Path(sys.argv[1])
    if output.exists():
        raise RuntimeError("Receipt must be new")
    calls = []

    def create(args: Namespace) -> Namespace:
        calls.append({"stage": "create", "path": args.to_path})
        return Namespace(status_code=201)

    def append(args: Namespace, content: str, position: int) -> None:
        calls.append({"stage": "append", "base64": base64.b64encode(content.encode()).decode(), "position": position})

    def flush(args: Namespace, position: int) -> None:
        calls.append({"stage": "flush", "position": position})

    scope = {
        "json": json, "Namespace": Namespace, "Any": Any, "Optional": Optional,
        "onelake_api": Namespace(touch_file=create, append_file=append, flush_file=flush),
    }
    load("src/fabric_cli/utils/fab_storage.py", {"_validate_json", "_write_to_lakehouse"}, scope)
    scope["_write_to_lakehouse"](Namespace(), "Demo.Workspace/Lake.Lakehouse/Files/unicode.txt", "\u00e9\U0001f600", True)
    output.write_text(json.dumps({"source": PIN, "calls": calls}, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

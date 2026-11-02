# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual pinned import file ordering/filtering and request bodies."""

import base64
import json
import os
from pathlib import Path
import sys
import tempfile
from typing import Any, Optional

from definition_export_reference import PIN, load_functions


class SourceError(Exception):
    """Stand-in for the source exception class, not a replacement algorithm."""


def main() -> None:
    """Run source leaves only on new caller-owned synthetic directories."""
    output = Path(sys.argv[1])
    if output.exists():
        raise RuntimeError("Use a new receipt path; existing expectations are immutable")
    scope = {
        "base64": base64,
        "json": json,
        "os": os,
        "Any": Any,
        "Optional": Optional,
        "FabricCLIError": SourceError,
        "fab_constant": type("Constants", (), {"ERROR_INVALID_DEFINITION_PAYLOAD": "InvalidDefinitionPayload"}),
    }
    load_functions(
        "src/fabric_cli/utils/fab_cmd_import_utils.py",
        {"_build_definition", "_encode_file_to_base64"},
        scope,
    )
    cases = [
        ("ordered", None, [
            ("z.txt", b"last root\n"), ("a.bin", b"\xff\x00"),
            ("sub-z/file.txt", b"z"), ("sub-a/file.txt", b"a"),
            ("cache.abf", b"excluded"), ("sub-a/cache.abf.copy", b"excluded too"),
        ]),
        ("notebook-format", "ipynb", [("notebook.ipynb", b'{"cells":[]}\r\n')]),
        ("empty-directory", None, []),
        ("report-connection", None, [("definition.pbir", b'{"datasetReference":{"byConnection":{"id":"synthetic"}}}')]),
        ("report-null-path", None, [("definition.pbir", b'{"datasetReference":{"byPath":null}}')]),
        ("report-relative-path", None, [("definition.pbir", b'{"datasetReference":{"byPath":{"path":"../Model"}}}')]),
        ("report-validation-before-exclusion", None, [("cache.abf.definition.pbir", b'{"datasetReference":{"byPath":{"path":"../Model"}}}')]),
    ]
    records = []
    with tempfile.TemporaryDirectory(dir=sys.argv[2]) as temporary:
        for name, format_name, files in cases:
            root = Path(temporary) / name
            root.mkdir()
            for relative, content in files:
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
            record = {
                "name": name,
                "format": format_name,
                "files": [{"path": path, "bytes": base64.b64encode(content).decode("ascii")} for path, content in files],
            }
            try:
                definition = scope["_build_definition"](str(root), format_name)
                record["definition"] = definition
                record["create_body"] = json.dumps({
                    "type": "Notebook", "folderId": None,
                    "displayName": "Synthetic", "definition": definition,
                })
                record["update_body"] = json.dumps({"definition": definition})
            except SourceError as error:
                record["error"] = list(error.args)
            records.append(record)
    output.write_text(json.dumps({"source": PIN, "cases": records}, indent=2) + "\n", encoding="utf-8")
    print(f"{len(records)} new actual pinned import cases captured to {output}")


if __name__ == "__main__":
    main()

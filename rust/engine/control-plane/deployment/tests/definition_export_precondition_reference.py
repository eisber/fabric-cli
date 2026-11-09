# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual pinned bulk arguments, output validation and prompt branches."""

import ast
from argparse import Namespace
import json
from pathlib import Path
import subprocess
import sys
from typing import Any, Iterator

from definition_export_fallback_reference import PIN, ROOT, load


class FabricCLIError(Exception):
    """Source exception boundary with no product/global initialization."""

    def __init__(self, message: str, status_code: str) -> None:
        super().__init__(message)
        self.message, self.status_code = message, status_code


class Scan:
    """Synthetic caller-owned directory listing."""

    def __init__(self, nonempty: bool) -> None:
        self.nonempty = nonempty

    def __enter__(self) -> Iterator[object]:
        return iter([object()] if self.nonempty else [])

    def __exit__(self, *args: Any) -> None:
        pass


def main() -> None:
    """Write a fresh receipt from actual source and explicitly injected FS/UI."""
    output = Path(sys.argv[1])
    if output.exists():
        raise RuntimeError("Receipt must be new")
    scope = {"Namespace": Namespace, "FabricCLIError": FabricCLIError}
    path = "src/fabric_cli/errors/bulk_export.py"
    text = subprocess.check_output(["git", "show", f"{PIN}:{path}"], cwd=ROOT, encoding="utf-8")
    classes = [node for node in ast.parse(text).body if isinstance(node, ast.ClassDef) and node.name == "BulkExportErrors"]
    assert len(classes) == 1
    exec(compile(ast.Module(body=classes, type_ignores=[]), path, "exec"), scope)
    text = subprocess.check_output(["git", "show", f"{PIN}:src/fabric_cli/core/fab_constant.py"], cwd=ROOT, encoding="utf-8")
    values = [
        ast.literal_eval(node.value)
        for node in ast.parse(text).body
        if isinstance(node, ast.Assign)
        and any(isinstance(target, ast.Name) and target.id == "ERROR_INVALID_OPERATION" for target in node.targets)
    ]
    assert len(values) == 1
    scope["fab_constant"] = Namespace(ERROR_INVALID_OPERATION=values[0])
    load("src/fabric_cli/commands/fs/fab_fs_bulk_export.py",
         {"_validate_bulk_export_arguments", "_confirm_export_preconditions"}, scope)
    cases = []
    for name, recursive, destination, kind, nonempty, force, confirmation in [
        ("missing-recursive", False, "output", "local", False, True, True),
        ("empty-output", True, "", "local", False, True, True),
        ("remote-output", True, "Demo.Workspace/Lake.Lakehouse/Files/Archive", "lakehouse", False, True, True),
        ("nonempty-declined", True, "output", "local", True, False, False),
        ("sensitivity-declined", True, "output", "local", False, False, False),
        ("forced-nonempty", True, "output", "local", True, True, True),
        ("sensitivity-confirmed", True, "output", "local", False, False, True),
    ]:
        events = []

        def confirm(message: str) -> bool:
            events.append({"kind": "confirm", "message": message})
            return confirmation

        def warning(message: str) -> None:
            events.append({"kind": "warning", "message": message})

        scope["fab_ui"] = Namespace(prompt_confirm=confirm, print_warning=warning)
        scope["fab_storage"] = Namespace(get_export_path=lambda _: {"type": kind, "path": destination})
        scope["os"] = Namespace(path=Namespace(isdir=lambda _: True), scandir=lambda _: Scan(nonempty))
        record = {"name": name, "output": destination, "recursive": recursive, "kind": kind,
                  "nonempty": nonempty, "force": force, "confirmation": confirmation}
        try:
            args = Namespace(recursive=recursive, output=destination, force=force)
            scope["_validate_bulk_export_arguments"](args)
            record["result"] = scope["_confirm_export_preconditions"](args)
            record["error"] = None
        except FabricCLIError as error:
            record["result"] = None
            record["error"] = {"message": error.message, "code": error.status_code}
        record["events"] = events
        cases.append(record)
    output.write_text(json.dumps({"source": PIN, "cases": cases}, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

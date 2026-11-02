# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture pinned import input metadata classification without filesystem/auth I/O."""

import ast
from argparse import Namespace
import json
from pathlib import Path
import subprocess
import sys
from typing import Optional

from definition_export_fallback_reference import PIN, ROOT, load


def source(path: str) -> ast.Module:
    """Read immutable source rather than importing product initialization."""
    text = subprocess.check_output(["git", "show", f"{PIN}:{path}"], cwd=ROOT, encoding="utf-8")
    return ast.parse(text)


def main() -> None:
    """Write a fresh metadata receipt before native expectations."""
    output = Path(sys.argv[1])
    if output.exists():
        raise RuntimeError("Receipt must be new")
    scope = {"Namespace": Namespace, "Optional": Optional, "json": json}
    path = "src/fabric_cli/core/fab_exceptions.py"
    nodes = [
        node for node in source(path).body
        if (isinstance(node, ast.ClassDef) and node.name == "FabricCLIError")
        or (isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id in {"DEFAULT_ERROR_MESSAGE", "DEFAULT_ERROR_CODE"}
            for target in node.targets
        ))
    ]
    exec(compile(ast.Module(body=nodes, type_ignores=[]), path, "exec"), scope)
    path = "src/fabric_cli/errors/common.py"
    common = next(node for node in source(path).body if isinstance(node, ast.ClassDef) and node.name == "CommonErrors")
    common.body = [node for node in common.body if isinstance(node, ast.FunctionDef) and node.name in {
        "invalid_path", "only_supported_for_lakehouse_files", "no_such_file_or_directory",
    }]
    assert len(common.body) == 3
    exec(compile(ast.Module(body=[common], type_ignores=[]), path, "exec"), scope)
    scope["ErrorMessages"] = Namespace(Common=scope["CommonErrors"])
    constants = {}
    for node in source("src/fabric_cli/core/fab_constant.py").body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id in {"ERROR_NOT_SUPPORTED", "ERROR_INVALID_PATH"}:
                    constants[target.id] = ast.literal_eval(node.value)
    assert len(constants) == 2
    scope["fab_constant"] = Namespace(**constants)

    class OneLake:
        """Synthetic already-resolved source metadata."""

        def __init__(self, path: str, kind: str, relative: str) -> None:
            self.path, self.local_path = path, relative
            self.item = Namespace(item_type=kind)

    scope.update({"OneLakeItem": OneLake, "Item": object, "ItemType": Namespace(LAKEHOUSE="Lakehouse")})
    load("src/fabric_cli/utils/fab_storage.py", {"get_import_path"}, scope)
    load("src/fabric_cli/commands/fs/impor/fab_fs_import_item.py", {"import_single_item"}, scope)
    scope["fab_item_util"] = Namespace(resolve_definition_format=lambda **kwargs: "")
    records = []
    for name, kind, relative in [
        ("local-directory-candidate", "local", ""),
        ("local-file-candidate", "local", ""),
        ("lakehouse-files-child", "Lakehouse", "Files/input"),
        ("lakehouse-files-root", "Lakehouse", "Files"),
        ("warehouse-files-child", "Warehouse", "Files/input"),
        ("fabric-folder", "fabric", ""),
    ]:
        input_path = "input" if kind == "local" else (
            "Demo.Workspace/Parent.Folder" if kind == "fabric"
            else f"Demo.Workspace/Lake.{kind}/{relative}"
        )
        events = []

        def context(path: str) -> object:
            if kind == "local":
                raise LookupError("Not a Fabric path")
            return object() if kind == "fabric" else OneLake(path, kind, relative)

        def confirm() -> bool:
            events.append("prompt")
            raise AssertionError("Remote inputs must fail before prompts")

        scope["handle_context"] = Namespace(get_command_context=context)
        scope["os"] = Namespace(path=Namespace(expanduser=lambda value: value, exists=lambda value: True))
        scope["utils_storage"] = Namespace(get_import_path=scope["get_import_path"])
        scope["utils_ui"] = Namespace(prompt_confirm=confirm)
        record = {"name": name, "input": input_path}
        try:
            if kind == "local":
                record["path_type"] = scope["get_import_path"](input_path)["type"]
            else:
                scope["import_single_item"](
                    Namespace(item_type="DataPipeline", workspace=Namespace(id="workspace")),
                    Namespace(input=input_path, force=False),
                )
                raise AssertionError("Remote input unexpectedly accepted")
            record["error"] = None
        except scope["FabricCLIError"] as error:
            record["path_type"] = None
            record["error"] = {"code": error.status_code, "message": error.message}
        record["events"] = events
        records.append(record)
    output.write_text(json.dumps({"source": PIN, "cases": records}, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

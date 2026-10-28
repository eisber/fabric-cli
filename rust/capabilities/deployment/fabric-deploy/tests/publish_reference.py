# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Actual pinned NotebookPublisher -> FabricWorkspace._publish_item wire oracle."""

from __future__ import annotations

import argparse
import ast
import base64
import builtins
import hashlib
import json
import logging
import re
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from config_reference import PIN, WORKSPACE, load

ITEM = "22222222-2222-2222-2222-222222222222"
PLATFORM = '{"metadata":{"displayName":"Book","type":"Notebook","description":"Synthetic"},"config":{"logicalId":"logical-book"}}'
CONTENT = "# Fabric notebook source\nprint('synthetic')\n"


class File:
    """Synthetic already-loaded input exposing the actual source payload property."""

    def __init__(self, path: str, contents: str) -> None:
        self.file_path = Path(path)
        self.relative_path = path
        self.contents = contents
        self.type = "text"


def capture(root: Path) -> dict[str, Any]:
    """Execute unchanged publisher methods with a recording, no-I/O endpoint."""
    head = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
        timeout=10,
    ).stdout.strip()
    if head != PIN:
        raise ValueError("Wrong pinned source")
    source = root / "src" / "fabric_cicd"
    paths = [
        source / name
        for name in [
            "fabric_workspace.py",
            "_items/_notebook.py",
            "_common/_file.py",
        ]
    ]
    subprocess.run(
        ["git", "-C", str(root), "diff", "--exit-code", PIN, "--", *map(str, paths)],
        check=True,
        capture_output=True,
        timeout=10,
    )
    constants = SimpleNamespace(
        SHELL_ONLY_PUBLISH=[],
        NON_MOVABLE_ITEM_TYPES=[],
        FEATURE_FLAG={"disable_workspace_folder_publish"},
        DEFAULT_GUID="00000000-0000-0000-0000-000000000000",
        WORKSPACE_ID_REFERENCE_REGEX=r'"?(default_lakehouse_workspace_id|workspaceId|workspace)"?\s*[:=]\s*"(.*?)"',
        API_FORMAT_MAPPING={"Notebook": "ipynb"},
        INDENT="    ",
    )
    flag = SimpleNamespace(
        DISABLE_WORKSPACE_FOLDER_PUBLISH=SimpleNamespace(
            value="disable_workspace_folder_publish"
        )
    )
    namespace: dict[str, Any] = {
        "__builtins__": dict(vars(builtins)),
        "json": json,
        "re": re,
        "Path": Path,
        "logger": logging.getLogger("publish-reference"),
        "constants": constants,
        "FeatureFlag": flag,
        "ItemType": SimpleNamespace(
            NOTEBOOK=SimpleNamespace(value="Notebook"),
            VARIABLE_LIBRARY=SimpleNamespace(value="VariableLibrary"),
            REFLEX=SimpleNamespace(value="Reflex"),
        ),
        "API_FORMAT_MAPPING": constants.API_FORMAT_MAPPING,
    }
    namespace["__builtins__"]["__import__"] = lambda name, *args: (
        SimpleNamespace()
        if name == "fabric_cicd._parameter._utils"
        else (_ for _ in ()).throw(ImportError(name))
    )
    methods = {
        "_publish_item",
        "_apply_publish_filters",
        "_apply_folder_path_filters",
        "_replace_logical_ids",
        "_replace_parameters",
        "_replace_workspace_ids",
    }
    tree = ast.parse(paths[0].read_text(encoding="utf-8-sig"))
    original = next(
        node
        for node in tree.body
        if isinstance(node, ast.ClassDef) and node.name == "FabricWorkspace"
    )
    selected = [
        node
        for node in original.body
        if isinstance(node, ast.FunctionDef) and node.name in methods
    ]
    exec(
        compile(ast.Module(body=selected, type_ignores=[]), str(paths[0]), "exec"),
        namespace,
    )
    file_tree = ast.parse(paths[2].read_text(encoding="utf-8-sig"))
    file_class = next(
        node
        for node in file_tree.body
        if isinstance(node, ast.ClassDef) and node.name == "File"
    )
    payload = next(
        node
        for node in file_class.body
        if isinstance(node, ast.FunctionDef) and node.name == "base64_payload"
    )
    file_namespace = {"base64": base64}
    exec(
        compile(ast.Module(body=[payload], type_ignores=[]), str(paths[2]), "exec"),
        file_namespace,
    )
    File.base64_payload = file_namespace["base64_payload"]
    publisher_tree = ast.parse(paths[1].read_text(encoding="utf-8-sig"))
    publisher = next(
        node
        for node in publisher_tree.body
        if isinstance(node, ast.ClassDef) and node.name == "NotebookPublisher"
    )
    method = next(
        node
        for node in publisher.body
        if isinstance(node, ast.FunctionDef) and node.name == "publish_one"
    )
    exec(
        compile(ast.Module(body=[method], type_ignores=[]), str(paths[1]), "exec"),
        namespace,
    )
    cases = []
    for existing in [False, True]:
        requests = []

        def invoke(**kwargs: Any) -> dict[str, Any]:
            requests.append(kwargs)
            return {"body": {"id": ITEM}, "status_code": 200 if existing else 201}

        item = SimpleNamespace(
            type="Notebook",
            name="Book",
            guid=ITEM if existing else "",
            logical_id="logical-book",
            description="Synthetic",
            folder_id="",
            folder_path="",
            item_files=[
                File("notebook-content.py", CONTENT),
                File(".platform", PLATFORM),
            ],
        )
        workspace = SimpleNamespace(
            base_api_url=f"https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}",
            workspace_id=WORKSPACE,
            repository_items={"Notebook": {"Book": item}},
            deployed_items={},
            environment_parameter={},
            responses=None,
            bulk_publish_enabled=False,
            publish_item_name_exclude_regex=None,
            publish_folder_path_exclude_regex=None,
            publish_folder_path_to_include=None,
            endpoint=SimpleNamespace(invoke=invoke),
        )
        for name in methods:
            setattr(workspace, name, namespace[name].__get__(workspace))
        # Actual source imports these names even when the parameter dictionary is empty.
        empty_parameter_module = SimpleNamespace(
            **{
                name: None
                for name in [
                    "check_replacement",
                    "extract_find_value",
                    "extract_parameter_filters",
                    "extract_replace_value",
                    "process_environment_key",
                    "replace_key_value",
                ]
            }
        )
        namespace["__builtins__"][
            "__import__"
        ] = lambda name, *args: empty_parameter_module
        publisher_input = SimpleNamespace(
            item_type="Notebook", fabric_workspace_obj=workspace
        )
        namespace["publish_one"](publisher_input, "Book", item)
        cases.append(
            {
                "id": "update" if existing else "create",
                "files": [[".platform", PLATFORM], ["notebook-content.py", CONTENT]],
                "requests": requests,
                "body_bytes": json.dumps(requests[0]["body"]),
                "observed_id": item.guid,
            }
        )
    return {
        "source": f"microsoft/fabric-cicd@{PIN}",
        "boundary": "Actual NotebookPublisher.publish_one, FabricWorkspace._publish_item/filter/transform methods and File.base64_payload. Synthetic endpoint records requests; no package/auth/transport initialization.",
        "hashes": {
            "\\".join(path.relative_to(source).parts): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in paths
        },
        "cases": cases,
    }


def main() -> None:
    """Capture once, or compare the immutable actual-source wire receipt."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = capture(args.reference_root)
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__).with_name("publish_source.json").read_text(encoding="utf-8")
    ):
        raise AssertionError(
            "Pinned Notebook source wire differs from immutable receipt"
        )
    print(
        json.dumps({"source": PIN, "cases": len(result["cases"]), "status": "passed"})
    )


if __name__ == "__main__":
    main()

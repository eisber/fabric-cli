# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual pinned CICD publisher calls without credentials or network.

Only source AST definitions are executed. Imports, package initialization, file
discovery and the production endpoint are not executed. The endpoint records
requests and returns synthetic IDs. This is deliberately separate from native
expected-value construction.
"""

from __future__ import annotations

import argparse
import ast
import base64
import json
import logging
import re
from pathlib import Path
from types import SimpleNamespace

PIN = "5ca096c00a9a878c646c59fe537b883eba951329"
WORKSPACE = "11111111-1111-1111-1111-111111111111"
ITEM = "22222222-2222-2222-2222-222222222222"
LOGICAL = "33333333-3333-3333-3333-333333333333"


def definitions(path: Path, namespace: dict, names: set[str] | None = None) -> None:
    tree = ast.parse(path.read_text(encoding="utf-8"))
    nodes = [
        node
        for node in tree.body
        if isinstance(node, (ast.ClassDef, ast.FunctionDef))
        and (names is None or node.name in names)
    ]
    module = ast.Module(
        body=[ast.ImportFrom(module="__future__", names=[ast.alias("annotations")], level=0), *nodes],
        type_ignores=[],
    )
    exec(compile(ast.fix_missing_locations(module), str(path), "exec"), namespace)


class SourceFile:
    def __init__(self, path: str, content: str) -> None:
        self.file_path = Path(path)
        self.relative_path = path
        self.name = self.file_path.name
        self.contents = content
        self.type = "text"

    @property
    def base64_payload(self) -> dict:
        return {
            "path": self.relative_path,
            "payload": base64.b64encode(self.contents.encode()).decode(),
            "payloadType": "InlineBase64",
        }


class Endpoint:
    def __init__(self) -> None:
        self.calls: list[dict] = []

    def invoke(self, **kwargs: object) -> dict:
        self.calls.append(kwargs)
        return {
            "body": {"id": ITEM, "properties": {"sqlEndpointProperties": {"provisioningStatus": "Success"}}},
            "header": {},
            "status_code": 200,
        }


def capture(source: Path) -> dict:
    namespace = {"Enum": __import__("enum").Enum}
    definitions(source / "constants.py", namespace, {"ItemType", "FeatureFlag"})
    constants = SimpleNamespace(
        ItemType=namespace["ItemType"],
        FeatureFlag=namespace["FeatureFlag"],
        FEATURE_FLAG=set(),
        DEFAULT_GUID="00000000-0000-0000-0000-000000000000",
        INDENT="  ",
        SHELL_ONLY_PUBLISH=["Lakehouse", "Warehouse", "SQLDatabase", "MLExperiment"],
        NON_MOVABLE_ITEM_TYPES=["OrgAppAudience"],
        API_FORMAT_MAPPING={"Notebook": "ipynb", "SparkJobDefinition": "SparkJobDefinitionV2"},
        EXCLUDE_PATH_REGEX_MAPPING={
            "Report": r".*\.pbi[/\\].*",
            "SemanticModel": r".*\.pbi[/\\].*",
            "DataAgent": r".*\.pbi[/\\].*",
            "Eventhouse": r".*\.children[/\\].*",
            "OrgApp": r".*\.children[/\\].*",
        },
    )
    workspace_namespace = {
        "re": re, "logger": logging.getLogger("offline-oracle"),
        "constants": constants, "FeatureFlag": namespace["FeatureFlag"], "ItemType": namespace["ItemType"],
    }
    workspace_tree = ast.parse((source / "fabric_workspace.py").read_text(encoding="utf-8"))
    workspace_class = next(node for node in workspace_tree.body if isinstance(node, ast.ClassDef))
    publish = next(node for node in workspace_class.body if isinstance(node, ast.FunctionDef) and node.name == "_publish_item")
    exec(compile(ast.fix_missing_locations(ast.Module(body=[
        ast.ImportFrom(module="__future__", names=[ast.alias("annotations")], level=0), publish
    ], type_ignores=[])), "source:_publish_item", "exec"), workspace_namespace)

    class Workspace:
        _publish_item = workspace_namespace["_publish_item"]
        base_api_url = f"https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}"
        environment = "test"
        responses = None
        bulk_publish_enabled = False
        publish_item_name_exclude_regex = None

        def _apply_publish_filters(self, *_: object) -> bool:
            return False

        def _replace_logical_ids(self, value: str) -> str:
            return value

        def _replace_parameters(self, file: SourceFile, _: object) -> str:
            return file.contents

        def _replace_workspace_ids(self, value: str, _: object) -> str:
            return value

    class Publisher:
        def __init__(self, workspace: Workspace) -> None:
            self.fabric_workspace_obj = workspace

    base_tree = ast.parse((source / "_items" / "_base_publisher.py").read_text(encoding="utf-8"))
    base_class = next(node for node in base_tree.body if isinstance(node, ast.ClassDef) and node.name == "ItemPublisher")
    base_publish = next(node for node in base_class.body if isinstance(node, ast.FunctionDef) and node.name == "publish_one")
    base_namespace = {}
    exec(compile(ast.fix_missing_locations(ast.Module(body=[
        ast.ImportFrom(module="__future__", names=[ast.alias("annotations")], level=0), base_publish
    ], type_ignores=[])), "source:ItemPublisher.publish_one", "exec"), base_namespace)
    Publisher.publish_one = base_namespace["publish_one"]

    class Dpath:
        @staticmethod
        def get(value: dict, path: str, default: object = None) -> object:
            for key in path.split("/"):
                if not isinstance(value, dict) or key not in value:
                    return default
                value = value[key]
            return value

    records = []
    for path in sorted((source / "_items").glob("*.py")):
        if path.name in {"__init__.py", "_base_publisher.py", "_bulk_publish_dependencies.py", "_manage_dependencies.py"}:
            continue
        local = {
            "ItemPublisher": Publisher, "Publisher": Publisher,
            "ParallelConfig": lambda **_: None,
            "constants": constants, "ItemType": namespace["ItemType"],
            "FeatureFlag": namespace["FeatureFlag"], "json": json, "re": re,
            "dpath": Dpath, "logger": logging.getLogger("offline-oracle"),
            "EXCLUDE_PATH_REGEX_MAPPING": constants.EXCLUDE_PATH_REGEX_MAPPING,
            "API_FORMAT_MAPPING": constants.API_FORMAT_MAPPING,
            "FailedPublishedItemStatusError": RuntimeError, "ParsingError": RuntimeError,
        }
        definitions(path, local)
        publisher_class = next(
            value for name, value in local.items()
            if name.endswith("Publisher") and isinstance(value, type)
            and value not in {Publisher} and hasattr(value, "item_type")
        )
        item_type = publisher_class.item_type
        for update in (False, True):
            item = SimpleNamespace(
                name="Example", type=item_type, description="source fixture",
                logical_id=LOGICAL, guid=ITEM if update else "", folder_id="",
                skip_publish=False, path=Path(f"Example.{item_type}"),
                item_files=[
                    SourceFile(".platform", '{"metadata": {"displayName": "Example"}}'),
                    SourceFile("content.json", '{"dataSources": [], "queryset": {}}'),
                ],
            )
            workspace = Workspace()
            workspace.endpoint = Endpoint()
            workspace.repository_items = {item_type: {"Example": item}}
            workspace.deployed_items = {item_type: {"Example": SimpleNamespace(folder_id="")}} if update else {}
            publisher_class(workspace).publish_one("Example", item)
            records.append({
                "type": item_type, "update": update,
                "source": f"_items/{path.name}:{publisher_class.__name__}.publish_one",
                "calls": workspace.endpoint.calls,
            })
    return {"source_commit": PIN, "workspace_id": WORKSPACE, "item_id": ITEM, "cases": records}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.write_text(json.dumps(capture(args.source), indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

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
import sys
import types
from collections import defaultdict, deque
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
    constant_names = {
        "FEATURE_FLAG", "DEFAULT_GUID", "INDENT", "SHELL_ONLY_PUBLISH",
        "NON_MOVABLE_ITEM_TYPES", "API_FORMAT_MAPPING", "EXCLUDE_PATH_REGEX_MAPPING",
        "UNPUBLISH_FLAG_MAPPING", "SERIAL_ITEM_PUBLISH_ORDER", "VALID_GUID_REGEX",
    }
    constants_tree = ast.parse((source / "constants.py").read_text(encoding="utf-8"))
    constant_nodes = [
        node for node in constants_tree.body
        if (isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id in constant_names for target in node.targets))
        or (isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name) and node.target.id in constant_names)
    ]
    exec(compile(ast.fix_missing_locations(ast.Module(body=constant_nodes, type_ignores=[])), "source:constants", "exec"), namespace)
    namespace["VALID_GUID_REGEX"] = r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"
    constants = SimpleNamespace(**namespace)
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
    pipeline_namespace = {
        "re": re, "constants": constants, "ItemType": namespace["ItemType"],
        "ItemPublisher": Publisher, "ParallelConfig": lambda **_: None,
    }
    definitions(source / "_items" / "_datapipeline.py", pipeline_namespace)
    pipeline_inputs = [
        {"properties": {"activities": [{
            "type": "IfCondition", "typeProperties": {"ifTrueActivities": [
                {"type": "InvokePipeline", "typeProperties": {"pipelineId": {"type": "Expression", "value": LOGICAL}}},
                {"type": "ExecutePipeline", "typeProperties": {"pipeline": {"referenceName": ITEM}}},
            ]},
        }, {"type": "Other", "typeProperties": {"pipelineId": WORKSPACE}}]}},
        {"properties": {"activities": [{"type": "ExecutePipeline", "typeProperties": {"pipeline": {"referenceName": {"type": "Expression", "value": "@dynamic"}}}}]}},
    ]
    pipeline_cases = []
    for content in pipeline_inputs:
        references = list(pipeline_namespace["_iter_pipeline_reference_ids"](content["properties"]["activities"]))
        pipeline_cases.append({"content": content, "references": [value for value in dict.fromkeys(references) if re.fullmatch(namespace["VALID_GUID_REGEX"], value)]})

    sort_namespace = {"defaultdict": defaultdict, "deque": deque, "logger": logging.getLogger("offline-oracle"), "ParsingError": RuntimeError}
    definitions(source / "_items" / "_manage_dependencies.py", sort_namespace)
    sorts = []
    for graph in [{"A": ["B"], "B": [], "C": []}, {"A": ["B"], "B": ["A"]}]:
        for mode in ("Repository", "Deployed"):
            try:
                result = sort_namespace["sort_items"](None, graph, mode, lambda _workspace, refs, _mode: refs)
            except RuntimeError:
                result = {"error": "cycle"}
            sorts.append({"graph": graph, "mode": mode, "result": result})
    return {
        "source_commit": PIN, "workspace_id": WORKSPACE, "item_id": ITEM,
        "cases": records, "pipeline_cases": pipeline_cases, "sort_cases": sorts,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = capture(args.source)
    result["preprocess_cases"] = capture_preprocessors(args.source)
    result["unpublish_cases"] = capture_unpublish(args.source)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")


def capture_preprocessors(source: Path) -> list[dict]:
    import yaml

    utils = types.ModuleType("fabric_cicd._parameter._utils")
    # These imports are unused by the no-parameter Sparkcompute capture. Refuse
    # accidental calls rather than providing a replacement oracle algorithm.
    def unused(*_: object, **__: object) -> None:
        raise AssertionError("Unexpected parameter helper call")
    utils._find_match = unused
    utils.process_environment_key = unused
    sys.modules[utils.__name__] = utils
    records = []
    for kind, module, path, content in [
        ("Report", "_report.py", "definition.pbir", '{"datasetReference": {"byPath": {"path": "../Model.SemanticModel"}}}'),
        ("KQLQueryset", "_kqlqueryset.py", "queryset.json", json.dumps({"queryset": {"dataSources": [{"clusterUri": "", "databaseItemId": LOGICAL}]}})),
        ("KQLDashboard", "_kqldashboard.py", "dashboard.json", '{"dataSources": [{"clusterUri": "", "name": "Database"}]}'),
        ("Environment", "_environment.py", "Setting/Sparkcompute.yml", "instance_pool_id: old\nruntime_version: '1.3'\n"),
    ]:
        endpoint = Endpoint()
        endpoint.invoke = lambda **kwargs: endpoint_reply(endpoint, kwargs)
        database = SimpleNamespace(logical_id=LOGICAL, guid=ITEM, name="Database")
        workspace = SimpleNamespace(
            base_api_url=f"https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}",
            endpoint=endpoint, repository_items={"KQLDatabase": {"Database": database}},
            deployed_items={"KQLDatabase": {"Database": database}},
            environment_parameter={}, environment="test",
            _convert_path_to_id=lambda *_: LOGICAL,
        )
        namespace = {
            "json": json, "yaml": yaml, "Path": Path, "re": re,
            "ItemPublisher": type("Publisher", (), {}), "ItemType": SimpleNamespace(
                REPORT=SimpleNamespace(value="Report"), KQL_QUERYSET=SimpleNamespace(value="KQLQueryset"),
                KQL_DASHBOARD=SimpleNamespace(value="KQLDashboard"), ENVIRONMENT=SimpleNamespace(value="Environment"),
                SEMANTIC_MODEL=SimpleNamespace(value="SemanticModel"), KQL_DATABASE=SimpleNamespace(value="KQLDatabase"),
            ),
            "logger": logging.getLogger("offline-oracle"),
            "ParsingError": RuntimeError, "ItemDependencyError": RuntimeError,
        }
        definitions(source / "_items" / module, namespace)
        file = SourceFile(path, content)
        item = SimpleNamespace(type=kind, name="Example", path=Path("Example.Report"))
        function = namespace["_process_environment_file"] if kind == "Environment" else namespace["func_process_file"]
        output = function(workspace, item, file)
        records.append({
            "type": kind, "path": path, "input": content, "output": output,
            "calls": endpoint.calls, "yaml_version": yaml.__version__ if kind == "Environment" else None,
        })
    return records


def endpoint_reply(endpoint: Endpoint, kwargs: dict) -> dict:
    endpoint.calls.append(kwargs)
    return {"body": {"properties": {"queryServiceUri": "https://synthetic.kusto.fabric.microsoft.com"}}}


def capture_unpublish(source: Path) -> list[dict]:
    tree = ast.parse((source / "fabric_workspace.py").read_text(encoding="utf-8"))
    workspace_class = next(node for node in tree.body if isinstance(node, ast.ClassDef))
    methods = [node for node in workspace_class.body if isinstance(node, ast.FunctionDef) and node.name in {"_unpublish_item", "_unpublish_folders"}]
    feature_namespace = {"Enum": __import__("enum").Enum}
    definitions(source / "constants.py", feature_namespace, {"FeatureFlag"})
    constants = SimpleNamespace(FEATURE_FLAG=set(), INDENT="->")
    namespace = {"constants": constants, "FeatureFlag": feature_namespace["FeatureFlag"], "logger": logging.getLogger("offline-oracle")}
    exec(compile(ast.fix_missing_locations(ast.Module(body=[
        ast.ImportFrom(module="__future__", names=[ast.alias("annotations")], level=0), *methods,
    ], type_ignores=[])), "source:unpublish", "exec"), namespace)
    records = []
    for hard in (False, True):
        constants.FEATURE_FLAG = {"enable_hard_delete"} if hard else set()
        endpoint = Endpoint()
        workspace = SimpleNamespace(
            endpoint=endpoint, base_api_url=f"https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}",
            deployed_items={"Notebook": {"Orphan": SimpleNamespace(guid=ITEM)}}, unpublish_responses=None,
        )
        namespace["_unpublish_item"](workspace, "Orphan", "Notebook")
        records.append({"id": "hard" if hard else "soft", "calls": endpoint.calls})
    endpoint = Endpoint()
    workspace = SimpleNamespace(
        endpoint=endpoint, base_api_url=f"https://api.fabric.microsoft.com/v1/workspaces/{WORKSPACE}",
        deployed_folders={"/Parent": ITEM, "/Parent/Kept": LOGICAL, "/Parent/Empty": WORKSPACE},
        deployed_items={"Notebook": {"Kept": SimpleNamespace(folder_id=LOGICAL)}},
    )
    namespace["_unpublish_folders"](workspace)
    records.append({"id": "prune-empty-preserve-ancestors", "calls": endpoint.calls})
    return records


if __name__ == "__main__":
    main()

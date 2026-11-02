# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute pinned OneLake/base-item support checks, not YAML help assumptions."""

from __future__ import annotations

import __future__
import ast
import hashlib
import json
import subprocess
from abc import ABC, abstractmethod
from pathlib import Path
from types import SimpleNamespace as NS
from typing import Any

import reference

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
COMMIT = reference.SOURCE_COMMIT
PINS = {
    "core/hiearchy/fab_element.py": "b3f82aacb100bd76c150ff42595283793db6bb0b",
    "core/hiearchy/fab_base_item.py": "9de460fa50360e148f28854975e77b6908b1530f",
    "core/hiearchy/fab_onelake_element.py": "bb9f1978aa5dbb0aa24af4d60ba1545c234671cb",
    "core/fab_commands.py": "65c40ef3ef0ea8af4f1a13f745cb53196eb505b1",
    "core/fab_config/command_support.yaml": "454f23b8066124db5ca79d5e37790bfd937dafe0",
    "core/fab_types.py": "ceba9685ea693078f64faf8303c8a62c198f9c1b",
    "errors/hierarchy.py": "41a95bb2408a7382579acc844810db96dbf2aa57",
    "utils/fab_cmd_job_utils.py": "df4cc361ba474015152cd096d579979a8baee785",
}


def load() -> tuple[dict[str, ast.Module], dict[str, Any]]:
    """Pin source blobs and parse only simple group-level YAML list facts."""
    sources = reference.load_sources()
    config: dict[str, Any] = {"commands": {}}
    for path, pin in PINS.items():
        data = subprocess.check_output(["git", "show", f"{COMMIT}:src/fabric_cli/{path}"], cwd=ROOT)
        assert hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest() == pin
        if path.endswith(".yaml"):
            group = key = None
            for line in data.decode().splitlines():
                if line.startswith("  ") and not line.startswith("   ") and line.endswith(":"):
                    group = line.strip()[:-1]
                    config["commands"][group] = {"subcommands": {}}
                    key = None
                elif group and line.startswith("    ") and not line.startswith("     ") and line.endswith(":"):
                    key = line.strip()[:-1]
                    if key != "subcommands":
                        config["commands"][group][key] = []
                elif group and key and key != "subcommands" and line.startswith("      - "):
                    config["commands"][group][key].append(line.strip()[2:])
        else:
            sources[path] = ast.parse(data.decode(), filename=path)
    assert config["commands"]["table"]["supported_elements"] == ["onelake"]
    return sources, config


def main() -> None:
    """Check all TABLE leaves on real OneLake hierarchy and the nested job boundary."""
    sources, config = load()
    manifest = json.loads((HERE / "resources.json").read_text(encoding="utf-8"))
    assert manifest["source_commit"] == COMMIT and len(manifest["cases"]) == 8
    class Kind(str):
        @classmethod
        def from_string(cls, value: str) -> Kind:
            return cls(value.casefold())
    env: dict[str, Any] = {
        "ABC": ABC, "abstractmethod": abstractmethod,
        "DEFAULT_ERROR_MESSAGE": "An error occurred while processing the operation",
        "DEFAULT_ERROR_CODE": "UnknownError", "FabricElementType": Kind,
        "ItemType": Kind, "VirtualItemType": Kind, "VirtualWorkspaceItemType": Kind,
        "fab_constant": NS(ERROR_UNSUPPORTED_COMMAND="UnsupportedCommand"),
    }
    Kind.ONELAKE = Kind("onelake")
    Kind.ITEM = Kind("item")
    reference.symbols(sources, "core/fab_exceptions.py", ["FabricCLIError"], env)
    hierarchy = reference.symbols(sources, "errors/hierarchy.py", ["HierarchyErrors"], env)
    env["ErrorMessages"] = NS(Hierarchy=hierarchy.HierarchyErrors)
    env["get_command_support_dict"] = lambda: config
    commands = reference.symbols(sources, "core/fab_commands.py", [
        "_snake_to_camel", "_get_property_from_commands_and_subcommands",
        "get_supported_elements", "_get_items", "get_supported_items", "get_unsupported_items",
    ], env)
    env["cmd"] = commands
    reference.symbols(sources, "core/hiearchy/fab_element.py", ["FabricElement"], env)
    base = reference.symbols(sources, "core/hiearchy/fab_base_item.py", ["_BaseItem"], env)
    env["Item"] = base._BaseItem
    onelake = reference.symbols(sources, "core/hiearchy/fab_onelake_element.py", ["OneLakeItem"], env)
    assert "check_command_support" not in onelake.OneLakeItem.__dict__
    job_values = {}
    for node in sources["core/fab_types.py"].body:
        if isinstance(node, ast.ClassDef) and node.name == "FabricJobType":
            job_values = {
                statement.targets[0].id: ast.literal_eval(statement.value)
                for statement in node.body if isinstance(statement, ast.Assign)
            }
    job_map = {}
    for node in sources["core/fab_types.py"].body:
        if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name) and node.target.id == "ITJobMap":
            assert isinstance(node.value, ast.Dict)
            job_map = {
                commands._snake_to_camel(key.attr.lower()).casefold(): job_values[value.attr]
                for key, value in zip(node.value.keys, node.value.values)
            }
    props = reference.symbols(sources, "utils/fab_cmd_job_utils.py", ["add_item_props_to_args"], env)
    # No source operation is allowed to escape the injected metadata boundary.
    for case in manifest["cases"]:
        item = base._BaseItem("item", "item", Kind.ITEM, None, Kind(case["item_type"].casefold()))
        child = onelake.OneLakeItem("sales", "0", item, "table")
        for leaf in ["schema", "load", "vacuum", "optimize"]:
            command = NS(name="TABLE_" + leaf.upper(), value="table " + leaf)
            assert child.check_command_support(command) is case["table_check"], (case, leaf)
        command = NS(name="JOB_RUN", value="job run")
        try:
            allowed = item.check_command_support(command)
        except env["FabricCLIError"] as error:
            assert error.status_code == "UnsupportedCommand"
            assert error.message == "not supported for command 'job run'"
            allowed = False
        assert allowed is case["job_check"], case
        if allowed:
            actual_job_type = job_map[case["item_type"].casefold()]
            assert actual_job_type == case["job_type"]
            args = NS(jobType="TableMaintenance")
            props.add_item_props_to_args(args, NS(
                workspace=NS(id="ws"), id="item", type="item", name="item",
                job_type=NS(value=actual_job_type),
            ))
            assert args.jobType == case["job_type"], "source nested job handler must overwrite initial type"
    print("TABLE actual hierarchy support: 32 leaf checks and 8 nested job checks passed")


if __name__ == "__main__":
    main()

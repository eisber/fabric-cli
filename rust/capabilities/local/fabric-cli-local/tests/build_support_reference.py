# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Extract a pinned, offline support/URI snapshot, never native-derived output."""

from __future__ import annotations

import ast
from enum import Enum
import json
from pathlib import Path
from types import SimpleNamespace

from source_contract import PIN, source


def mapping_yaml(text: str) -> dict:
    """Parse only the pinned YAML's mapping/string-sequence subset, or fail."""
    lines = []
    for raw in text.splitlines():
        value = raw.split("#", 1)[0].rstrip()
        if value.strip() and value.strip() != "---":
            lines.append((len(value) - len(value.lstrip()), value.strip()))

    def block(index: int, indent: int) -> tuple[dict | list, int]:
        sequence = lines[index][1].startswith("- ")
        result = [] if sequence else {}
        while index < len(lines) and lines[index][0] == indent:
            value = lines[index][1]
            index += 1
            if sequence:
                assert value.startswith("- ")
                result.append(value[2:])
                continue
            assert value.endswith(":") and not value.startswith("- ")
            key = value[:-1]
            assert key not in result
            if index < len(lines) and lines[index][0] > indent:
                child, index = block(index, lines[index][0])
            else:
                child = None
            result[key] = child
        return result, index

    result, consumed = block(0, 0)
    assert consumed == len(lines) and isinstance(result, dict)
    return result


def snapshot() -> dict:
    """Translate the actual source enums and support properties into JSON."""
    type_tree = ast.parse(source("core/fab_types.py"))
    type_names = (
        "FabricElementType", "ItemType", "VirtualItemType",
        "VirtualWorkspaceItemType", "WorkspaceType",
    )
    canonical = {}
    member_values = {}
    for name in type_names:
        node = next(node for node in type_tree.body if isinstance(node, ast.ClassDef) and node.name == name)
        values = {}
        for assignment in node.body:
            if isinstance(assignment, ast.Assign) and isinstance(assignment.targets[0], ast.Name):
                key = assignment.targets[0].id
                value = assignment.value.value if isinstance(assignment.value, ast.Constant) else key.title().replace("_", "")
                values[key] = value
        canonical[name] = list(values.values())
        member_values[name] = values

    command_tree = ast.parse(source("core/fab_commands.py"))
    command_node = next(node for node in command_tree.body if isinstance(node, ast.ClassDef) and node.name == "Command")
    command_node.body = [node for node in command_node.body if isinstance(node, ast.Assign)]
    namespace = {"Enum": Enum, "platform": SimpleNamespace(system=lambda: "Linux")}
    exec(compile(ast.Module(body=[command_node], type_ignores=[]), "source_commands", "exec"), namespace)
    commands = namespace["Command"]
    support = mapping_yaml(source("core/fab_config/command_support.yaml"))["commands"]

    function_node = next(node for node in command_tree.body if isinstance(node, ast.FunctionDef) and node.name == "_get_property_from_commands_and_subcommands")
    namespace["Command"] = commands
    namespace["get_command_support_dict"] = lambda: {"commands": support}
    exec(compile(ast.Module(body=[function_node], type_ignores=[]), "source_support", "exec"), namespace)
    properties = namespace["_get_property_from_commands_and_subcommands"]

    def normalized(values: list[str], allowed: list[str]) -> list[str]:
        result = []
        for value in values:
            camel = value.title().replace("_", "")
            found = next((name for name in allowed if name.lower() == camel.lower()), None)
            if found:
                result.append(found)
        return result

    items = canonical["ItemType"] + canonical["VirtualItemType"] + canonical["VirtualWorkspaceItemType"]
    policies = []
    for command in commands:
        if command.name.split("_")[0].lower() not in support:
            continue
        policies.append({
            "command": command.value,
            "supported_elements": normalized(properties(command, "supported_elements"), canonical["FabricElementType"]),
            "supported_items": normalized(properties(command, "supported_items"), items),
            "unsupported_items": normalized(properties(command, "unsupported_items"), items),
        })
    uri_node = next(node for node in type_tree.body if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id == "uri_mapping" for target in node.targets))
    uris = {
        member_values["ItemType"][key.attr]: ast.literal_eval(value)
        for key, value in zip(uri_node.value.keys, uri_node.value.values)
    }
    return {
        "source_commit": PIN,
        "elements": [".Folder"] + [
            f".{value}" for name in ("ItemType", "VirtualItemType", "VirtualWorkspaceItemType", "WorkspaceType")
            for value in canonical[name] if value != "Personal"
        ],
        "policies": policies,
        "uris": uris,
    }


def main() -> None:
    """Write the reviewed source snapshot only when explicitly invoked."""
    target = Path(__file__).resolve().parents[1] / "src" / "support.json"
    target.write_text(json.dumps(snapshot(), indent=2) + "\n", encoding="utf-8")
    print(f"Extracted pinned source support to {target}")


if __name__ == "__main__":
    main()

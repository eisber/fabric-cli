# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Inventory source registrations without importing product code or credentials."""

from __future__ import annotations

import ast
import json
from pathlib import Path
from typing import Any


def _literal(node: ast.AST, bindings: dict[str, Any]) -> Any:
    if isinstance(node, ast.Name) and node.id in bindings:
        return bindings[node.id]
    return ast.literal_eval(node)


def _argument(call: ast.Call, bindings: dict[str, Any]) -> dict[str, Any]:
    shape = {"names": [_literal(arg, bindings) for arg in call.args]}
    for keyword in call.keywords:
        if keyword.arg not in {"help", "metavar"}:
            try:
                value = _literal(keyword.value, bindings)
            except (ValueError, TypeError):
                value = {"expression": ast.unparse(keyword.value)}
            shape[keyword.arg] = value
    return shape


def inventory(root: Path) -> dict[str, Any]:
    """Read the actually called parser registrations and their argument shapes."""
    package = root / "src" / "fabric_cli"
    setup_path = package / "core" / "fab_parser_setup.py"
    setup = ast.parse(setup_path.read_text(encoding="utf-8"))
    imports = {
        alias.asname or alias.name: alias.name
        for node in setup.body
        if isinstance(node, ast.ImportFrom) and node.module == "fabric_cli.parsers"
        for alias in node.names
    }
    create = next(
        node
        for node in setup.body
        if isinstance(node, ast.FunctionDef)
        and node.name == "create_parser_and_subparsers"
    )
    rows: list[dict[str, Any]] = []

    def registrations(path: Path, function: ast.FunctionDef) -> None:
        tree = ast.parse(path.read_text(encoding="utf-8"))
        bindings: dict[str, Any] = {}
        parsers: dict[str, dict[str, Any]] = {}
        groups: dict[str, str] = {"subparsers": ""}
        for statement in tree.body:
            if isinstance(statement, ast.Assign) and isinstance(
                statement.value, ast.Constant
            ):
                for target in statement.targets:
                    if isinstance(target, ast.Name):
                        bindings[target.id] = statement.value.value
        for node in sorted(ast.walk(function), key=lambda n: getattr(n, "lineno", 0)):
            if isinstance(node, ast.Assign):
                try:
                    value = _literal(node.value, bindings)
                except (ValueError, TypeError):
                    pass
                else:
                    for target in node.targets:
                        if isinstance(target, ast.Name):
                            bindings[target.id] = value
                call = node.value
                target = node.targets[0]
                if not (
                    isinstance(target, ast.Name)
                    and isinstance(call, ast.Call)
                    and isinstance(call.func, ast.Attribute)
                    and isinstance(call.func.value, ast.Name)
                ):
                    continue
                receiver = call.func.value.id
                if call.func.attr == "add_subparsers" and receiver in parsers:
                    groups[target.id] = parsers[receiver]["command"]
                elif call.func.attr == "add_parser":
                    name = _literal(call.args[0], bindings)
                    prefix = groups[receiver]
                    aliases = next(
                        (
                            _literal(kw.value, bindings)
                            for kw in call.keywords
                            if kw.arg == "aliases"
                        ),
                        [],
                    )
                    row = {
                        "command": f"{prefix} {name}".strip(),
                        "aliases": [f"{prefix} {alias}".strip() for alias in aliases],
                        "source": str(path.relative_to(root)).replace("\\", "/"),
                        "line": call.lineno,
                        "registration": function.name,
                        "arguments": [],
                        "handler": None,
                    }
                    parsers[target.id] = row
                    rows.append(row)
            if not (
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Attribute)
                and isinstance(node.func.value, ast.Name)
                and node.func.value.id in parsers
            ):
                continue
            row = parsers[node.func.value.id]
            if node.func.attr == "add_argument":
                row["arguments"].append(_argument(node, bindings))
            elif node.func.attr == "set_defaults":
                for keyword in node.keywords:
                    if keyword.arg == "func":
                        handler = keyword.value
                        if (
                            isinstance(handler, ast.Call)
                            and isinstance(handler.func, ast.Name)
                            and handler.func.id == "lazy_command"
                        ):
                            row["handler"] = ".".join(
                                _literal(arg, bindings) for arg in handler.args
                            )
                        else:
                            row["handler"] = ast.unparse(handler)

    for statement in create.body:
        call = statement.value if isinstance(statement, ast.Expr) else None
        if (
            isinstance(call, ast.Call)
            and isinstance(call.func, ast.Attribute)
            and isinstance(call.func.value, ast.Name)
            and call.func.value.id in imports
        ):
            path = package / "parsers" / f"{imports[call.func.value.id]}.py"
            tree = ast.parse(path.read_text(encoding="utf-8"))
            function = next(
                node
                for node in tree.body
                if isinstance(node, ast.FunctionDef) and node.name == call.func.attr
            )
            registrations(path, function)
    # The directly registered version leaf lives in the setup function itself.
    registrations(setup_path, create)
    rows.sort(key=lambda row: row["command"])
    names = [row["command"] for row in rows]
    if len(names) != len(set(names)):
        raise ValueError("Duplicate canonical command registration")
    for row in rows:
        row["kind"] = (
            "group"
            if any(name.startswith(row["command"] + " ") for name in names)
            else "leaf"
        )
        if row["handler"] is None:
            raise ValueError(f"Missing command handler: {row['command']}")
    return {
        "python_source_commit": "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4",
        "method": "AST of create_parser_and_subparsers and called registrations; no product imports",
        "groups": sum(row["kind"] == "group" for row in rows),
        "leaves": sum(row["kind"] == "leaf" for row in rows),
        "root_arguments": [
            _argument(node, {})
            for node in ast.walk(create)
            if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id == "parser"
            and node.func.attr == "add_argument"
        ],
        "global_arguments": [
            _argument(node, {})
            for node in ast.walk(
                ast.parse(
                    (package / "parsers" / "fab_global_params.py").read_text(
                        encoding="utf-8"
                    )
                )
            )
            if isinstance(node, ast.Call)
            and isinstance(node.func, ast.Attribute)
            and node.func.attr == "add_argument"
        ],
        "commands": rows,
    }


if __name__ == "__main__":
    print(json.dumps(inventory(Path(__file__).resolve().parents[4]), indent=2))

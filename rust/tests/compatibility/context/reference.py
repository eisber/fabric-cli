# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Check hand-authored remote context cases against the actual pinned Python AST."""

from __future__ import annotations

import __future__
import ast
import builtins
import contextlib
import hashlib
import html
import io
import json
import os
import re
import socket
import subprocess
import sys
import time
from abc import ABC, abstractmethod
from collections import deque
from enum import Enum, auto
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Callable, Iterator
from unittest.mock import patch


PINNED_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
_DENIAL_MESSAGE = "Real filesystem/auth/network/process/sleep is forbidden"
_GUARDED = False


def _deny(*args: Any, **kwargs: Any) -> Any:
    raise AssertionError(_DENIAL_MESSAGE)


def _audit(event: str, args: tuple[Any, ...]) -> None:
    if _GUARDED and (
        event in {"open", "import", "builtins.input", "builtins.breakpoint"}
        or event.startswith(
            ("os.", "socket.", "subprocess.", "ctypes.", "winreg.", "mmap.")
        )
    ):
        _deny()


class _Unavailable:
    def __getattr__(self, name: str) -> Any:
        return _deny()

    def __call__(self, *args: Any, **kwargs: Any) -> Any:
        return _deny()

    def __getitem__(self, key: Any) -> Any:
        return _deny()


@contextlib.contextmanager
def _algorithm_guard() -> Iterator[None]:
    """Deny side effects and ambient discovery while product algorithms run."""
    global _GUARDED
    if _GUARDED:
        raise AssertionError("Nested oracle guard")
    with contextlib.ExitStack() as stack:
        for module, names in (
            (builtins, ("open", "input", "breakpoint")),
            (io, ("open", "open_code")),
            (
                os,
                (
                    "access", "chdir", "chmod", "chown", "close", "fdopen", "fork",
                    "fstat", "getcwd", "getcwdb", "getenv", "kill", "link", "listdir",
                    "lstat", "mkdir", "makedirs", "open", "popen", "read", "readlink",
                    "remove", "removedirs", "rename", "renames", "replace", "rmdir",
                    "scandir", "spawnl", "spawnle", "spawnlp", "spawnlpe", "spawnv",
                    "spawnve", "spawnvp", "spawnvpe", "stat", "symlink", "system",
                    "truncate", "unlink", "utime", "walk", "write",
                ),
            ),
            (socket, ("socket", "socketpair", "create_connection", "getaddrinfo")),
            (subprocess, ("Popen", "run", "call", "check_call", "check_output")),
            (time, ("sleep",)),
        ):
            for name in names:
                if hasattr(module, name):
                    stack.enter_context(patch.object(module, name, _deny))
        stack.enter_context(patch.object(os, "environ", _Unavailable()))
        _GUARDED = True
        try:
            yield
        finally:
            _GUARDED = False


def _check_guard_denials(namespace: dict[str, Any]) -> None:
    """Require guard failures before any probe can perform a real operation."""
    product_import = namespace["__builtins__"]["__import__"]
    probes: tuple[tuple[str, Callable[[], Any]], ...] = (
        ("filesystem open", lambda: builtins.open("context-oracle-denied", "rb")),
        ("filesystem io.open", lambda: io.open("context-oracle-denied", "rb")),
        ("filesystem stat", lambda: os.stat("context-oracle-denied")),
        ("filesystem access", lambda: os.access("context-oracle-denied", os.R_OK)),
        ("environment mapping", lambda: os.environ["CONTEXT_ORACLE_DENIED"]),
        ("environment get", lambda: os.environ.get("CONTEXT_ORACLE_DENIED")),
        ("environment getenv", lambda: os.getenv("CONTEXT_ORACLE_DENIED")),
        ("socket", lambda: socket.socket()),
        (
            "subprocess",
            lambda: subprocess.run(["context-oracle-denied-executable"], check=False),
        ),
        ("sleep", lambda: time.sleep(0)),
        (
            "controlled product import",
            lambda: product_import(
                "fabric_cli.core.fab_handle_context", fromlist=("*",)
            ),
        ),
        (
            "authentication import",
            lambda: product_import("fabric_cli.core.fab_auth", fromlist=("FabAuth",)),
        ),
        (
            "network dependency",
            lambda: namespace["onelake_api"].get_properties(None),
        ),
        (
            "filesystem audit event",
            lambda: sys.audit("open", "context-oracle-denied", "r", 0),
        ),
    )
    with _algorithm_guard():
        for label, probe in probes:
            try:
                probe()
            except AssertionError as error:
                if str(error) != _DENIAL_MESSAGE:
                    raise AssertionError(
                        f"Guard probe failed for an unrelated reason: {label}"
                    ) from error
            else:
                raise AssertionError(f"Guard did not deny: {label}")
    print(f"PASS {len(probes)} guard-negative checks")


def _compile_nodes(tree: ast.Module, names: set[str], filename: str) -> Any:
    selected: list[ast.stmt] = []
    found: set[str] = set()
    for node in tree.body:
        name = getattr(node, "name", None)
        if isinstance(node, ast.Assign) and len(node.targets) == 1:
            name = getattr(node.targets[0], "id", None)
        if isinstance(node, ast.AnnAssign):
            name = getattr(node.target, "id", None)
        if name in names:
            selected.append(node)
            found.add(name)
    if found != names:
        raise AssertionError(f"Missing source nodes in {filename}: {names - found}")
    module = ast.Module(body=selected, type_ignores=[])
    return compile(
        ast.fix_missing_locations(module),
        filename,
        "exec",
        flags=__future__.annotations.compiler_flag,
        dont_inherit=True,
    )


def _source_namespace(trees: dict[str, ast.Module]) -> dict[str, Any]:
    namespace: dict[str, Any] = {
        "__name__": "offline_context_reference",
        "ABC": ABC,
        "abstractmethod": abstractmethod,
        "Enum": Enum,
        "auto": auto,
        "re": re,
        "json": json,
        "html": html,
        "deque": deque,
    }

    def controlled_import(
        name: str,
        globals: Any = None,
        locals: Any = None,
        fromlist: Any = (),
        level: int = 0,
    ) -> Any:
        if name == "fabric_cli.core.fab_context" and fromlist == ("Context",):
            return namespace["_context_module"]
        return _deny()

    namespace["__builtins__"] = {
        **vars(builtins), "__import__": controlled_import,
        "open": _deny, "input": _deny, "breakpoint": _deny,
    }

    def load(path: str, names: set[str]) -> None:
        exec(_compile_nodes(trees[path], names, path), namespace)

    constants: dict[str, Any] = {}
    for node in trees["src/fabric_cli/core/fab_constant.py"].body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name) and target.id.startswith(
                    ("ERROR_", "WARNING_")
                ):
                    constants[target.id] = ast.literal_eval(node.value)
    namespace["fab_constant"] = SimpleNamespace(**constants)
    load(
        "src/fabric_cli/core/fab_exceptions.py",
        {"DEFAULT_ERROR_MESSAGE", "DEFAULT_ERROR_CODE", "FabricCLIError"},
    )
    load("src/fabric_cli/errors/common.py", {"CommonErrors"})
    load("src/fabric_cli/errors/hierarchy.py", {"HierarchyErrors"})
    namespace["ErrorMessages"] = SimpleNamespace(
        Common=namespace["CommonErrors"], Hierarchy=namespace["HierarchyErrors"]
    )
    load(
        "src/fabric_cli/core/fab_types.py",
        {
            "FabricElementType", "WorkspaceType", "VirtualWorkspaceType",
            "_BaseItemType", "VirtualWorkspaceItemType", "VirtualItemContainerType",
            "VirtualItemType", "ItemType", "OneLakeItemType", "FabricJobType",
            "LakehouseFolders", "VWIMap",
        },
    )
    for file, names in (
        ("fab_element.py", {"FabricElement"}),
        ("fab_tenant.py", {"Tenant"}),
        ("fab_workspace.py", {"_BaseWorkspace", "Workspace", "VirtualWorkspace"}),
        ("fab_folder.py", {"Folder"}),
        ("fab_base_item.py", {"_BaseItem"}),
        ("fab_item.py", {"Item"}),
    ):
        load(f"src/fabric_cli/core/hiearchy/{file}", names)
    for name in (
        "VirtualItem", "VirtualItemContainer", "VirtualWorkspaceItem", "OneLakeItem",
        "LocalPath",
    ):
        namespace[name] = type(name, (), {})
    for name in (
        "os", "fab_logger", "utils", "utils_ui", "onelake_api", "Command", "cmd"
    ):
        namespace[name] = _Unavailable()
    load(
        "src/fabric_cli/core/fab_handle_context.py",
        {
            "get_command_context", "process_path_part", "process_context_path",
            "process_absolute_path", "process_relative_path", "_get_workspace_id",
            "_get_folder_id", "_get_nested_folder_id", "_get_item_id",
            "_handle_path_in_tenant", "_handle_path_in_ws", "_handle_path_in_folder",
        },
    )
    for name in (
        "_handle_path_in_item", "_handle_path_in_onelake", "_handle_path_in_vws",
        "_handle_path_in_vic",
    ):
        namespace[name] = _deny
    return namespace


def _snapshot(element: Any) -> dict[str, Any]:
    return {
        "path": element.path,
        "path_id": element.path_id,
        "kind": str(element.type),
        "name": element.name,
        "id": element.id,
    }


def _catalog(
    namespace: dict[str, Any], records: list[dict[str, Any]]
) -> dict[str, Any]:
    nodes: dict[str, Any] = {}
    for record in records:
        kind = record["kind"]
        parent = nodes.get(record.get("parent"))
        args = [record["name"], record["id"]]
        if kind != "Tenant":
            args.append(parent)
        if kind in {"Workspace", "Item"}:
            args.append(record["type"])
        nodes[record["key"]] = namespace[kind](*args)
    return nodes


def _store(
    namespace: dict[str, Any],
    tree: ast.Module,
    nodes: dict[str, Any],
    failures: list[dict[str, Any]],
    lookups: list[dict[str, str]],
) -> Any:
    """Fake only collection/caller boundaries; execute real matching and errors."""
    mem = dict(namespace)
    tenant_type = namespace["Tenant"]
    workspace_type = namespace["Workspace"]
    folder_type = namespace["Folder"]
    item_type = namespace["Item"]
    mem["get_workspaces"] = lambda tenant: [
        n for n in nodes.values()
        if isinstance(n, workspace_type) and n.parent == tenant
    ]
    mem["get_workspace_folders"] = lambda workspace: [
        n for n in nodes.values()
        if isinstance(n, folder_type) and n.workspace == workspace
    ]
    mem["get_workspace_items"] = lambda workspace: [
        n for n in nodes.values()
        if isinstance(n, item_type) and n.workspace == workspace
    ]
    cache = SimpleNamespace(cache=SimpleNamespace(clear=lambda: None))
    for name in (
        "_get_workspaces_from_cache", "_get_workspace_folders_from_cache",
        "_get_workspace_items_from_cache",
    ):
        mem[name] = cache
    functions = {
        "_get_workspace_id_from_api", "get_workspace_id", "_get_folder_id",
        "get_folder_id", "_get_nested_folder_id", "get_nested_folder_id",
        "_get_item_id", "get_item_id",
    }
    exec(
        _compile_nodes(tree, functions, "src/fabric_cli/utils/fab_mem_store.py"), mem
    )

    def boundary(op: str, function: Any) -> Any:
        def lookup(parent: Any, name: str) -> Any:
            if op == "workspace" and not isinstance(parent, tenant_type):
                raise AssertionError("Workspace lookup parent must be tenant")
            call = {"op": op, "parent": parent.path, "name": name}
            lookups.append(call)
            for failure in failures:
                if all(failure[key] == value for key, value in call.items()):
                    error = failure["error"]
                    raise namespace["FabricCLIError"](error["message"], error["code"])
            return function(parent, name)

        return lookup

    return SimpleNamespace(
        **{
            f"get_{op}_id": boundary(op, mem[f"get_{op}_id"])
            for op in ("workspace", "folder", "nested_folder", "item")
        }
    )


def _run_case(
    namespace: dict[str, Any],
    mem_tree: ast.Module,
    records: list[dict[str, Any]],
    case: dict[str, Any],
) -> dict[str, Any]:
    nodes = _catalog(namespace, records)
    current = nodes[case["current"]]
    before = {key: _snapshot(node) for key, node in nodes.items()}
    parent_links = {key: node.parent for key, node in nodes.items()}
    lookups: list[dict[str, str]] = []
    namespace["mem_store"] = _store(
        namespace, mem_tree, nodes, case.get("failures", []), lookups
    )
    context = SimpleNamespace(context=current, get_tenant=lambda: nodes["tenant"])
    namespace["_context_module"] = SimpleNamespace(Context=lambda: context)
    namespace["get_personal_workspace_name"] = lambda: case["home"]
    observed: dict[str, Any] = {"current_before": case["current"]}
    try:
        result = namespace["get_command_context"](
            case["input"],
            raise_error=case["raise_error"],
            supports_local_path=case["supports_local_path"],
        )
        observed["result"] = _snapshot(result)
    except namespace["FabricCLIError"] as error:
        observed["error"] = {"code": error.status_code, "message": error.message}
    observed["lookups"] = lookups
    observed["current_after"] = case["current"]
    if context.context is not current:
        raise AssertionError("Resolution changed the caller's current context")
    if before != {key: _snapshot(node) for key, node in nodes.items()}:
        raise AssertionError("Resolution mutated the catalog")
    if any(nodes[key].parent is not parent for key, parent in parent_links.items()):
        raise AssertionError("Resolution changed a catalog parent")
    return observed


def check_reference() -> None:
    """Check source hashes, bounded cases, exact traces, and caller immutability."""
    directory = Path(__file__).resolve().parent
    root = directory.parents[3]
    fixture = json.loads((directory / "cases.json").read_text(encoding="utf-8"))
    if fixture["schema_version"] != 1 or fixture["source_commit"] != PINNED_COMMIT:
        raise AssertionError("Unexpected fixture schema or source commit")
    trees: dict[str, ast.Module] = {}
    for relative, expected_hash in fixture["source_sha256"].items():
        path = root.joinpath(*relative.split("/"))
        raw = path.read_bytes()
        if hashlib.sha256(raw).hexdigest() != expected_hash:
            raise AssertionError(
                f"Pinned Python source drift requires review: {relative}"
            )
        trees[relative] = ast.parse(raw.decode("utf-8"), filename=relative)
    namespace = _source_namespace(trees)
    sys.addaudithook(_audit)
    _check_guard_denials(namespace)
    exact = 0
    unsupported = 0
    names: set[str] = set()
    for case in fixture["cases"]:
        if case["name"] in names:
            raise AssertionError(f"Duplicate case name: {case['name']}")
        names.add(case["name"])
        if "unsupported" in case["expected"]:
            if not case.get("source_behavior"):
                raise AssertionError("Unsupported case needs source classification")
            unsupported += 1
            print(f"UNSUPPORTED {case['name']}: {case['expected']['unsupported']}")
            continue
        if case["supports_local_path"] or not isinstance(case["home"], str):
            raise AssertionError("An exact case escaped the bounded remote contract")
        with _algorithm_guard():
            observed = _run_case(
                namespace,
                trees["src/fabric_cli/utils/fab_mem_store.py"],
                fixture["catalog"],
                case,
            )
        if observed != case["expected"]:
            raise AssertionError(
                f"{case['name']}:\nobserved={observed!r}\n"
                f"expected={case['expected']!r}"
            )
        exact += 1
        print(f"PASS {case['name']}")
    if exact < 40:
        raise AssertionError("At least 40 independently hand-authored cases required")
    print(f"{exact} exact cases; {unsupported} classified unsupported cases")


if __name__ == "__main__":
    check_reference()

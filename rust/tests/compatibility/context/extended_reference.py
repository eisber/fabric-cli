# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Check extended hand-authored context fixtures using pinned product AST nodes.

This is an additive development oracle, not a shipped Python runtime bridge.
Source loading and hash verification precede the side-effect-denying guard.
"""

from __future__ import annotations

import ast
import builtins
import contextlib
import hashlib
import importlib.util
import json
import ntpath
import posixpath
import re
import sys
from argparse import Namespace
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any, Iterator
from unittest.mock import patch


PINNED_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"

# This projection is checked against the actual hashed YAML before use. The
# product's registry helpers and hierarchy command-support methods still run.
_FS_LS_REGISTRY = {
    "commands": {
        "fs": {
            "subcommands": {
                "ls": {
                    "supported_elements": [
                        "tenant",
                        "workspace",
                        "folder",
                        "virtual_workspace",
                        "virtual_item_container",
                        "onelake",
                    ],
                    "supported_items": [
                        "lakehouse",
                        "spark_job_definition",
                        "warehouse",
                        "kql_database",
                        "mirrored_database",
                        "semantic_model",
                        "capacity",
                        "domain",
                        "spark_pool",
                        "managed_identity",
                        "managed_private_endpoint",
                        "external_data_share",
                        "connection",
                        "gateway",
                        "sql_database",
                        "cosmos_db_database",
                        "graph_ql_api",
                    ],
                }
            }
        }
    }
}


def _load_base(directory: Path) -> ModuleType:
    """Load the unchanged sibling oracle before the product execution guard."""
    spec = importlib.util.spec_from_file_location(
        "_extended_context_base_reference", directory / "reference.py"
    )
    if spec is None or spec.loader is None:
        raise AssertionError("Cannot load the existing context oracle")
    module = importlib.util.module_from_spec(spec)
    old_bytecode = sys.dont_write_bytecode
    sys.dont_write_bytecode = True
    try:
        spec.loader.exec_module(module)
    finally:
        sys.dont_write_bytecode = old_bytecode
    return module


def _verify_registry(raw: str) -> None:
    """Verify the entire bounded FS_LS projection without loading YAML code."""
    raw = raw.replace("\r\n", "\n")
    match = re.search(
        r"(?m)^  fs:\n    subcommands:\n      ls:\n(.*?)^      rm:",
        raw,
        re.DOTALL,
    )
    if match is None:
        raise AssertionError("Cannot identify the pinned FS_LS registry block")
    projection: dict[str, list[str]] = {}
    key: str | None = None
    for line in match.group(1).splitlines():
        if not line.strip():
            continue
        key_match = re.fullmatch(r"        ([a-z_]+):\s*", line)
        value_match = re.fullmatch(r"          - ([a-z_]+)\s*", line)
        if key_match is not None:
            key = key_match.group(1)
            if key in projection:
                raise AssertionError("Duplicate FS_LS registry property")
            projection[key] = []
        elif value_match is not None and key is not None:
            projection[key].append(value_match.group(1))
        else:
            raise AssertionError(f"Unreviewed FS_LS registry syntax: {line!r}")
    if projection != _FS_LS_REGISTRY["commands"]["fs"]["subcommands"]["ls"]:
        raise AssertionError("The source-verified FS_LS projection has drifted")


def _source_namespace(
    base: ModuleType, trees: dict[str, ast.Module], mode: str
) -> dict[str, Any]:
    namespace = base._source_namespace(trees)

    def load(path: str, names: set[str]) -> None:
        exec(base._compile_nodes(trees[path], names, path), namespace)

    load(
        "src/fabric_cli/core/fab_types.py",
        {
            "VICMap",
            "WarehouseFolders",
            "SemanticModelFolders",
            "SparkJobDefinitionFolders",
            "KQLDatabaseFolders",
            "SQLDatabaseFolders",
            "CosmosDBDatabaseFolders",
            "MirroredDatabaseFolders",
            "ItemFoldersMap",
        },
    )
    for filename, name in (
        ("fab_virtual_item_container.py", "VirtualItemContainer"),
        ("fab_virtual_item.py", "VirtualItem"),
        ("fab_virtual_workspace_item.py", "VirtualWorkspaceItem"),
        ("fab_onelake_element.py", "OneLakeItem"),
        ("fab_local_path.py", "LocalPath"),
    ):
        load(f"src/fabric_cli/core/hiearchy/{filename}", {name})
    namespace["platform"] = SimpleNamespace(
        system=lambda: "Windows" if mode == "windows" else "Linux"
    )
    load(
        "src/fabric_cli/core/fab_commands.py",
        {
            "Command",
            "_snake_to_camel",
            "get_command_support_dict",
            "_get_property_from_commands_and_subcommands",
            "get_supported_elements",
            "_get_items",
            "get_supported_items",
            "get_unsupported_items",
        },
    )
    namespace["mem_dictionary"] = _FS_LS_REGISTRY
    namespace["cmd"] = SimpleNamespace(
        **{
            name: namespace[name]
            for name in (
                "get_supported_elements",
                "get_supported_items",
                "get_unsupported_items",
            )
        }
    )
    namespace["Namespace"] = Namespace
    load("src/fabric_cli/utils/fab_util.py", {"remove_dot_suffix"})
    load(
        "src/fabric_cli/utils/fab_item_util.py", {"get_item_name_from_eds_name"}
    )
    namespace["utils"] = SimpleNamespace(
        remove_dot_suffix=namespace["remove_dot_suffix"]
    )
    load(
        "src/fabric_cli/core/fab_handle_context.py",
        {
            "_get_capacity_id",
            "_get_domain_id",
            "_get_connection_id",
            "_get_gateway_id",
            "_get_spark_pool_id",
            "_get_managed_identity_id",
            "_get_managed_private_endpoint_id",
            "_get_external_data_share_id",
            "_handle_path_in_vws",
            "_handle_path_in_vic",
            "_handle_path_in_item",
            "_handle_path_in_onelake",
            "_get_onelake_details",
        },
    )
    namespace["hash"] = base._deny
    return namespace


@contextlib.contextmanager
def _guard(base: ModuleType) -> Iterator[None]:
    """Additionally deny cached real imports; only synthetic Context is allowed."""
    with base._algorithm_guard():
        with patch.object(builtins, "__import__", base._deny):
            yield


def _snapshot(element: Any) -> dict[str, Any]:
    snapshot = {
        "path": element.path,
        "path_id": element.path_id,
        "kind": str(element.type),
        "name": element.name,
        "id": element.id,
    }
    kind = snapshot["kind"]
    if kind == "Workspace":
        snapshot["subtype"] = str(element.ws_type)
    elif kind == "VirtualWorkspace":
        snapshot["subtype"] = str(element.vws_type)
    elif kind == "VirtualItemContainer":
        snapshot["subtype"] = str(element.vic_type)
    elif kind in {"Item", "VirtualItem", "VirtualWorkspaceItem"}:
        snapshot["subtype"] = str(element.item_type)
    elif kind == "Onelake":
        snapshot.update(
            subtype=element.nested_type.name,
            local_path=element.local_path,
            root_folder=element.root_folder,
        )
    return snapshot


def _catalog(
    namespace: dict[str, Any], records: list[dict[str, Any]]
) -> dict[str, Any]:
    nodes: dict[str, Any] = {}
    for record in records:
        kind = record["kind"]
        args = [record["name"], record["id"]]
        if kind != "Tenant":
            args.append(nodes[record["parent"]])
        if kind in {"Workspace", "Item", "VirtualItem", "VirtualWorkspaceItem"}:
            args.append(record["type"])
        elif kind == "OneLakeItem":
            args.append(namespace["OneLakeItemType"][record["subtype"]])
        nodes[record["key"]] = namespace[kind](*args)
    return nodes


def _store(
    base: ModuleType,
    namespace: dict[str, Any],
    tree: ast.Module,
    nodes: dict[str, Any],
    failures: list[dict[str, Any]],
    calls: list[dict[str, Any]],
) -> Any:
    """Run source lookup matching over injected collections, never real APIs."""
    remote = base._store(namespace, tree, nodes, failures, calls)
    mem = dict(namespace)
    mem["get_item_id"] = remote.get_item_id
    mem["item_utils"] = SimpleNamespace(
        get_item_name_from_eds_name=namespace["get_item_name_from_eds_name"]
    )
    kinds = {
        "capacity": ("get_capacities", "Capacity", "VirtualWorkspaceItem"),
        "domain": ("get_domains", "Domain", "VirtualWorkspaceItem"),
        "connection": ("get_connections", "Connection", "VirtualWorkspaceItem"),
        "gateway": ("get_gateways", "Gateway", "VirtualWorkspaceItem"),
        "spark_pool": ("get_spark_pools", "SparkPool", "VirtualItem"),
        "managed_identity": (
            "get_managed_identities", "ManagedIdentity", "VirtualItem"
        ),
        "managed_private_endpoint": (
            "get_managed_private_endpoints", "ManagedPrivateEndpoint", "VirtualItem"
        ),
    }

    def collection(subtype: str, kind: str) -> Any:
        def get(parent: Any) -> list[Any]:
            return [
                node
                for node in nodes.values()
                if isinstance(node, namespace[kind])
                and str(node.item_type) == subtype
                and (
                    node.tenant.path == parent.path
                    if kind == "VirtualWorkspaceItem"
                    else node.parent.path == parent.path
                )
            ]

        return get

    functions = {"_get_external_data_share_id", "get_external_data_share_id"}
    for op, (function, subtype, kind) in kinds.items():
        mem[function] = collection(subtype, kind)
        functions.update({f"_get_{op}_id", f"get_{op}_id"})
    for name in (
        "_get_spark_pools_from_cache",
        "_get_managed_identities_from_cache",
        "_get_managed_private_endpoints_from_cache",
    ):
        mem[name] = SimpleNamespace(cache=SimpleNamespace(clear=lambda: None))

    def shares(workspace: Any, item: Any) -> list[Any]:
        return [
            node
            for node in nodes.values()
            if isinstance(node, namespace["VirtualItem"])
            and str(node.item_type) == "ExternalDataShare"
            and node.workspace.path == workspace.path
            and namespace["get_item_name_from_eds_name"](node.name) == item.name
        ]

    mem["get_external_data_shares_for_item"] = shares
    exec(
        base._compile_nodes(
            tree, functions, "src/fabric_cli/utils/fab_mem_store.py"
        ),
        mem,
    )

    def boundary(op: str) -> Any:
        def lookup(parent: Any, name: str) -> Any:
            call = {"op": op, "parent": parent.path, "name": name}
            calls.append(call)
            for failure in failures:
                if all(failure[key] == value for key, value in call.items()):
                    error = failure["error"]
                    raise namespace["FabricCLIError"](
                        error["message"], error["code"]
                    )
            return mem[f"get_{op}_id"](parent, name)

        return lookup

    return SimpleNamespace(
        **vars(remote),
        **{
            f"get_{op}_id": boundary(op)
            for op in (*kinds, "external_data_share")
        },
    )


class _Responses:
    """Strict ordered metadata boundary; absent scripted operations fail closed."""

    def __init__(
        self,
        namespace: dict[str, Any],
        script: list[dict[str, Any]],
        calls: list[dict[str, Any]],
    ) -> None:
        self.namespace = namespace
        self.script = list(script)
        self.calls = calls

    def properties(self, args: Any) -> Any:
        """Return one caller-supplied OneLake metadata response."""
        call = {"op": "properties", "path": args.from_path}
        self.calls.append(call)
        if not self.script:
            raise AssertionError(f"Unscripted metadata request: {call!r}")
        response = self.script.pop(0)
        if response["path"] != args.from_path:
            raise AssertionError(f"Metadata path differs: {call!r} vs {response!r}")
        if "error" in response:
            error = response["error"]
            raise self.namespace["FabricCLIError"](
                error["message"], error["code"]
            )
        return SimpleNamespace(
            status_code=response["status_code"], headers=dict(response["headers"])
        )

    def metadata(self, op: str, arg: Any, mode: str | None = None) -> bool:
        """Return one caller-supplied local exists/access value, retaining bools."""
        call = {"op": op, "arg": arg}
        if mode is not None:
            call["mode"] = mode
        self.calls.append(call)
        if not self.script:
            raise AssertionError(f"Unscripted local metadata request: {call!r}")
        response = self.script.pop(0)
        expected_call = {
            key: value for key, value in response.items() if key != "result"
        }
        if call != expected_call or type(arg) is not type(response["arg"]):
            raise AssertionError(f"Local metadata differs: {call!r} vs {response!r}")
        if type(response["result"]) is not bool:
            raise AssertionError("Local metadata responses must be booleans")
        return response["result"]

    def complete(self) -> None:
        """Reject unused script entries, including entries after a source error."""
        if self.script:
            raise AssertionError(f"Unused metadata script: {self.script!r}")


def _run_case(
    base: ModuleType,
    namespace: dict[str, Any],
    trees: dict[str, ast.Module],
    fixture: dict[str, Any],
    case: dict[str, Any],
) -> dict[str, Any]:
    nodes = _catalog(namespace, fixture["catalog"])
    current_key = case["current"]
    current = nodes[current_key]
    before = {key: _snapshot(node) for key, node in nodes.items()}
    if before[current_key] != fixture["current_snapshots"][current_key]:
        raise AssertionError(f"Hand-authored current snapshot differs: {current_key}")
    parents = {key: node.parent for key, node in nodes.items()}
    calls: list[dict[str, Any]] = []
    warnings: list[str] = []
    notices: list[str] = []
    namespace["mem_store"] = _store(
        base,
        namespace,
        trees["src/fabric_cli/utils/fab_mem_store.py"],
        nodes,
        case.get("failures", []),
        calls,
    )
    context = SimpleNamespace(context=current, get_tenant=lambda: nodes["tenant"])
    namespace["_context_module"] = SimpleNamespace(Context=lambda: context)

    def home() -> str:
        injected = case["home"]
        if isinstance(injected, dict):
            error = injected["error"]
            raise namespace["FabricCLIError"](error["message"], error["code"])
        return injected

    namespace["get_personal_workspace_name"] = home
    namespace["fab_logger"] = SimpleNamespace(log_warning=warnings.append)
    namespace["utils_ui"] = SimpleNamespace(print_grey=notices.append)
    properties = _Responses(namespace, case.get("properties", []), calls)
    namespace["onelake_api"] = SimpleNamespace(get_properties=properties.properties)
    local = case.get("local_fs", {})
    filesystem = _Responses(namespace, local.get("operations", []), calls)
    path_module = ntpath if case["local_mode"] == "windows" else posixpath

    def dirname(path: str) -> str:
        calls.append({"op": "dirname", "arg": path})
        return path_module.dirname(path)

    namespace["os"] = SimpleNamespace(
        path=SimpleNamespace(
            basename=path_module.basename,
            dirname=dirname,
            exists=lambda arg: filesystem.metadata("exists", arg),
            isfile=base._deny,
            isdir=base._deny,
            samefile=base._deny,
        ),
        access=lambda arg, mode: filesystem.metadata("access", arg, mode),
        R_OK="R_OK",
        W_OK="W_OK",
    )

    def fixed_hash(path: str) -> int:
        if not isinstance(path, str) or type(local.get("hash")) is not int:
            raise AssertionError("LocalPath needs an explicit deterministic hash")
        return local["hash"]

    namespace["hash"] = fixed_hash
    observed: dict[str, Any] = {"current_before": current_key}
    try:
        result = namespace["get_command_context"](
            case["input"],
            raise_error=case["raise_error"],
            supports_local_path=case["supports_local_path"],
        )
        observed["result"] = _snapshot(result)
    except namespace["FabricCLIError"] as error:
        observed["error"] = {"code": error.status_code, "message": error.message}
    except AssertionError as error:
        traceback = error.__traceback__
        while traceback is not None and traceback.tb_next is not None:
            traceback = traceback.tb_next
        if (
            error.args
            or traceback is None
            or traceback.tb_frame.f_code
            is not namespace["_handle_path_in_onelake"].__code__
        ):
            raise
        observed["error"] = {"code": "AssertionError", "message": ""}
    properties.complete()
    filesystem.complete()
    observed.update(
        calls=calls,
        warnings=warnings,
        notices=notices,
        current_after=current_key,
    )
    if context.context is not current:
        raise AssertionError("Resolution changed the caller's current context")
    if before != {key: _snapshot(node) for key, node in nodes.items()}:
        raise AssertionError("Resolution mutated the synthetic hierarchy")
    if any(nodes[key].parent is not parent for key, parent in parents.items()):
        raise AssertionError("Resolution changed a hierarchy parent")
    return observed


def check_reference() -> None:
    """Check source pins, real handlers, strict traces, and immutable contexts."""
    directory = Path(__file__).resolve().parent
    root = directory.parents[3]
    fixture = json.loads(
        (directory / "extended_cases.json").read_text(encoding="utf-8")
    )
    if fixture["schema_version"] != 1 or fixture["source_commit"] != PINNED_COMMIT:
        raise AssertionError("Unexpected extended fixture schema or source commit")
    trees: dict[str, ast.Module] = {}
    registry_path = "src/fabric_cli/core/fab_config/command_support.yaml"
    for relative, expected_hash in fixture["source_sha256"].items():
        raw = root.joinpath(*relative.split("/")).read_bytes()
        if hashlib.sha256(raw).hexdigest() != expected_hash:
            raise AssertionError(f"Pinned Python source drift: {relative}")
        source = raw.decode("utf-8")
        if relative == registry_path:
            _verify_registry(source)
        else:
            trees[relative] = ast.parse(source, filename=relative)
    base = _load_base(directory)
    namespaces = {
        mode: _source_namespace(base, trees, mode) for mode in ("posix", "windows")
    }
    sys.addaudithook(base._audit)
    base._check_guard_denials(namespaces["posix"])
    with _guard(base):
        try:
            builtins.__import__("os")
        except AssertionError as error:
            if str(error) != base._DENIAL_MESSAGE:
                raise
        else:
            raise AssertionError("A cached real import escaped the guard")
    names: set[str] = set()
    for authored in fixture["cases"]:
        case = {**fixture["defaults"], **authored}
        if case["name"] in names:
            raise AssertionError(f"Duplicate extended case: {case['name']}")
        names.add(case["name"])
        mode = case["local_mode"]
        if mode not in namespaces:
            raise AssertionError("Local path mode must be explicitly Windows/POSIX")
        with _guard(base):
            observed = _run_case(base, namespaces[mode], trees, fixture, case)
        if observed != case["expected"]:
            raise AssertionError(
                f"{case['name']}:\nobserved={observed!r}\n"
                f"expected={case['expected']!r}"
            )
        print(f"PASS {case['name']}")
    if len(names) < 50:
        raise AssertionError("At least 50 hand-authored extended cases required")
    print(f"{len(names)} exact extended cases; no live service/platform parity claim")


if __name__ == "__main__":
    check_reference()

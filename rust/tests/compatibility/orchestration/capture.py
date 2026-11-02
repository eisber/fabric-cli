# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Additive offline LS/GET orchestration evidence, never an oracle update mode."""

from __future__ import annotations

import argparse
import ast
import copy
import importlib.util
import json
import re
import subprocess
import sys
from abc import ABC, abstractmethod
from collections import deque
from collections.abc import Sequence
from enum import Enum, auto
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Optional, Union
from urllib.parse import urlencode, urlparse

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
PYTHON_PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
RUST_PIN = "03fab20577a1016178de227eac0ee178921bb8d0"
ORIGIN = "https://api.fabric.microsoft.com/v1/"
SCOPE = ["https://api.fabric.microsoft.com/.default"]


def support(area: str) -> Any:
    """Load unchanged independent source adapters without importing the product."""
    spec = importlib.util.spec_from_file_location(
        f"_orchestration_{area}", HERE.parent / area / "reference.py"
    )
    if spec is None or spec.loader is None:
        raise AssertionError(f"Missing existing source adapter: {area}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


COMMANDS = support("commands")
ARGUMENTS = support("query_arguments")


def reply(body: Any, status: int = 200, **headers: str) -> dict[str, Any]:
    """Construct a synthetic service response, not an expected observation."""
    return {"body": body, "status": status, "headers": headers}


def cases() -> list[dict[str, Any]]:
    """Specify a finite input inventory before running either implementation."""
    ws = reply({"value": [{"id": "w", "displayName": "Demo", "type": "Workspace"}]})
    items = reply({"value": [{"id": "n", "displayName": "Book", "type": "Notebook"}]})
    metadata = reply({"id": "n", "displayName": "Book", "description": "offline"})
    connections = reply({"value": [{"id": "connection"}]})
    schedules = reply({"value": [{"id": "schedule"}]})
    denied = reply({"errorCode": "Forbidden", "message": "synthetic"}, 403)
    empty = reply({"value": []})
    folder = reply({"value": [{"id": "f", "displayName": "Parent"}]})
    folder_items = reply(
        {
            "value": [
                {"id": "n", "displayName": "Book", "type": "Notebook", "folderId": "f"}
            ]
        }
    )
    accepted = reply({}, 202, **{"x-ms-operation-id": "op", "Retry-After": "0"})
    completed = reply(
        {"status": "Succeeded"},
        **{"Location": ORIGIN + "operations/op/result"},
    )
    primary = reply({"id": "w", "displayName": "Demo"})
    enrichment = [empty, reply({"enabled": False}), empty]
    inventory = [
        ("REQ-001", ["ls", "/"], [empty], [empty], {}),
        (
            "REQ-002",
            ["ls", "/"],
            [reply({"value": [], "continuationToken": "next"}), ws],
            [reply({"value": [], "continuationToken": "next"}), ws],
            {},
        ),
        ("REQ-003", ["ls", "Demo.Workspace"], [ws, items], [ws, items], {}),
        (
            "REQ-004",
            ["ls", "Demo.Workspace/Parent.Folder", "-l"],
            [ws, folder, folder_items],
            [ws, folder, folder_items, folder],
            {"folders": True},
        ),
        (
            "REQ-005",
            ["get", "Demo.Workspace/Book.Notebook", "-q", "description"],
            [ws, items, metadata, connections, schedules],
            [ws, items, metadata],
            {},
        ),
        (
            "REQ-006",
            ["get", "Demo.Workspace/Book.Notebook", "-q", "description"],
            [ws, items, metadata, denied, schedules],
            [ws, items, metadata],
            {},
        ),
        (
            "REQ-007",
            ["get", "Demo.Workspace/Book.Notebook", "-q", "description"],
            [ws, items, accepted, completed, metadata, connections, schedules],
            [ws, items, accepted],
            {},
        ),
        (
            "REQ-008",
            ["get", "Demo.Workspace/Book.Notebook", "-q", "description"],
            [ws, items, reply(metadata["body"], 202), connections, schedules],
            [ws, items, reply(metadata["body"], 202)],
            {},
        ),
        (
            "REQ-009",
            ["get", "Demo.Workspace", "-q", "displayName"],
            [ws, accepted, completed, primary, *enrichment],
            [ws, accepted, completed, primary, *enrichment],
            {},
        ),
        (
            "REQ-010",
            ["get", "Demo.Workspace", "-q", "displayName"],
            [ws, reply({}, 202)],
            [ws, reply({}, 202)],
            {},
        ),
        (
            "REQ-011",
            ["ls", "/"],
            [
                reply({"value": ws["body"]["value"], "continuationToken": "next"}),
                denied,
            ],
            [
                reply({"value": ws["body"]["value"], "continuationToken": "next"}),
                denied,
            ],
            {},
        ),
        (
            "REQ-012",
            ["ls", "/"],
            [reply({}, 429, **{"Retry-After": "0"}), ws],
            [reply({}, 429, **{"Retry-After": "0"}), ws],
            {},
        ),
        (
            "REQ-013",
            ["get", "Demo.Workspace/Book.Notebook", "-q", "description"],
            [ws, items, reply({}, 404)],
            [ws, items, reply({}, 404)],
            {},
        ),
        (
            "REQ-014",
            ["ls"],
            [items],
            [ws, items],
            {"current": "/Demo.Workspace"},
        ),
        (
            "REQ-015",
            ["ls", "/"],
            [ws],
            [ws],
            {"current": "/Gone.Workspace"},
        ),
        (
            "REQ-016",
            ["ls", "Demo.Workspace"],
            [empty, ws, items],
            [empty],
            {},
        ),
        (
            "REQ-017",
            ["ls", "Demo.Workspace"],
            [ws, reply({"value": [], "continuationToken": "next"}), items],
            [ws, reply({"value": [], "continuationToken": "next"}), items],
            {},
        ),
        (
            "REQ-018",
            ["get", "Demo.Workspace", "-q", "displayName"],
            [ws, primary, denied, reply({"enabled": False}), empty],
            [ws, primary, denied, reply({"enabled": False}), empty],
            {},
        ),
    ]
    return [
        {
            "id": identifier,
            "command": command + ["--output_format", "json"],
            "python_responses": copy.deepcopy(python),
            "responses": copy.deepcopy(native),
            **options,
        }
        for identifier, command, python, native, options in inventory
    ]


def load_sources() -> dict[str, ast.Module]:
    """Check all 259 protected source files before compiling original definitions."""
    sources = COMMANDS.load_sources()
    extra = (
        "client/fab_api_folders.py",
        "client/fab_api_capacity.py",
        "client/fab_api_jobs.py",
        "core/fab_handle_context.py",
        "utils/fab_util.py",
        "utils/fab_http_polling_utils.py",
        "utils/fab_ui.py",
        "commands/fs/get/fab_fs_get_item.py",
        "commands/fs/get/fab_fs_get_workspace.py",
        "commands/fs/ls/fab_fs_ls_folder.py",
    )
    for path in extra:
        sources[path] = ast.parse(
            ROOT.joinpath("src", "fabric_cli", *path.split("/"))
            .read_bytes()
            .replace(b"\r\n", b"\n"),
            filename=path,
        )
    return sources


def observe(case: dict[str, Any], sources: dict[str, ast.Module]) -> dict[str, Any]:
    """Execute real parser, path/cache/discovery, full metadata GET and HTTP source."""
    import cachetools
    import jmespath

    args = ARGUMENTS.Reference().arguments(case["command"])
    if args is None:
        raise AssertionError("The fixed valid argv inventory must parse")
    args.command_path = args.command
    output = COMMANDS.OUTPUT.Reference(COMMANDS.TIMESTAMP)
    env = output.namespace
    seen: list[str] = []
    scopes: list[list[str]] = []
    waits: list[float] = []
    events: list[str] = []
    returned: list[Any] = []
    violations: list[str] = []
    response_index = 0

    def deny(*unused: Any, **kwargs: Any) -> Any:
        violations.append("unapproved collaborator")
        raise COMMANDS.OfflineViolation("Unapproved source collaborator")

    def definitions(path: str, names: set[str]) -> None:
        exec(COMMANDS.ITEMS._compile_symbols(sources, path, names), env)

    names = {
        target.id
        for node in sources["core/fab_constant.py"].body
        if isinstance(node, ast.Assign)
        for target in node.targets
        if isinstance(target, ast.Name)
        and (
            target.id.startswith(("ERROR_", "EXIT_CODE_", "API_ENDPOINT_", "SCOPE_"))
            or target.id
            in {
                "API_VERSION_FABRIC",
                "DEBUG",
                "FAB_CACHE_ENABLED",
                "FAB_FOLDER_LISTING_ENABLED",
                "FAB_OUTPUT_ITEM_SORT_CRITERIA",
                "FAB_SHOW_HIDDEN",
                "FAB_WS_PRIVATE_LINKS_ENABLED",
                "ITEM_METADATA_PROPERTIES",
            }
        )
    }
    constants = {"validate_and_get_env_variable": lambda name, default: default}
    exec(
        COMMANDS.ITEMS._compile_symbols(sources, "core/fab_constant.py", names),
        constants,
    )
    env["fab_constant"].__dict__.update({name: constants[name] for name in names})
    constant = env["fab_constant"]
    settings = {
        constant.FAB_CACHE_ENABLED: "true",
        constant.FAB_FOLDER_LISTING_ENABLED: "true" if case.get("folders") else "false",
        constant.FAB_OUTPUT_ITEM_SORT_CRITERIA: "byname",
        constant.FAB_SHOW_HIDDEN: "false",
        constant.FAB_WS_PRIVATE_LINKS_ENABLED: "false",
    }

    class Auth:
        def get_access_token(self, scope: list[str]) -> str:
            if scope != SCOPE:
                raise AssertionError("Unexpected source scope")
            scopes.append(list(scope))
            return "offline-orchestration-only"

    class RequestException(Exception):
        pass

    class Session:
        def request(self, method: str, url: str, **kwargs: Any) -> Any:
            nonlocal response_index
            if method != "get" or kwargs != {
                "headers": {
                    "Authorization": "Bearer offline-orchestration-only",
                    "User-Agent": "offline-reference",
                    "Content-Type": "application/json",
                },
                "timeout": 240,
            }:
                raise AssertionError("Unexpected source HTTP method/headers/timeout")
            if response_index >= len(case["python_responses"]):
                return deny()
            fixture = case["python_responses"][response_index]
            response_index += 1
            seen.append(url)
            body = fixture["body"]
            text = body if isinstance(body, str) else json.dumps(body)
            return SimpleNamespace(
                status_code=fixture["status"],
                text=text,
                content=text.encode(),
                headers=COMMANDS.ITEMS._Headers(fixture["headers"]),
            )

    context = SimpleNamespace(command=args.command, fabric_skill=None)
    modules = {
        "fabric_cli.core.fab_auth": SimpleNamespace(FabAuth=Auth),
        "fabric_cli.core.fab_context": SimpleNamespace(Context=lambda: context),
        "fabric_cli.core": SimpleNamespace(fab_constant=constant),
        "fabric_cli.utils.fab_util": SimpleNamespace(dumps=env["dumps"]),
        "prompt_toolkit": SimpleNamespace(
            HTML=lambda text: text,
            print_formatted_text=lambda text, file: print(
                re.sub(r"</?ansiblue>", "", text), file=file
            ),
        ),
    }

    def injected_import(name: str, *unused: Any, **kwargs: Any) -> Any:
        if name == "json.decoder":
            return json.decoder
        return modules[name] if name in modules else deny()

    env["__builtins__"]["__import__"] = injected_import
    env.update(
        ABC=ABC,
        abstractmethod=abstractmethod,
        Enum=Enum,
        auto=auto,
        re=re,
        Namespace=argparse.Namespace,
        deque=deque,
        Optional=Optional,
        CaseInsensitiveDict=dict,
        Union=Union,
        Sequence=Sequence,
        List=list,
        json=json,
        jmespath=jmespath,
        state_config=SimpleNamespace(get_config=lambda name: settings[name]),
        fab_state_config=SimpleNamespace(get_config=lambda name: settings[name]),
        fab_logger=SimpleNamespace(
            log_debug_http_request=lambda *a: None,
            log_debug_http_response=lambda *a: None,
            log_debug_http_request_exception=lambda *a: None,
            log_debug=lambda *a: None,
            log_progress=lambda *a: events.append(str(a[0])),
        ),
        requests=SimpleNamespace(
            compat=SimpleNamespace(urlencode=urlencode),
            RequestException=RequestException,
        ),
        time=SimpleNamespace(time=lambda: 0, sleep=waits.append),
        urlparse=urlparse,
        _build_user_agent=lambda command: "offline-reference",
        _get_session=Session,
        _handle_azure_async_op=deny,
        files_utils=SimpleNamespace(load_json_from_path=deny),
        utils_storage=SimpleNamespace(do_output=deny),
        export_item=SimpleNamespace(export_single_item=deny),
        cached=cachetools.cached,
        keys=cachetools.keys,
        TTLCache=lambda maxsize, ttl: cachetools.TTLCache(
            maxsize, ttl, timer=lambda: 0
        ),
    )
    selections = (
        ("errors/common.py", {"CommonErrors"}),
        ("core/fab_exceptions.py", {"OnelakeAPIError", "AzureAPIError"}),
        ("client/fab_api_types.py", {"ApiResponse"}),
        (
            "core/fab_types.py",
            {
                "FabricElementType",
                "WorkspaceType",
                "VirtualWorkspaceType",
                "VirtualItemContainerType",
                "VirtualItemType",
                "VirtualWorkspaceItemType",
                "_BaseItemType",
                "ItemType",
                "FabricJobType",
                "format_mapping",
            },
        ),
        ("core/hiearchy/fab_element.py", {"FabricElement"}),
        ("core/hiearchy/fab_tenant.py", {"Tenant"}),
        ("core/hiearchy/fab_workspace.py", {"_BaseWorkspace", "Workspace"}),
        ("core/hiearchy/fab_folder.py", {"Folder"}),
        ("core/hiearchy/fab_base_item.py", {"_BaseItem"}),
        ("core/hiearchy/fab_item.py", {"Item"}),
        (
            "client/fab_api_client.py",
            {
                "do_request",
                "_handle_successful_response",
                "_transform_workspace_url_for_private_link_if_needed",
                "_handle_fab_long_running_op",
                "_poll_operation",
                "_fetch_operation_result",
                "_log_operation_progress",
            },
        ),
        (
            "client/fab_api_workspace.py",
            {
                "list_workspaces",
                "ls_workspace_items",
                "get_workspace",
                "ls_workspace_managed_private_endpoints",
                "get_workspace_spark_settings",
                "acl_list_from_workspace",
            },
        ),
        ("client/fab_api_capacity.py", {"list_capacities"}),
        ("client/fab_api_folders.py", {"list_folders"}),
        ("client/fab_api_item.py", {"get_item", "get_item_connections"}),
        ("client/fab_api_jobs.py", {"list_item_schedules"}),
        (
            "utils/fab_mem_store.py",
            {
                "build_ws_cache_key",
                "_get_workspaces_from_api",
                "_get_workspaces_from_cache",
                "get_workspaces",
                "_get_workspace_id_from_api",
                "get_workspace_id",
                "build_item_cache_key",
                "_get_workspace_items_from_api",
                "_get_workspace_items_from_cache",
                "get_workspace_items",
                "_get_item_id",
                "get_item_id",
                "build_folder_cache_key",
                "_extract_root_folders",
                "_extract_nested_folders",
                "_get_workspace_folders_from_api",
                "_get_workspace_folders_from_cache",
                "get_workspace_folders",
                "_get_folder_id",
                "get_folder_id",
                "_get_nested_folder_id",
                "get_nested_folder_id",
                "get_folder",
            },
        ),
        (
            "utils/fab_item_util.py",
            {
                "_get_elem_type",
                "sort_ws_elems_by_config",
                "get_item_with_definition",
            },
        ),
        ("utils/fab_cmd_fs_utils.py", {"get_ws_elements", "sort_ws_elements"}),
        (
            "utils/fab_cmd_ls_utils.py",
            {
                "sort_elements",
                "format_and_print_output",
                "get_capacities_and_workspaces",
                "enrich_workspaces_with_details",
            },
        ),
        ("utils/fab_util.py", {"process_nargs"}),
        ("utils/fab_jmespath.py", {"search", "_get_json_paths"}),
        (
            "utils/fab_cmd_get_utils.py",
            {"query_and_export", "is_metadata_property_query"},
        ),
        ("utils/fab_http_polling_utils.py", {"get_polling_interval"}),
    )
    for path, selected in selections:
        definitions(path, selected)
    job_maps = [
        node
        for node in sources["core/fab_types.py"].body
        if isinstance(node, ast.AnnAssign)
        and isinstance(node.target, ast.Name)
        and node.target.id == "ITJobMap"
    ]
    if len(job_maps) != 1:
        raise AssertionError("The original source job mapping must remain unique")
    exec(
        compile(
            ast.Module(body=job_maps, type_ignores=[]), "core/fab_types.py", "exec"
        ),
        env,
    )
    exec(
        COMMANDS.ITEMS._compile_symbols(
            sources, "utils/fab_http_polling_utils.py", {"DEFAULT_POLLING_INTERVAL"}
        ),
        env,
    )
    definitions("utils/fab_ui.py", {"print_info", "_safe_print_formatted_text"})
    env["ErrorMessages"] = SimpleNamespace(Common=env["CommonErrors"])
    env["fabric_api"] = SimpleNamespace(do_request=env["do_request"])
    for module, functions in (
        (
            "workspace_api",
            (
                "list_workspaces",
                "ls_workspace_items",
                "get_workspace",
                "ls_workspace_managed_private_endpoints",
                "get_workspace_spark_settings",
                "acl_list_from_workspace",
            ),
        ),
        ("capacity_api", ("list_capacities",)),
        ("folders_api", ("list_folders",)),
        ("item_api", ("get_item", "get_item_connections")),
        ("jobs_api", ("list_item_schedules",)),
    ):
        env[module] = SimpleNamespace(**{name: env[name] for name in functions})
    env["mem_store"] = env["utils_mem_store"] = SimpleNamespace(
        **{
            name: env[name]
            for name in (
                "get_workspaces",
                "get_workspace_id",
                "get_workspace_items",
                "get_item_id",
                "get_workspace_folders",
                "get_folder_id",
                "get_nested_folder_id",
            )
        }
    )
    env["item_utils"] = SimpleNamespace(
        sort_ws_elems_by_config=env["sort_ws_elems_by_config"],
        get_item_with_definition=env["get_item_with_definition"],
    )
    env["utils_fs"] = SimpleNamespace(
        get_ws_elements=env["get_ws_elements"], sort_ws_elements=env["sort_ws_elements"]
    )
    env["utils_ls"] = SimpleNamespace(
        **{
            name: env[name]
            for name in (
                "sort_elements",
                "format_and_print_output",
                "get_capacities_and_workspaces",
                "enrich_workspaces_with_details",
            )
        }
    )
    env["utils_jmespath"] = SimpleNamespace(search=env["search"])
    env["utils_get"] = SimpleNamespace(
        query_and_export=env["query_and_export"],
        is_metadata_property_query=env["is_metadata_property_query"],
    )
    env["fab_util"] = SimpleNamespace(process_nargs=env["process_nargs"])
    env["utils_ui"] = SimpleNamespace(
        print_output_format=env["print_output_format"],
        print_info=env["print_info"],
    )
    handlers = {}
    for key, path in (
        ("tenant", "commands/fs/ls/fab_fs_ls_workspace.py"),
        ("workspace", "commands/fs/ls/fab_fs_ls_item.py"),
        ("folder", "commands/fs/ls/fab_fs_ls_folder.py"),
        ("get_item", "commands/fs/get/fab_fs_get_item.py"),
        ("get_workspace", "commands/fs/get/fab_fs_get_workspace.py"),
    ):
        definitions(path, {"exec"})
        handlers[key] = env["exec"]
    # Unexercised resource classes are explicit guard types, not fallback handlers.
    for name in (
        "VirtualWorkspace",
        "VirtualItemContainer",
        "VirtualItem",
        "OneLakeItem",
    ):
        env[name] = type(name, (), {})
    context_env = dict(env)
    exec(
        COMMANDS.ITEMS._compile_symbols(
            sources,
            "core/fab_handle_context.py",
            {
                "get_command_context",
                "process_path_part",
                "process_context_path",
                "process_absolute_path",
                "process_relative_path",
                "_get_workspace_id",
                "_get_item_id",
                "_get_folder_id",
                "_get_nested_folder_id",
                "_handle_path_in_tenant",
                "_handle_path_in_ws",
                "_handle_path_in_folder",
            },
        ),
        context_env,
    )
    tenant = env["Tenant"]("Offline tenant", "offline-tenant")
    current_name = case.get("current", "/").strip("/")
    current = (
        env["Workspace"](
            current_name.removesuffix(".Workspace"), "w", tenant, "Workspace"
        )
        if current_name
        else tenant
    )
    context.context = current
    context.get_tenant = lambda: tenant
    before = current.path

    @env["handle_exceptions"]()
    def invoke(arguments: argparse.Namespace) -> None:
        target = context_env["get_command_context"](arguments.path)
        if arguments.command == "get":
            arguments.query = env["process_nargs"](arguments.query)
            arguments.output = env["process_nargs"](arguments.output)
            key = "get_item" if isinstance(target, env["Item"]) else "get_workspace"
            returned.append(handlers[key](target, arguments))
        else:
            key = (
                "tenant"
                if isinstance(target, env["Tenant"])
                else "folder" if isinstance(target, env["Folder"]) else "workspace"
            )
            handlers[key](target, arguments)

    with COMMANDS.offline_boundary():
        exit_code = invoke(args) or 0
    if violations or response_index != len(case["python_responses"]):
        raise AssertionError(f"{case['id']}: forbidden I/O or unused synthetic replies")
    expected_scopes = len(seen) - (1 if case["id"] == "REQ-012" else 0)
    if current.path != before or len(scopes) != expected_scopes:
        raise AssertionError(
            "Context mutation or unexpected logical-request scope acquisition"
        )
    return {
        "id": case["id"],
        "exit": exit_code,
        "stdout": output.stdout.getvalue(),
        "stderr": output.stderr.getvalue(),
        "requests": seen,
        "scopes": scopes,
        "waits": waits,
        "events": events,
        "returned": returned,
        "current_before": before,
        "current_after": current.path,
    }


def main() -> None:
    """Emit new observations; optional Rust execution never builds or installs."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-probe", type=Path)
    parser.add_argument("--rust-pin", default=RUST_PIN)
    parser.add_argument("--case", action="append")
    options = parser.parse_args()
    if not re.fullmatch(r"[a-f0-9]{40}", options.rust_pin):
        parser.error("--rust-pin requires an exact immutable commit")
    sources = load_sources()
    inventory = [
        case for case in cases() if not options.case or case["id"] in options.case
    ]
    if not inventory or (
        options.case and set(options.case) != {case["id"] for case in inventory}
    ):
        parser.error("--case requires a known REQ identifier")
    python = [observe(case, sources) for case in inventory]
    rust: list[Any] = [None] * len(inventory)
    if options.rust_probe:
        result = subprocess.run(
            [str(options.rust_probe.resolve(strict=True))],
            input="".join(json.dumps(case) + "\n" for case in inventory),
            text=True,
            capture_output=True,
            check=True,
            timeout=120,
        )
        if result.stderr:
            raise AssertionError("Rust consumer emitted unexpected stderr")
        rust = [json.loads(line) for line in result.stdout.splitlines()]
        if len(rust) != len(inventory):
            raise AssertionError("Missing or reordered Rust consumer results")
    print(
        json.dumps(
            {
                "_copyright": COMMANDS.COPYRIGHT,
                "python_pin": PYTHON_PIN,
                "rust_source_pin": options.rust_pin,
                "boundary": (
                    "Real source parser/path/memstore/cache/API/full metadata GET/query/UI; "
                    "synthetic current context/auth/HTTP/time. No main, persisted state, "
                    "command-support validation, definitions, live service or native broker."
                ),
                "rust_execution": "actual-run" if options.rust_probe else "not-run",
                "observations": [
                    {
                        "id": case["id"],
                        "fixture": case,
                        "python": original,
                        "rust": native,
                    }
                    for case, original, native in zip(inventory, python, rust)
                ],
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Source-pinned, in-memory ls and bounded metadata-get command oracle."""

from __future__ import annotations

import argparse
import ast
import builtins
import copy
import importlib.util
import io
import json
import os
import re
import socket
import subprocess
import sys
import time
from abc import ABC, abstractmethod
from contextlib import contextmanager
from enum import Enum, auto
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any, Iterator
from unittest.mock import patch
from urllib.parse import urlencode

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
TIMESTAMP = "2000-01-01T00:00:00Z"
SCOPE = ["https://api.fabric.microsoft.com/.default"]
COPYRIGHT = "Copyright (c) Microsoft Corporation. Licensed under the MIT License."
SOURCE_PIN = {
    "commit": SOURCE_COMMIT,
    "source_file_count": 259,
    "source_tree_sha256": "b044061fc4d8e43d104addf0de4d6860451ebd9fd0f5b0ddf81d5de1da3b41ce",
}
OLD_CASE_IDS = (
    "empty",
    "names_types_order",
    "token_pagination",
    "query_projection",
    "missing_value",
    "forbidden_mapping",
    "fabric_error",
    "root_from_workspace",
)
NEW_CASE_IDS = (
    "item_empty",
    "item_byname_stable_unicode",
    "item_bytype_enum_names",
    "item_future_only",
    "item_hidden_all",
    "item_hidden_config",
    "root_hidden_config",
    "item_long_id",
    "item_token_pagination",
    "item_forbidden",
    "item_not_found",
    "item_unauthorized",
    "get_metadata_paths",
    "get_metadata_depth",
    "get_forbidden",
    "get_not_found",
    "get_unauthorized",
    "get_fabric_error",
)
NATIVE_BOUNDARY_IDS = {
    "safety_exceptions": (
        "token_pagination", "missing_value", "item_token_pagination"
    ),
    "unsupported_projection": ("get_metadata_paths", "get_metadata_depth"),
    "metadata_order": ("get_metadata_paths", "get_metadata_depth"),
}
GET_MODES = (
    "metadata-preserving",
    "historical-typed-order",
    "historical-no-projection",
)
REQUEST_MODES = ("historical-generic", "source-routed")


def _reference(area: str) -> ModuleType:
    path = HERE.parent / area / "reference.py"
    spec = importlib.util.spec_from_file_location(f"_commands_{area}", path)
    if spec is None or spec.loader is None:
        raise AssertionError(f"Cannot read existing {area} reference")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


WORKSPACES = _reference("workspaces")
ITEMS = _reference("items")
OUTPUT = _reference("output")
OfflineViolation = ITEMS.OfflineViolation


def verify_source(provenance: dict[str, Any] | None = None) -> None:
    """Reuse the unchanged workspace validator, never update a source hash pin."""
    pin = SOURCE_PIN if provenance is None else provenance
    if pin != SOURCE_PIN:
        raise AssertionError("Python source pin changed; owner review is required")
    WORKSPACES.verify_source(pin)


def validate_manifest(manifest: dict[str, Any]) -> None:
    """Reject inventory erosion, missing expectations, and unrecorded URI waivers."""
    if (
        manifest["schema_version"] != 1
        or manifest["source_commit"] != SOURCE_COMMIT
        or manifest["_copyright"] != COPYRIGHT
    ):
        raise AssertionError("The source reference identity changed")
    if tuple(case["id"] for case in manifest["cases"]) != NEW_CASE_IDS:
        raise AssertionError("The independent command case inventory changed")
    if [case["id"] for case in manifest["source_only"]] != ["get_empty_metadata"]:
        raise AssertionError("The explicitly non-comparable inventory changed")
    for case in manifest["cases"] + manifest["source_only"]:
        inputs, expected = case["input"], case["expected"]
        if (
            not case.get("purpose")
            or inputs["id"] != case["id"]
            or not inputs["responses"]
            or set(expected) != {"exit_code", "output", "requests"}
            or not expected["requests"]
        ):
            raise AssertionError("A source case cannot become an empty smoke fixture")
        if inputs["command"][0] == "get":
            if (
                case.get("comparison") != "specialized-uri"
                or not case.get("python_requests")
                or case["python_requests"] == expected["requests"]
            ):
                raise AssertionError("GET must expose the specialized/generic URI seam")
        elif "comparison" in case or "python_requests" in case:
            raise AssertionError("LS may not waive request equality")
    if not manifest["source_only"][0].get("reason"):
        raise AssertionError("Source-only behavior needs an explicit rationale")


def load_cases() -> dict[str, Any]:
    """Read new hand-authored cases and reuse old workspace rows without copying."""
    manifest = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
    validate_manifest(manifest)
    old = WORKSPACES.load_cases()
    if tuple(case["id"] for case in old["cases"]) != OLD_CASE_IDS:
        raise AssertionError("The existing eight-workspace reference changed")
    if any(old["provenance"][key] != value for key, value in SOURCE_PIN.items()):
        raise AssertionError("The existing workspace source pin changed")
    # Preserve the original objects, including expected fields, byte-for-byte in
    # their original file. Only the probe input is adapted to the new protocol.
    manifest["workspaces"] = old["cases"]
    return manifest


def validate_native_boundaries(manifest: dict[str, Any]) -> None:
    """Require independently documented native differences, never broad waivers."""
    if (
        manifest["schema_version"] != 1
        or manifest["source_commit"] != SOURCE_COMMIT
        or manifest["_copyright"] != COPYRIGHT
    ):
        raise AssertionError("Native boundary identity changed")
    for category, ids in NATIVE_BOUNDARY_IDS.items():
        if tuple(case["id"] for case in manifest[category]) != ids:
            raise AssertionError(f"Native boundary inventory changed: {category}")
        for case in manifest[category]:
            if (
                not case.get("boundary")
                or set(case.get("evidence", {})) != {"python", "native"}
                or not all(case["evidence"].values())
                or set(case.get("expected", {})) != {
                    "exit_code", "output", "requests"
                }
                or not case["expected"]["requests"]
            ):
                raise AssertionError("A native boundary needs exact expectations/evidence")
            output = case["expected"]["output"]
            if category == "unsupported_projection" and (
                case["expected"]["exit_code"] != 1
                or output["status"] != "Failure"
                or output["result"].get("error_code") != "NotSupported"
            ):
                raise AssertionError("Missing projection is not a successful comparison")


def load_native_boundaries() -> dict[str, dict[str, dict[str, Any]]]:
    """Read separate hand-authored safety/schema evidence without source edits."""
    manifest = json.loads(
        (HERE / "native-boundaries.json").read_text(encoding="utf-8")
    )
    validate_native_boundaries(manifest)
    return {
        category: {case["id"]: case for case in manifest[category]}
        for category in NATIVE_BOUNDARY_IDS
    }


def workspace_input(case: dict[str, Any]) -> dict[str, Any]:
    """Adapt an old workspace input without rewriting any old expectations."""
    command = ["ls", case.get("path", "/"), "--output_format", "json"]
    if case.get("all"):
        command.append("--all")
    if case.get("query"):
        command.extend(["--query", case["query"]])
    result: dict[str, Any] = {
        "id": case["id"],
        "command": command,
        "responses": copy.deepcopy(case["responses"]),
    }
    if "current_workspace" in case:
        result["current_workspace"] = copy.deepcopy(case["current_workspace"])
    return result


def load_sources() -> dict[str, ast.Module]:
    """Validate the complete Python tree before parsing the executed definitions."""
    verify_source()
    paths = {
        "client/fab_api_client.py",
        "client/fab_api_item.py",
        "client/fab_api_types.py",
        "client/fab_api_workspace.py",
        "core/fab_constant.py",
        "core/fab_exceptions.py",
        "core/fab_types.py",
        "core/hiearchy/fab_element.py",
        "core/hiearchy/fab_tenant.py",
        "core/hiearchy/fab_workspace.py",
        "core/hiearchy/fab_folder.py",
        "core/hiearchy/fab_base_item.py",
        "core/hiearchy/fab_item.py",
        "errors/common.py",
        "utils/fab_mem_store.py",
        "utils/fab_cmd_fs_utils.py",
        "utils/fab_cmd_ls_utils.py",
        "utils/fab_item_util.py",
        "utils/fab_cmd_get_utils.py",
        "utils/fab_jmespath.py",
        "commands/fs/ls/fab_fs_ls_workspace.py",
        "commands/fs/ls/fab_fs_ls_item.py",
    }
    source = ROOT / "src" / "fabric_cli"
    return {
        name: ast.parse(
            source.joinpath(*name.split("/")).read_bytes().replace(b"\r\n", b"\n"),
            filename=name,
        )
        for name in paths
    }


@contextmanager
def offline_boundary() -> Iterator[None]:
    """Deny all product file I/O, network, real imports, processes, and sleeps."""
    active = True
    violations: list[str] = []

    def deny(*args: Any, **kwargs: Any) -> Any:
        violations.append("forbidden side effect")
        raise OfflineViolation("Product I/O and ambient dependencies are forbidden")

    def audit(event: str, args: tuple[Any, ...]) -> None:
        if active and (
            event == "open"
            or event.startswith(("socket.", "subprocess."))
            or event
            in {
                "os.system",
                "os.posix_spawn",
                "os.spawn",
                "os.mkdir",
                "os.remove",
                "os.rename",
                "os.rmdir",
                "os.chmod",
                "os.truncate",
                "os.link",
                "os.symlink",
                "os.listdir",
                "os.scandir",
                "os.chdir",
                "os.putenv",
                "os.unsetenv",
            }
        ):
            deny()

    sys.addaudithook(audit)
    try:
        with (
            patch.object(builtins, "__import__", deny),
            patch.object(builtins, "open", deny),
            patch.object(io, "open", deny),
            patch.object(os, "open", deny),
            patch.object(socket, "socket", deny),
            patch.object(socket, "create_connection", deny),
            patch.object(socket, "getaddrinfo", deny),
            patch.object(subprocess, "Popen", deny),
            patch.object(time, "sleep", deny),
        ):
            yield
    finally:
        active = False
    if violations:
        raise OfflineViolation("A forbidden operation was attempted, even if caught")


def _arguments(inputs: dict[str, Any]) -> argparse.Namespace:
    command = inputs["command"]
    verb = command[0]
    if "--query" in command or "-q" in command:
        raise ValueError("Query cases remain in the unchanged workspace oracle")
    if verb not in {"ls", "get"} or command[-2:] != ["--output_format", "json"]:
        # Old workspace --all is appended after the format flag.
        if not (
            verb == "ls"
            and command[-3:] == ["--output_format", "json", "--all"]
        ):
            raise ValueError("Only the documented JSON read-command slice is supported")
    positions = [
        part for part in command[1:] if part not in {
            "--all", "-a", "--long", "-l", "--output_format", "json"
        }
    ]
    path = positions[0] if positions else None
    if len(positions) > 1 or (verb == "get" and path != "."):
        raise ValueError("GET only supports the injected current item '.' boundary")
    if verb == "ls" and path not in {None, "/"}:
        raise ValueError("Explicit metadata path lookup is not part of this oracle")
    if inputs.get("config", {}).get("folder_listing_enabled", False):
        raise ValueError("Folder listing is outside this bounded command slice")
    return argparse.Namespace(
        command=verb,
        command_path=verb,
        output_format="json",
        path=path,
        query=None,
        output=None,
        deep_traversal=False,
        long="--long" in command or "-l" in command,
        all="--all" in command or "-a" in command,
    )


def observe(
    inputs: dict[str, Any], sources: dict[str, ast.Module] | None = None
) -> dict[str, Any]:
    """Execute real source command helpers down to fake auth and HTTP boundaries."""
    args = _arguments(inputs)
    sources = load_sources() if sources is None else sources
    output = OUTPUT.Reference(TIMESTAMP)
    env = output.namespace
    requests_seen: list[str] = []
    scopes: list[list[str]] = []
    response_index = 0

    def definitions(path: str, names: set[str]) -> None:
        exec(ITEMS._compile_symbols(sources, path, names), env)

    constant_names = {
        target.id
        for node in sources["core/fab_constant.py"].body
        if isinstance(node, ast.Assign)
        for target in node.targets
        if isinstance(target, ast.Name)
        and (
            target.id.startswith(("ERROR_", "EXIT_CODE_"))
            or target.id
            in {
                "API_ENDPOINT_FABRIC",
                "API_ENDPOINT_ONELAKE",
                "API_ENDPOINT_AZURE",
                "API_ENDPOINT_POWER_BI",
                "API_VERSION_FABRIC",
                "SCOPE_FABRIC_DEFAULT",
                "SCOPE_ONELAKE_DEFAULT",
                "SCOPE_AZURE_DEFAULT",
                "FAB_WS_PRIVATE_LINKS_ENABLED",
                "FAB_SHOW_HIDDEN",
                "FAB_OUTPUT_ITEM_SORT_CRITERIA",
                "FAB_FOLDER_LISTING_ENABLED",
                "FAB_CACHE_ENABLED",
                "DEBUG",
            }
        )
    }
    constants: dict[str, Any] = {
        "validate_and_get_env_variable": lambda name, default: default
    }
    exec(
        ITEMS._compile_symbols(sources, "core/fab_constant.py", constant_names),
        constants,
    )
    env["fab_constant"].__dict__.update(
        {name: constants[name] for name in constant_names}
    )
    config = inputs.get("config", {})
    settings = {
        env["fab_constant"].FAB_WS_PRIVATE_LINKS_ENABLED: "false",
        env["fab_constant"].FAB_CACHE_ENABLED: "false",
        env["fab_constant"].FAB_FOLDER_LISTING_ENABLED: "false",
        env["fab_constant"].FAB_SHOW_HIDDEN: (
            "true" if config.get("show_hidden", False) else "false"
        ),
        env["fab_constant"].FAB_OUTPUT_ITEM_SORT_CRITERIA: config.get(
            "item_sort", "byname"
        ),
    }

    def get_config(name: str) -> str:
        if name not in settings:
            raise OfflineViolation(f"Unapproved ambient configuration: {name}")
        return settings[name]

    def deny(*unused: Any, **kwargs: Any) -> Any:
        raise OfflineViolation("Unported collaborator or side effect was requested")

    class SyntheticRequestException(Exception):
        pass

    class TestAuth:
        def get_access_token(self, scope: list[str]) -> str:
            if scope != SCOPE:
                raise AssertionError("Unexpected source credential scope")
            scopes.append(list(scope))
            return "offline-command-reference-only"

    class ScriptedSession:
        def request(self, method: str, url: str, **kwargs: Any) -> Any:
            nonlocal response_index
            if method != "get" or kwargs != {
                "headers": {
                    "Authorization": "Bearer offline-command-reference-only",
                    "User-Agent": "offline-reference",
                    "Content-Type": "application/json",
                },
                "timeout": 240,
            }:
                raise AssertionError("The real HTTP helper boundary changed")
            if len(scopes) != len(requests_seen) + 1:
                raise AssertionError("An HTTP request must acquire exactly one scope")
            if response_index >= len(inputs["responses"]):
                raise OfflineViolation("Unexpected additional HTTP request")
            requests_seen.append(url)
            fixture = inputs["responses"][response_index]
            response_index += 1
            body = fixture["body"]
            text = body if isinstance(body, str) else json.dumps(body)
            return SimpleNamespace(
                status_code=fixture["status"],
                headers=ITEMS._Headers(fixture.get("headers", {})),
                text=text,
                content=text.encode("utf-8"),
            )

    modules = {
        "fabric_cli.core.fab_auth": SimpleNamespace(FabAuth=TestAuth),
        "fabric_cli.core.fab_context": SimpleNamespace(
            Context=lambda: SimpleNamespace(command=args.command, fabric_skill=None)
        ),
        "fabric_cli.utils.fab_util": SimpleNamespace(dumps=env["dumps"]),
    }

    def injected_import(
        name: str,
        globals: Any = None,
        locals: Any = None,
        fromlist: Any = (),
        level: int = 0,
    ) -> Any:
        if level == 0 and name in modules:
            return modules[name]
        return deny()

    # Each compiled source function receives a private import whitelist. The
    # global offline guard still forbids genuine imports and ambient discovery.
    env["__builtins__"]["__import__"] = injected_import
    env.update(
        {
            "ABC": ABC,
            "abstractmethod": abstractmethod,
            "Enum": Enum,
            "auto": auto,
            "re": re,
            "Namespace": argparse.Namespace,
            "state_config": SimpleNamespace(get_config=get_config),
            "fab_state_config": SimpleNamespace(get_config=get_config),
            "fab_logger": SimpleNamespace(
                log_debug_http_request=lambda *args: None,
                log_debug_http_response=lambda *args: None,
                log_debug_http_request_exception=lambda *args: None,
            ),
            "requests": SimpleNamespace(
                compat=SimpleNamespace(urlencode=urlencode),
                RequestException=SyntheticRequestException,
            ),
            "time": SimpleNamespace(time=lambda: 0, sleep=deny),
            "_build_user_agent": lambda command: "offline-reference",
            "_get_session": lambda: ScriptedSession(),
            "_handle_fab_long_running_op": deny,
            "_handle_azure_async_op": deny,
            "get_polling_interval": deny,
            "files_utils": SimpleNamespace(load_json_from_path=deny),
            "utils_storage": SimpleNamespace(do_output=deny),
            "jmespath": SimpleNamespace(search=deny),
            "export_item": SimpleNamespace(export_single_item=deny),
            "get_folder": deny,
        }
    )
    selections = [
        ("errors/common.py", {"CommonErrors"}),
        ("core/fab_exceptions.py", {"OnelakeAPIError", "AzureAPIError"}),
        ("client/fab_api_types.py", {"ApiResponse"}),
        ("core/fab_types.py", {
            "FabricElementType", "WorkspaceType", "VirtualWorkspaceType",
            "VirtualItemContainerType", "_BaseItemType", "ItemType",
            "format_mapping",
        }),
        ("core/hiearchy/fab_element.py", {"FabricElement"}),
        ("core/hiearchy/fab_tenant.py", {"Tenant"}),
        ("core/hiearchy/fab_workspace.py", {"_BaseWorkspace", "Workspace"}),
        ("core/hiearchy/fab_folder.py", {"Folder"}),
        ("core/hiearchy/fab_base_item.py", {"_BaseItem"}),
        ("core/hiearchy/fab_item.py", {"Item"}),
        ("client/fab_api_client.py", {
            "do_request", "_handle_successful_response",
            "_transform_workspace_url_for_private_link_if_needed",
        }),
        ("client/fab_api_workspace.py", {"list_workspaces", "ls_workspace_items"}),
        ("client/fab_api_item.py", {"get_item"}),
        ("utils/fab_mem_store.py", {
            "_get_workspaces_from_api", "get_workspaces",
            "_get_workspace_items_from_api", "get_workspace_items",
        }),
        ("utils/fab_item_util.py", {
            "_get_elem_type", "sort_ws_elems_by_config", "get_item_with_definition",
        }),
        ("utils/fab_cmd_fs_utils.py", {"get_ws_elements", "sort_ws_elements"}),
        ("utils/fab_cmd_ls_utils.py", {"sort_elements", "format_and_print_output"}),
        ("utils/fab_jmespath.py", {"search", "_get_json_paths"}),
        ("utils/fab_cmd_get_utils.py", {"query_and_export"}),
    ]
    for path, names in selections:
        definitions(path, names)
    env["ErrorMessages"] = SimpleNamespace(Common=env["CommonErrors"])
    env["fabric_api"] = SimpleNamespace(do_request=env["do_request"])
    env["workspace_api"] = SimpleNamespace(
        list_workspaces=env["list_workspaces"],
        ls_workspace_items=env["ls_workspace_items"],
    )
    env["item_api"] = SimpleNamespace(get_item=env["get_item"])
    env["utils_ui"] = SimpleNamespace(
        print_output_format=env["print_output_format"], print_info=deny
    )
    env["utils_mem_store"] = SimpleNamespace(
        get_workspaces=env["get_workspaces"],
        get_workspace_items=env["get_workspace_items"],
        get_workspace_folders=deny,
    )
    env["item_utils"] = SimpleNamespace(
        sort_ws_elems_by_config=env["sort_ws_elems_by_config"]
    )
    env["utils_fs"] = SimpleNamespace(
        get_ws_elements=env["get_ws_elements"],
        sort_ws_elements=env["sort_ws_elements"],
    )
    env["utils_ls"] = SimpleNamespace(
        sort_elements=env["sort_elements"],
        format_and_print_output=env["format_and_print_output"],
        get_capacities_and_workspaces=deny,
    )
    env["utils_jmespath"] = SimpleNamespace(search=env["search"])
    definitions("commands/fs/ls/fab_fs_ls_workspace.py", {"exec"})
    ls_workspace = env["exec"]
    definitions("commands/fs/ls/fab_fs_ls_item.py", {"exec"})
    ls_item = env["exec"]
    tenant = env["Tenant"]("Offline tenant", "synthetic-tenant")
    ws = inputs.get("current_workspace")
    workspace = (
        env["Workspace"](ws["displayName"], ws["id"], tenant, ws["type"])
        if ws else None
    )
    item_row = inputs.get("current_item")
    item = (
        env["Item"](
            item_row["displayName"], item_row["id"], workspace, item_row["type"]
        )
        if item_row and workspace else None
    )
    before = item.path if item else workspace.path if workspace else tenant.path

    @env["handle_exceptions"]()
    def invoke(arguments: argparse.Namespace) -> None:
        if arguments.command == "ls":
            if workspace is None or arguments.path == "/":
                ls_workspace(tenant, arguments)
            else:
                ls_item(workspace, arguments)
        else:
            if item is None:
                raise ValueError("GET requires both current_workspace and current_item")
            metadata = env["get_item_with_definition"](
                item, arguments, decode=True, obtain_definition=False
            )
            env["query_and_export"](metadata, arguments, item.full_name)

    with offline_boundary():
        try:
            exit_code = invoke(args) or 0
        except KeyError as error:
            try:
                env["_handle_unexpected_error"](error, args)
            except OUTPUT.ObservedExit as caught:
                exit_code = caught.code
        after = item.path if item else workspace.path if workspace else tenant.path
    if before != after:
        raise AssertionError("A read command mutated its current context")
    if response_index != len(inputs["responses"]):
        raise AssertionError(f"{inputs['id']}: unused scripted HTTP responses")
    return {
        "id": inputs["id"],
        "exit_code": exit_code,
        "stdout": output.stdout.getvalue(),
        "stderr": output.stderr.getvalue(),
        "requests": requests_seen,
        "scopes": scopes,
    }


def _expected(case: dict[str, Any], python: bool = False) -> dict[str, Any]:
    expected = case["expected"]
    data = expected["output"]
    stdout = (
        json.dumps({"timestamp": TIMESTAMP, **data}, indent=4) + "\n"
        if data is not None else ""
    )
    requests = (
        case.get("python_requests", expected["requests"])
        if python else expected["requests"]
    )
    return {
        "id": case["id"],
        "exit_code": expected["exit_code"],
        "stdout": stdout,
        "stderr": "",
        "requests": requests,
        "scopes": [list(SCOPE) for _ in requests],
    }


def assert_observation(
    case: dict[str, Any], actual: dict[str, Any], *, python: bool = False
) -> None:
    """Compare every protocol field and exact stream bytes; never bless snapshots."""
    expected = _expected(case, python)
    if actual != expected:
        differing = [
            key for key in expected if actual.get(key) != expected[key]
        ]
        raise AssertionError(
            f"{case['id']}: contract mismatch in {', '.join(differing)}\n"
            f"Expected: {expected!r}\nObserved: {actual!r}"
        )


def assert_workspace(case: dict[str, Any], actual: dict[str, Any]) -> None:
    """Reuse old semantic expectations, removing only their unpinned timestamp."""
    expected = case["expected"]
    parsed = json.loads(actual["stdout"])
    if "timestamp" not in parsed:
        raise AssertionError("Old workspace output omitted its timestamp")
    parsed.pop("timestamp")
    if (
        actual["id"] != case["id"]
        or actual["exit_code"] != expected["exit_code"]
        or parsed != expected["output"]
        or actual["stderr"] != ""
        or actual["requests"] != expected["requests"]
        or actual["scopes"] != [SCOPE for _ in expected["requests"]]
    ):
        raise AssertionError(f"{case['id']}: old workspace contract mismatch")


def run_rust(
    probe: Path, inputs: list[dict[str, Any]]
) -> list[dict[str, Any]]:
    """Run the explicit caller-provided probe once; it must use the real SDK path."""
    if not probe.is_file():
        raise ValueError(f"The explicitly supplied Rust probe is missing: {probe}")
    payload = "".join(json.dumps(case, ensure_ascii=True) + "\n" for case in inputs)
    process = subprocess.run(
        [str(probe.resolve())],
        input=payload.encode("utf-8"),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=120,
        check=False,
    )
    if process.returncode or process.stderr:
        raise AssertionError(
            f"Rust probe failed ({process.returncode}): "
            f"{process.stderr.decode('utf-8', errors='replace')}"
        )
    if b"\r" in process.stdout:
        raise AssertionError("Probe transport must use LF, not CRLF")
    lines = process.stdout.decode("utf-8").splitlines()
    if len(lines) != len(inputs) or not process.stdout.endswith(b"\n"):
        raise AssertionError("The probe must emit exactly one JSON line per input")
    result = [json.loads(line) for line in lines]
    for case, actual in zip(inputs, result):
        if actual.get("id") != case["id"]:
            raise AssertionError("Rust probe output is reordered or miscorrelated")
    return result


def compare_rust(
    case: dict[str, Any], python: dict[str, Any], rust: dict[str, Any],
    request_mode: str = "historical-generic",
) -> None:
    """Check each side independently before comparing all non-waived fields."""
    if request_mode not in REQUEST_MODES:
        raise ValueError("Unknown request comparison mode")
    assert_observation(case, python, python=True)
    assert_observation(case, rust, python=request_mode == "source-routed")
    common = set(python) - {"requests"}
    if any(python[key] != rust[key] for key in common):
        raise AssertionError(f"{case['id']}: Python/Rust byte contract mismatch")
    if request_mode == "source-routed" or case.get("comparison") != "specialized-uri":
        if python["requests"] != rust["requests"]:
            raise AssertionError(f"{case['id']}: request equality was not preserved")


def native_inventory(
    cases: list[dict[str, Any]], get_projection_integrated: bool
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Execute every case, requiring actual unsupported failures where applicable."""
    unsupported = []
    for case in cases:
        if (
            case["id"] in NATIVE_BOUNDARY_IDS["unsupported_projection"]
            and not get_projection_integrated
        ):
            unsupported.append(case)
    return list(cases), unsupported


def assert_native_boundary(
    case: dict[str, Any],
    python: dict[str, Any],
    native: dict[str, Any],
    boundary: dict[str, Any],
    *,
    workspace: bool = False,
) -> None:
    """Assert both independent outcomes; a native difference is never source parity."""
    if workspace:
        assert_workspace(case, python)
    else:
        assert_observation(case, python, python=True)
    if boundary["id"] != case["id"]:
        raise AssertionError("Native safety/schema evidence is miscorrelated")
    assert_observation(boundary, native)
    if python == native:
        raise AssertionError("An explicit native boundary must not be called source parity")


def compare_native_case(
    case: dict[str, Any],
    python: dict[str, Any],
    native: dict[str, Any],
    boundaries: dict[str, dict[str, dict[str, Any]]],
    get_mode: str = "metadata-preserving",
    request_mode: str = "historical-generic",
) -> str:
    """Default to source-order equality; historical outcomes require explicit modes."""
    if get_mode not in GET_MODES:
        raise ValueError("Unknown GET comparison mode")
    if request_mode not in REQUEST_MODES:
        raise ValueError("Unknown request comparison mode")
    if request_mode == "source-routed" and get_mode != "metadata-preserving":
        raise ValueError("Source routing requires metadata-preserving GET")
    case_id = case["id"]
    if case_id in boundaries["safety_exceptions"]:
        category, label = "safety_exceptions", "native-safety"
    elif (
        case_id in boundaries["metadata_order"]
        and get_mode != "metadata-preserving"
    ):
        category, label = (
            ("metadata_order", "metadata-schema-order")
            if get_mode == "historical-typed-order"
            else ("unsupported_projection", "unsupported-projection")
        )
    else:
        compare_rust(case, python, native, request_mode)
        return (
            "generic-get-uri"
            if python["requests"] != native["requests"]
            else "source-exact"
        )
    assert_native_boundary(case, python, native, boundaries[category][case_id])
    return label


def probe_boundary(name: str) -> None:
    """Attempt a forbidden action; the caller must observe OfflineViolation."""
    with offline_boundary():
        if name == "network":
            socket.getaddrinfo("offline.invalid", 443)
        elif name == "write":
            (HERE / "forbidden-write.json").write_text("{}", encoding="utf-8")
        elif name == "read":
            (HERE / "cases.json").read_bytes()
        elif name == "identity":
            builtins.__import__("msal")
        elif name == "process":
            subprocess.run([sys.executable, "--version"], check=True)
        elif name == "sleep":
            time.sleep(1)
        else:
            raise ValueError("Unknown offline guard probe")


def main() -> None:
    """Check source references and optionally compare an explicit native probe."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-probe", type=Path)
    parser.add_argument(
        "--request-mode", choices=REQUEST_MODES, default="historical-generic",
        help="require actual source routes or explicitly retain historical generic routes",
    )
    parser.add_argument(
        "--get-mode",
        choices=GET_MODES,
        default="metadata-preserving",
        help=(
            "default: require original source metadata order; historical modes "
            "assert old typed-order or disabled-provider evidence explicitly"
        ),
    )
    parser.add_argument(
        "--with-get-projection",
        action="store_true",
        help=(
            "compatibility flag: real projection is now required by default; "
            "historical behavior requires --get-mode"
        ),
    )
    parser.add_argument("--case", help="select one exact non-query case")
    options = parser.parse_args()
    if options.request_mode == "source-routed" and options.get_mode != "metadata-preserving":
        parser.error("source-routed requires metadata-preserving GET")
    if options.with_get_projection and not options.rust_probe:
        parser.error("--with-get-projection requires --rust-probe")
    if options.with_get_projection and options.get_mode == "historical-no-projection":
        parser.error("--with-get-projection conflicts with historical-no-projection")
    manifest = load_cases()
    boundaries = load_native_boundaries()
    sources = load_sources()
    old = [case for case in manifest["workspaces"] if not case.get("query")]
    cases = manifest["cases"]
    source_only = manifest["source_only"]
    if options.case:
        old = [case for case in old if case["id"] == options.case]
        cases = [case for case in cases if case["id"] == options.case]
        source_only = [
            case for case in source_only if case["id"] == options.case
        ]
        if not old and not cases and not source_only:
            parser.error("unknown case or query owned by the unchanged old oracle")
    source_inputs = [workspace_input(case) for case in old] + [
        case["input"] for case in cases
    ]
    python = [observe(inputs, sources) for inputs in source_inputs]
    for case, actual in zip(old, python):
        assert_workspace(case, actual)
    for case, actual in zip(cases, python[len(old):]):
        assert_observation(case, actual, python=True)
    for case in source_only:
        assert_observation(case, observe(case["input"], sources), python=True)
    native_cases, unsupported = native_inventory(
        cases, options.get_mode != "historical-no-projection"
    )
    native_inputs = [workspace_input(case) for case in old] + [
        case["input"] for case in native_cases
    ]
    if options.rust_probe:
        if not native_inputs:
            parser.error(
                "the selected case is source-only, not native-comparable"
            )
        rust = run_rust(options.rust_probe, native_inputs)
        counts: dict[str, int] = {
            "source-exact": 0,
            "generic-get-uri": 0,
            "native-safety": 0,
            "unsupported-projection": 0,
            "metadata-schema-order": 0,
        }
        for case, py, actual in zip(old, python, rust):
            safety = boundaries["safety_exceptions"].get(case["id"])
            if safety is not None:
                assert_native_boundary(case, py, actual, safety, workspace=True)
                counts["native-safety"] += 1
            else:
                assert_workspace(case, actual)
                if py != actual:
                    raise AssertionError(f"{case['id']}: source/native byte mismatch")
                counts["source-exact"] += 1
        python_by_id = {actual["id"]: actual for actual in python}
        for case, actual in zip(native_cases, rust[len(old):]):
            label = compare_native_case(
                case, python_by_id[case["id"]], actual, boundaries, options.get_mode,
                options.request_mode,
            )
            counts[label] += 1
        print(
            f"Native assertions (GET mode={options.get_mode}; requests={options.request_mode}; "
            "separate evidence, not blanket parity): "
            + ", ".join(f"{label}={count}" for label, count in counts.items())
        )
    else:
        for case in unsupported:
            print(
                f"UNSUPPORTED native comparison {case['id']}: "
                "actual NotSupported failure not verified without --rust-probe"
            )
    native_status = (
        f"asserted {len(native_inputs)} actual outcomes"
        if options.rust_probe else "not requested"
    )
    print(
        f"Checked {len(source_inputs)} command references and "
        f"{len(source_only)} source-only case(s) against {SOURCE_COMMIT}; "
        f"native probe {native_status}; "
        f"{len(unsupported)} historical GET path-discovery case(s) "
        "require explicit NotSupported. "
        "This is not full GET parity."
    )


if __name__ == "__main__":
    main()

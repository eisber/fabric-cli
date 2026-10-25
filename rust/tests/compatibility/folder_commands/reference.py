# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independent folder-command source oracle; there is no recording/update mode."""

from __future__ import annotations

import argparse
import ast
import builtins
import copy
import hashlib
import importlib.util
import json
import re
import subprocess
import sys
from abc import ABC, abstractmethod
from collections.abc import Iterator, Sequence
from contextlib import contextmanager
from enum import Enum, auto
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any, Optional, Union
from unittest.mock import patch
from urllib.parse import urlencode

sys.dont_write_bytecode = True

import cachetools
import jmespath

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
TIMESTAMP = "2000-01-01T00:00:00Z"
SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
INTEGRATION_COMMIT = "d9df14a6acf7fe3d80adf3bb74ee10ad0a7ac92f"
COPYRIGHT = "Copyright (c) Microsoft Corporation. Licensed under the MIT License."
SCOPE = ["https://api.fabric.microsoft.com/.default"]
# Canonical-LF Git blob of the hand-authored manifest, not an observation digest.
MANIFEST_BLOB = "dab9f9628d48989a44e7270a577771b2a94df447"
INVENTORY = {
    "exact": (
        "empty_root",
        "root_valid",
        "nested_context",
        "child_before_parent",
        "duplicate_first_match",
        "display_suffix_case",
        "byname_ties",
        "bytype_ties",
        "unicode_opaque_long",
        "query_rows",
        "query_identity",
        "query_empty",
        "query_empty_scalar",
        "query_false",
        "query_no_matches",
        "query_null",
        "query_count",
        "hidden_all",
        "hidden_config",
        "folder_ignores_query_all",
        "folder_disabled_config",
        "workspace_flat_disabled",
        "workspace_flat_no_folder_read",
        "unknown_type_parent_lookup",
        "items_401",
        "items_403",
        "items_404",
        "folders_401",
        "folders_403",
        "folders_404",
        "item_pagination",
        "folder_pagination",
    ),
    "safety": (
        "orphan_graph",
        "null_parent_graph",
        "nonstring_parent_graph",
        "cycle_graph",
        "self_cycle_graph",
        "missing_item_type",
        "missing_folder_name",
        "missing_items_value",
        "missing_folders_value",
        "malformed_items_json",
        "cancel_before",
        "item_null_folder",
        "item_orphan_folder",
        "item_count_limit",
        "folder_count_limit",
        "item_response_bytes",
        "folder_response_bytes",
        "item_page_limit",
        "folder_page_limit",
        "foreign_items_continuation",
        "foreign_folders_continuation",
    ),
    "snapshot-difference": (
        "cache_disabled_repeat",
        "cache_disabled_change",
        "current_snapshot_name",
    ),
    "unsupported": (
        "query_wide_sum",
        "query_provider_disabled",
        "query_big_literal",
    ),
}
NATIVE_BOUNDARIES_BLOB = "c2a058e6b040aecab2ba9a0f02cfb217ebe9ed24"
NATIVE_BOUNDARY_IDS = (
    "workspace_flat_disabled",
    "missing_items_value",
    "malformed_items_json",
    "cancel_before",
    "item_count_limit",
    "item_response_bytes",
    "item_page_limit",
    "foreign_items_continuation",
    "current_snapshot_name",
)
CATEGORY_CORRECTIONS = {
    "workspace_flat_disabled": "snapshot-difference",
    "current_snapshot_name": "exact",
}


def _support(area: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(
        f"_folder_commands_{area}", HERE.parent / area / "reference.py"
    )
    if spec is None or spec.loader is None:
        raise AssertionError(f"Missing read-only reference support: {area}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


COMMANDS = _support("commands")
OUTPUT = COMMANDS.OUTPUT
OfflineViolation = COMMANDS.OfflineViolation


class SourceNonprogress(Exception):
    """Bounded evidence rejected a graph before any source graph loop ran."""


def verify_source(pin: dict[str, Any] | None = None) -> None:
    """Verify the unchanged complete-tree Python pin, including private helpers."""
    COMMANDS.verify_source(COMMANDS.SOURCE_PIN if pin is None else pin)


def load_sources() -> dict[str, ast.Module]:
    """Read complete source ASTs only after checking the entire protected tree."""
    verify_source()
    sources = COMMANDS.load_sources()
    for path in (
        "client/fab_api_folders.py",
        "commands/fs/ls/fab_fs_ls_folder.py",
        "utils/fab_util.py",
    ):
        raw = ROOT.joinpath("src", "fabric_cli", *path.split("/")).read_bytes()
        sources[path] = ast.parse(raw.replace(b"\r\n", b"\n"), filename=path)
    return sources


def blob_identity(raw: bytes) -> str:
    """Match repository LF checkout policy, changing no other fixture bytes."""
    raw = raw.replace(b"\r\n", b"\n")
    return hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()


def validate_manifest(manifest: dict[str, Any], raw: bytes) -> None:
    """Reject changed expectations, scripts, labels, order, or denominators."""
    if blob_identity(raw) != MANIFEST_BLOB:
        raise AssertionError("Immutable folder-command fixture content changed")
    if json.dumps(json.loads(raw), ensure_ascii=True) != json.dumps(
        manifest, ensure_ascii=True
    ):
        raise AssertionError("In-memory folder-command manifest changed")
    if (
        manifest.get("_copyright") != COPYRIGHT
        or manifest.get("schema_version") != 1
        or manifest.get("source_commit") != SOURCE_COMMIT
        or manifest.get("integration_commit") != INTEGRATION_COMMIT
    ):
        raise AssertionError("Folder-command provenance changed")
    for category, identifiers in INVENTORY.items():
        observed = tuple(
            case["id"] for case in manifest["cases"] if case["category"] == category
        )
        if observed != identifiers:
            raise AssertionError(f"Folder-command inventory changed: {category}")
    cases = manifest["cases"]
    if len(cases) != sum(map(len, INVENTORY.values())):
        raise AssertionError("Unknown or duplicate folder-command category")
    for case in cases:
        if not case.get("purpose") or not case.get("expected"):
            raise AssertionError("A case lost its independent expectation")
        if case["category"] != "exact" and not case.get("boundary"):
            raise AssertionError("A difference needs explicit source evidence")
        if case["category"] == "exact" and (
            "python_input" in case
            or "python_expected" in case
            or case.get("python_cache_enabled", True) is not True
        ):
            raise AssertionError("Exact cases may not waive source requests")


def load_cases() -> dict[str, Any]:
    """Load immutable synthetic inputs/expectations; never generate expectations."""
    raw = (HERE / "cases.json").read_bytes()
    manifest = json.loads(raw)
    validate_manifest(manifest, raw)
    return manifest


def expected_identity(expected: dict[str, Any]) -> str:
    """Bind reviewed evidence to the original native-only handwritten expectation."""
    encoded = json.dumps(
        expected, ensure_ascii=True, separators=(",", ":"), sort_keys=True
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def validate_native_boundaries(
    manifest: dict[str, Any], raw: bytes, source_cases: dict[str, Any]
) -> None:
    """Permit only the nine independently reviewed, content-pinned native contracts."""
    if blob_identity(raw) != NATIVE_BOUNDARIES_BLOB:
        raise AssertionError("Immutable native evidence content changed")
    if json.loads(raw) != manifest:
        raise AssertionError("In-memory native evidence changed")
    if (
        manifest.get("_copyright") != COPYRIGHT
        or manifest.get("schema_version") != 1
        or manifest.get("source_commit") != SOURCE_COMMIT
        or manifest.get("integration_commit") != INTEGRATION_COMMIT
        or manifest.get("original_cases_blob") != MANIFEST_BLOB
        or not manifest.get("review")
    ):
        raise AssertionError("Native evidence provenance changed")
    if tuple(case["id"] for case in manifest["cases"]) != NATIVE_BOUNDARY_IDS:
        raise AssertionError("Native evidence nine-ID inventory changed")
    originals = {case["id"]: case for case in source_cases["cases"]}
    for case in manifest["cases"]:
        original = originals[case["id"]]
        if case["original_category"] != original["category"] or case[
            "original_native_expected_sha256"
        ] != expected_identity(original["expected"]):
            raise AssertionError("Native evidence original contract binding changed")
        category = CATEGORY_CORRECTIONS.get(case["id"], original["category"])
        if case["category"] != category:
            raise AssertionError("Native evidence per-ID classification changed")
        if (
            not case.get("reason")
            or not case["evidence"]
            or any(key not in manifest["evidence"] for key in case["evidence"])
        ):
            raise AssertionError("Native evidence lost its independent references")
        expected = case["expected"]
        if set(expected) != {
            "exit_code",
            "command",
            "result",
            "stderr",
            "requests",
            "scopes",
        } or expected["scopes"] != [SCOPE for _ in expected["requests"]]:
            raise AssertionError("A reviewed native contract is incomplete")
        if case["category"] == "safety" and (
            expected["exit_code"] != 1 or "data" in expected["result"]
        ):
            raise AssertionError("Reviewed safety cannot become partial success")


def load_native_boundaries() -> dict[str, Any]:
    """Read the additive overlay without changing any original fixture or source label."""
    raw = (HERE / "native-boundaries.json").read_bytes()
    manifest = json.loads(raw)
    validate_native_boundaries(manifest, raw, load_cases())
    return manifest


def verify_native_evidence(manifest: dict[str, Any]) -> None:
    """Check immutable historical source/base blobs, not current Rust observations."""
    for evidence in manifest["evidence"].values():
        result = subprocess.run(
            ["git", "cat-file", "blob", f"{evidence['commit']}:{evidence['path']}"],
            cwd=ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
            timeout=30,
        )
        if blob_identity(result.stdout) != evidence["blob"]:
            raise AssertionError("Native evidence historical source/base blob changed")


def native_category(case: dict[str, Any]) -> str:
    """Keep the two approved per-ID category corrections explicit."""
    return CATEGORY_CORRECTIONS.get(case["id"], case["category"])


def native_inventory(cases: list[dict[str, Any]]) -> dict[str, list[str]]:
    """Report actual native classifications by ID, not just coincident aggregate counts."""
    return {
        category: [case["id"] for case in cases if native_category(case) == category]
        for category in INVENTORY
    }


def case_input(case: dict[str, Any], *, python: bool = False) -> dict[str, Any]:
    """Expand declarative input defaults; never select or sort product rows."""
    result: dict[str, Any] = {
        "id": case["id"],
        "command": ["ls", "--output_format", "json"],
        "current_workspace": {"id": "w", "displayName": "Current", "type": "Workspace"},
        "config": {
            "folder_listing_enabled": True,
            "item_sort": "byname",
            "show_hidden": False,
        },
    }
    overrides = copy.deepcopy(case["input"])
    result["config"].update(overrides.pop("config", {}))
    result.update(overrides)
    if python:
        changes = copy.deepcopy(case.get("python_input", {}))
        result["config"].update(changes.pop("config", {}))
        result.update(changes)
    return result


def _arguments(inputs: dict[str, Any]) -> argparse.Namespace:
    parts = list(inputs["command"])
    if not parts or parts.pop(0) != "ls":
        raise ValueError("Only source ls command execution is assigned")
    args = argparse.Namespace(
        command="ls",
        command_path="ls",
        output_format=None,
        path=None,
        query=None,
        output=None,
        long=False,
        all=False,
    )
    while parts:
        value = parts.pop(0)
        if value == "--output_format":
            if not parts:
                raise ValueError("Missing read-command flag argument")
            args.output_format = parts.pop(0)
        elif value in ("--query", "-q"):
            args.query = []
            while parts and not parts[0].startswith("-"):
                args.query.append(parts.pop(0))
            if not args.query:
                raise ValueError("Missing read-command query argument")
        elif value in ("--long", "-l"):
            args.long = True
        elif value in ("--all", "-a"):
            args.all = True
        elif value == "." and args.path is None:
            args.path = value
        else:
            raise ValueError("Unassigned explicit path or command flag")
    if args.output_format != "json":
        raise ValueError("The oracle requires --output_format json")
    return args


def graph_evidence(rows: Any) -> dict[str, Any]:
    """Prove bounded root reachability without invoking the source while loop.

    This is a safety classifier only. It neither creates hierarchy objects nor
    supplies parent selection, ordering, or output to either implementation.
    Presence of parentFolderId is distinct from its null value in the source.
    """
    if not isinstance(rows, list) or any(
        not isinstance(row, dict) or not isinstance(row.get("id"), str) for row in rows
    ):
        return {
            "terminates": False,
            "reason": "malformed-graph-unproven",
            "blocked": [],
        }
    reachable = {row["id"] for row in rows if "parentFolderId" not in row}
    pending = [row for row in rows if "parentFolderId" in row]
    for _ in range(len(rows) + 1):
        remaining = [
            row
            for row in pending
            if not isinstance(row["parentFolderId"], str)
            or row["parentFolderId"] not in reachable
        ]
        if not remaining:
            return {"terminates": True, "reason": "root-reachable", "blocked": []}
        if len(remaining) == len(pending):
            return {
                "terminates": False,
                "reason": "source-while-loop-no-progress",
                "blocked": [row["id"] for row in remaining],
            }
        reachable.update(row["id"] for row in pending if row not in remaining)
        pending = remaining
    raise AssertionError("The bounded safety classifier exceeded its proof bound")


def require_source_progress(rows: Any) -> None:
    """Refuse unsafe source execution without timers, worker threads, or sleeps."""
    evidence = graph_evidence(rows)
    if not evidence["terminates"]:
        raise SourceNonprogress(json.dumps(evidence, sort_keys=True))


def _preflight(inputs: dict[str, Any]) -> None:
    # A child can precede its parent on another page: never classify pages as
    # independent graphs. This early conservative proof uses only potential
    # folder rows, not selected command rows. The API-return gate below also
    # checks each actual fully aggregated snapshot before its source loop.
    candidates = []
    for response in inputs["responses"]:
        body = response["body"]
        if isinstance(body, str):
            try:
                body = json.loads(body)
            except json.JSONDecodeError:
                continue
        if isinstance(body, dict) and isinstance(body.get("value"), list):
            candidates.extend(
                row
                for row in body["value"]
                if isinstance(row, dict) and "type" not in row
            )
    if any("parentFolderId" in row for row in candidates):
        require_source_progress(candidates)


@contextmanager
def offline_boundary() -> Iterator[None]:
    """Allow only CPython's already-loaded decoder, never a real import loader."""
    with COMMANDS.offline_boundary():
        denied_import = builtins.__import__

        def cached_decoder(name: str, *args: Any, **kwargs: Any) -> Any:
            if name == "json.decoder":
                return json.decoder
            return denied_import(name, *args, **kwargs)

        with patch.object(builtins, "__import__", cached_decoder):
            yield


@contextmanager
def _profile(trace: list[str]) -> Iterator[None]:
    watched = {
        "get_ws_elements",
        "get_workspace_items",
        "_get_workspace_items_from_cache",
        "_get_workspace_items_from_api",
        "ls_workspace_items",
        "get_folder",
        "get_workspace_folders",
        "_get_workspace_folders_from_cache",
        "_get_workspace_folders_from_api",
        "list_folders",
        "_extract_root_folders",
        "_extract_nested_folders",
        "sort_ws_elements",
        "sort_ws_elems_by_config",
        "format_and_print_output",
        "search",
        "print_output_format",
    }
    previous = sys.getprofile()

    def record(frame: Any, event: str, argument: Any) -> None:
        if event == "call" and frame.f_code.co_name in watched:
            if frame.f_code.co_filename.startswith(("utils/", "client/", "commands/")):
                trace.append(frame.f_code.co_name)

    sys.setprofile(record)
    try:
        yield
    finally:
        sys.setprofile(previous)


def observe(
    inputs: dict[str, Any],
    sources: dict[str, ast.Module] | None = None,
    *,
    cache_enabled: bool = True,
    trace: list[str] | None = None,
) -> dict[str, Any]:
    """Execute complete real command, memstore, cache, graph, API and UI ASTs."""
    _preflight(inputs)
    args = _arguments(inputs)
    if jmespath.__version__ != "1.0.1":
        raise AssertionError("The independent query engine must be jmespath 1.0.1")
    sources = load_sources() if sources is None else sources
    output = OUTPUT.Reference(TIMESTAMP)
    env = output.namespace

    def definitions(path: str, names: set[str]) -> None:
        exec(COMMANDS.ITEMS._compile_symbols(sources, path, names), env)

    constant_names = {
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
            }
        )
    }
    constants = {"validate_and_get_env_variable": lambda name, default: default}
    exec(
        COMMANDS.ITEMS._compile_symbols(
            sources, "core/fab_constant.py", constant_names
        ),
        constants,
    )
    env["fab_constant"].__dict__.update(
        {name: constants[name] for name in constant_names}
    )
    cfg = inputs["config"]
    constant = env["fab_constant"]
    settings = {
        constant.FAB_CACHE_ENABLED: "true" if cache_enabled else "false",
        constant.FAB_FOLDER_LISTING_ENABLED: (
            "true" if cfg["folder_listing_enabled"] else "false"
        ),
        constant.FAB_SHOW_HIDDEN: "true" if cfg["show_hidden"] else "false",
        constant.FAB_OUTPUT_ITEM_SORT_CRITERIA: cfg["item_sort"],
        constant.FAB_WS_PRIVATE_LINKS_ENABLED: "false",
    }
    requests_seen: list[str] = []
    scopes: list[list[str]] = []
    response_index = 0
    denied_operations: list[str] = []

    def deny(*unused: Any, **kwargs: Any) -> Any:
        denied_operations.append("unassigned product I/O or ambient collaborator")
        raise OfflineViolation("Unassigned product I/O or ambient collaborator")

    def get_config(name: str) -> str:
        if name not in settings:
            return deny()
        return settings[name]

    class RequestException(Exception):
        pass

    class Auth:
        def get_access_token(self, scope: list[str]) -> str:
            if scope != SCOPE:
                raise AssertionError("Unexpected API scope")
            scopes.append(list(scope))
            return "offline-folder-command-only"

    class Session:
        def request(self, method: str, url: str, **kwargs: Any) -> Any:
            nonlocal response_index
            if method != "get" or kwargs != {
                "headers": {
                    "Authorization": "Bearer " + "offline-folder-command-only",
                    "User-Agent": "offline-reference",
                    "Content-Type": "application/json",
                },
                "timeout": 240,
            }:
                raise AssertionError("Source API request boundary changed")
            if len(scopes) != len(requests_seen) + 1:
                raise AssertionError("Each HTTP request must acquire its API scope")
            if response_index >= len(inputs["responses"]):
                raise OfflineViolation("Unscripted additional HTTP request")
            fixture = inputs["responses"][response_index]
            response_index += 1
            requests_seen.append(url)
            body = fixture["body"]
            text = body if isinstance(body, str) else json.dumps(body)
            return SimpleNamespace(
                status_code=fixture["status"],
                text=text,
                content=text.encode("utf-8"),
                headers=COMMANDS.ITEMS._Headers(fixture.get("headers", {})),
            )

    modules = {
        "fabric_cli.core.fab_auth": SimpleNamespace(FabAuth=Auth),
        "fabric_cli.core.fab_context": SimpleNamespace(
            Context=lambda: SimpleNamespace(command="ls", fabric_skill=None)
        ),
        "fabric_cli.utils.fab_util": SimpleNamespace(dumps=env["dumps"]),
    }

    def injected_import(name: str, *unused: Any, **kwargs: Any) -> Any:
        if name in modules:
            return modules[name]
        # CPython's C JSON decoder may ask for this already-loaded error class.
        # No loader, filesystem lookup or genuine Python import is performed.
        if name == "json.decoder":
            return json.decoder
        return deny()

    env["__builtins__"]["__import__"] = injected_import
    env.update(
        {
            "ABC": ABC,
            "abstractmethod": abstractmethod,
            "Enum": Enum,
            "auto": auto,
            "re": re,
            "Namespace": argparse.Namespace,
            "Union": Union,
            "Optional": Optional,
            "Sequence": Sequence,
            "state_config": SimpleNamespace(get_config=get_config),
            "fab_state_config": SimpleNamespace(get_config=get_config),
            "fab_logger": SimpleNamespace(
                log_debug_http_request=lambda *args: None,
                log_debug_http_response=lambda *args: None,
                log_debug_http_request_exception=lambda *args: None,
            ),
            "requests": SimpleNamespace(
                compat=SimpleNamespace(urlencode=urlencode),
                RequestException=RequestException,
            ),
            "json": json,
            "jmespath": jmespath,
            "time": SimpleNamespace(time=lambda: 0, sleep=deny),
            "_build_user_agent": lambda command: "offline-reference",
            "_get_session": Session,
            "_handle_fab_long_running_op": deny,
            "_handle_azure_async_op": deny,
            "get_polling_interval": deny,
            "files_utils": SimpleNamespace(load_json_from_path=deny),
            "cached": cachetools.cached,
            "keys": cachetools.keys,
            "TTLCache": lambda maxsize, ttl: cachetools.TTLCache(
                maxsize=maxsize, ttl=ttl, timer=lambda: 0
            ),
        }
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
                "_BaseItemType",
                "ItemType",
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
            },
        ),
        ("client/fab_api_workspace.py", {"ls_workspace_items"}),
        ("client/fab_api_folders.py", {"list_folders"}),
        (
            "utils/fab_mem_store.py",
            {
                "build_item_cache_key",
                "_get_workspace_items_from_api",
                "_get_workspace_items_from_cache",
                "get_workspace_items",
                "build_folder_cache_key",
                "_extract_root_folders",
                "_extract_nested_folders",
                "_get_workspace_folders_from_api",
                "_get_workspace_folders_from_cache",
                "get_workspace_folders",
                "get_folder",
            },
        ),
        ("utils/fab_item_util.py", {"_get_elem_type", "sort_ws_elems_by_config"}),
        ("utils/fab_cmd_fs_utils.py", {"get_ws_elements", "sort_ws_elements"}),
        ("utils/fab_cmd_ls_utils.py", {"format_and_print_output"}),
        ("utils/fab_util.py", {"process_nargs"}),
        ("utils/fab_jmespath.py", {"search", "_get_json_paths"}),
    )
    for path, names in selections:
        definitions(path, names)
    env["ErrorMessages"] = SimpleNamespace(Common=env["CommonErrors"])
    env["fabric_api"] = SimpleNamespace(do_request=env["do_request"])
    env["workspace_api"] = SimpleNamespace(ls_workspace_items=env["ls_workspace_items"])

    def checked_folder_api(arguments: argparse.Namespace) -> Any:
        response = env["list_folders"](arguments)
        if response.status_code in (200, 201):
            try:
                body = json.loads(response.text)
            except (json.JSONDecodeError, TypeError):
                return response
            if isinstance(body, dict) and isinstance(body.get("value"), list):
                rows = body["value"]
                if any(
                    isinstance(row, dict) and "parentFolderId" in row for row in rows
                ):
                    require_source_progress(rows)
        return response

    env["folders_api"] = SimpleNamespace(list_folders=checked_folder_api)
    env["utils_mem_store"] = SimpleNamespace(
        get_workspace_items=env["get_workspace_items"],
        get_workspace_folders=env["get_workspace_folders"],
    )
    env["item_utils"] = SimpleNamespace(
        sort_ws_elems_by_config=env["sort_ws_elems_by_config"]
    )
    env["utils_fs"] = SimpleNamespace(
        get_ws_elements=env["get_ws_elements"], sort_ws_elements=env["sort_ws_elements"]
    )
    env["utils_ui"] = SimpleNamespace(print_output_format=env["print_output_format"])
    env["utils_ls"] = SimpleNamespace(
        format_and_print_output=env["format_and_print_output"]
    )
    env["fab_util"] = SimpleNamespace(process_nargs=env["process_nargs"])
    env["utils_jmespath"] = SimpleNamespace(search=env["search"])
    definitions("commands/fs/ls/fab_fs_ls_item.py", {"exec"})
    workspace_exec = env["exec"]
    definitions("commands/fs/ls/fab_fs_ls_folder.py", {"exec"})
    folder_exec = env["exec"]
    observed_trace = [] if trace is None else trace
    with offline_boundary(), _profile(observed_trace):
        tenant = env["Tenant"]("Offline tenant", "synthetic-tenant")
        ws = inputs["current_workspace"]
        workspace = env["Workspace"](ws["displayName"], ws["id"], tenant, ws["type"])
        current = workspace
        for folder in inputs.get("current_folders", []):
            current = env["Folder"](folder["displayName"], folder["id"], current)
        before = current.path

        @env["handle_exceptions"]()
        def invoke(arguments: argparse.Namespace) -> None:
            if isinstance(current, env["Folder"]):
                folder_exec(current, arguments)
            else:
                workspace_exec(current, arguments)

        try:
            exit_code = invoke(args) or 0
        except OfflineViolation:
            raise
        except (
            KeyError,
            AssertionError,
            TypeError,
            ValueError,
            AttributeError,
        ) as error:
            try:
                env["_handle_unexpected_error"](error, args)
            except OUTPUT.ObservedExit as caught:
                exit_code = caught.code
        if current.path != before:
            raise AssertionError("A read command mutated its injected current context")
    if denied_operations:
        raise OfflineViolation(
            "A forbidden source collaborator was attempted, even if caught"
        )
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


def expected_observation(
    case: dict[str, Any], *, python: bool = False
) -> dict[str, Any]:
    """Serialize only hand-authored expected content using fixed protocol spelling."""
    expected = (
        case.get("python_expected", case["expected"]) if python else case["expected"]
    )
    if not python and case["id"] in NATIVE_BOUNDARY_IDS:
        boundaries = load_native_boundaries()
        boundary = next(row for row in boundaries["cases"] if row["id"] == case["id"])
        if (
            boundary["original_native_expected_sha256"]
            != expected_identity(case["expected"])
            or boundary["original_category"] != case["category"]
        ):
            raise AssertionError("Native evidence original contract binding changed")
        expected = boundary["expected"]
    envelope: dict[str, Any] = {
        "timestamp": TIMESTAMP,
        "status": "Failure" if expected.get("exit_code", 0) else "Success",
    }
    if expected.get("command", "ls") is not None:
        envelope["command"] = expected.get("command", "ls")
    envelope["result"] = expected["result"]
    inputs = case_input(case, python=python)
    base = f"https://api.fabric.microsoft.com/v1/workspaces/{inputs['current_workspace']['id']}"
    requests = [
        base + "/" + uri
        for uri in expected.get("requests", ["items", "folders?recursive=True"])
    ]
    return {
        "id": case["id"],
        "exit_code": expected.get("exit_code", 0),
        "stdout": json.dumps(envelope, indent=4, ensure_ascii=True) + "\n",
        "stderr": expected.get("stderr", ""),
        "requests": requests,
        "scopes": expected.get("scopes", [list(SCOPE) for _ in requests]),
    }


def assert_observation(
    case: dict[str, Any], actual: dict[str, Any], *, python: bool = False
) -> None:
    """Assert exact keys, streams, exit status, request order and per-request scopes."""
    expected = expected_observation(case, python=python)
    if actual != expected:
        fields = sorted(
            key
            for key in set(expected) | set(actual)
            if actual.get(key) != expected.get(key)
            or (key in actual) != (key in expected)
        )
        raise AssertionError(
            f"{case['id']}: {'Python' if python else 'native'} contract mismatch: {fields}\n"
            f"Expected: {expected!r}\nObserved: {actual!r}"
        )


def compare(
    case: dict[str, Any], python: dict[str, Any] | None, native: dict[str, Any]
) -> None:
    """A declared difference is verified, not waived or counted as exact parity."""
    assert_observation(case, native)
    if python is not None:
        assert_observation(case, python, python=True)
    category = native_category(case)
    if category == "exact" and python != native:
        raise AssertionError(f"{case['id']}: a full exact case differs")
    if category != "exact" and python == native:
        raise AssertionError(f"{case['id']}: stale difference classification")


def check_python(
    manifest: dict[str, Any], sources: dict[str, ast.Module]
) -> list[dict[str, Any] | None]:
    """Check terminating source cases, separately proving excluded nonprogress."""
    result = []
    for case in manifest["cases"]:
        inputs = case_input(case, python=True)
        if case.get("python_nonprogress"):
            try:
                _preflight(inputs)
            except SourceNonprogress:
                result.append(None)
                continue
            raise AssertionError(f"{case['id']}: nonprogress evidence disappeared")
        actual = observe(
            inputs, sources, cache_enabled=case.get("python_cache_enabled", True)
        )
        assert_observation(case, actual, python=True)
        result.append(actual)
    return result


def main() -> None:
    """Check Python alone or compare one explicitly supplied real native probe."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--rust-probe", type=Path)
    options = parser.parse_args()
    if not options.check and options.rust_probe is None:
        parser.error("supply --check and/or --rust-probe; no recording mode exists")
    manifest = load_cases()
    verify_native_evidence(load_native_boundaries())
    source = check_python(manifest, load_sources())
    if options.rust_probe is not None:
        native = COMMANDS.run_rust(
            options.rust_probe, [case_input(case) for case in manifest["cases"]]
        )
        for case, python, rust in zip(manifest["cases"], source, native):
            compare(case, python, rust)
    print(
        json.dumps(
            {
                "source_commit": SOURCE_COMMIT,
                "python_terminating": sum(row is not None for row in source),
                "python_nonprogress_not_executed": sum(row is None for row in source),
                "native_executed": len(source) if options.rust_probe is not None else 0,
                "denominators": {key: len(value) for key, value in INVENTORY.items()},
                "native_classification_ids": native_inventory(manifest["cases"]),
            }
        )
    )


if __name__ == "__main__":
    main()

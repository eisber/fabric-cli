# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute unchanged pinned TABLE functions against hand-authored offline cases.

No product import, credential, real network, real tempfile or clock is used.
Expected values have no recording/update path and are not derived from Rust.
"""

from __future__ import annotations

import __future__
import argparse
import ast
import builtins
import contextlib
import hashlib
import io
import json
import os
import socket
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace as NS
from typing import Any
from unittest.mock import patch
from urllib.parse import urlparse, urlencode

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
BLOBS = {
    "commands/tables/fab_tables.py": "5a4134688776923bbaa3632d00129510fd9c4527",
    "commands/tables/fab_tables_load.py": "4da52548171a177eefbb4ff1e8283f439017ec87",
    "commands/tables/fab_tables_schema.py": "74f3b39ddfba1616bc041bf0d84dc6109332a6d5",
    "commands/tables/fab_tables_opt.py": "c101c581f6ee560e0a7e56b98f9087cdbe17324c",
    "utils/fab_cmd_table_utils.py": "3b00fb6af4e5e1037e288a84cc2ac361970f596a",
    "client/fab_api_tables.py": "eaec86fff74f2ea424c5bc3138cdbd2e3a5e2c0f",
    "core/fab_exceptions.py": "b3d6cd5e4eed8240271d1c423b69e4cd806b2d66",
    "errors/table.py": "46e4f8773b4bd3169cc7459de2ba7aa9a06c7d11",
    "client/fab_api_client.py": "0e896e27ff9fe0e6446448d721679db422f283dd",
    "errors/common.py": "6fcfc00098d71517d8bb345015b48d7527544640",
    "errors/client.py": "2b21eeb5c7b72f28a8783fbc411780aeb0e6fa4b",
    "utils/fab_http_polling_utils.py": "6adec884ca1a18a80063c66c65f5fe3e516817e5",
    "client/fab_api_onelake.py": "73508f6d00811dd0240727348bc3f62dac8b0b51",
    "client/fab_api_types.py": "09b81e62b57965b3e39075f2ce55d80b3bc849d3",
}
_ALLOWED_IMPORTS: dict[str, Any] = {}


def load_sources() -> dict[str, ast.Module]:
    """Pin actual Git blobs before entering the source execution boundary."""
    sources = {}
    for path, blob in BLOBS.items():
        data = subprocess.check_output(
            ["git", "show", f"{SOURCE_COMMIT}:src/fabric_cli/{path}"], cwd=ROOT
        )
        actual = hashlib.sha1(
            b"blob " + str(len(data)).encode("ascii") + b"\0" + data
        ).hexdigest()
        if blob is not None and actual != blob:
            raise AssertionError(f"Source blob drift: {path}")
        sources[path] = ast.parse(data.decode("utf-8"), filename=path)
    return sources


def symbols(sources: dict[str, ast.Module], path: str, names: list[str], env: dict[str, Any]) -> NS:
    """Compile original function bodies, including original decorators."""
    nodes = [
        node
        for node in sources[path].body
        if isinstance(node, (ast.FunctionDef, ast.ClassDef))
        and node.name in names
    ]
    if {node.name for node in nodes} != set(names):
        raise AssertionError(f"Missing source symbols: {path}")
    exec(
        compile(
            ast.Module(body=nodes, type_ignores=[]),
            path,
            "exec",
            flags=__future__.annotations.compiler_flag,
            dont_inherit=True,
        ),
        env,
    )
    return NS(**{name: env[name] for name in names})


def observe(case: dict[str, Any], sources: dict[str, ast.Module]) -> tuple[dict[str, Any], str | None, list[str]]:
    """Observe source payloads, schema fields, order and normalized error codes."""
    env = {
        "json": json,
        "Namespace": argparse.Namespace,
        "DEFAULT_ERROR_MESSAGE": "An error occurred while processing the operation",
        "DEFAULT_ERROR_CODE": "UnknownError",
    }
    errors = symbols(sources, "core/fab_exceptions.py", ["FabricCLIError"], env)
    table_errors = symbols(sources, "errors/table.py", ["TableErrors"], env)
    env.update(
        FabricCLIError=errors.FabricCLIError,
        ErrorMessages=NS(Table=table_errors.TableErrors),
        fab_constant=NS(
            ERROR_INVALID_INPUT="InvalidInput",
            ERROR_INVALID_PATH="InvalidPath",
            ERROR_INVALID_DETLA_TABLE="InvalidDeltaTable",
        ),
    )
    utils = symbols(
        sources,
        "utils/fab_cmd_table_utils.py",
        ["parse_table_format_argument", "convert_hours_to_dhhmmss", "add_table_props_to_args"],
        env,
    )
    output = {}
    diagnostics = []
    reads = []
    args = argparse.Namespace(
        path=[case.get("local_path", "Tables/sales")],
        file=[case.get("source_path", "Files/input.csv")],
        mode=case.get("mode", "overwrite"),
        format=case.get("format"),
        extension=case.get("extension"),
        table_name="sales",
        ws_id="ws",
        lakehouse_id="item",
        output_format="json",
        retain_n_hours=case.get("hours", "168"),
        vorder=case.get("vorder", False),
        zorder=case.get("zorder"),
    )
    class OneLake:
        def __init__(self, path: str) -> None:
            self.local_path = path
            self.path_id = "ws/item/" + path
            self.workspace = NS(id="ws")
            self.item = NS(id="item", item_type="Lakehouse", path="/ws.Workspace/lh.Lakehouse")
        def check_command_support(self, command: Any) -> None:
            return None
    env["OneLakeItem"] = OneLake
    env["handle_context"] = NS(
        get_command_context=lambda path, *a, **k: OneLake(" ".join(path))
    )
    env["utils_table"] = utils
    def emit(a: argparse.Namespace, message: str | None = None, data: Any = None, **kwargs: Any) -> None:
        output.update(data=data, message=message)
    ui = NS(print_grey=diagnostics.append, print_output_format=emit)
    env["utils_ui"] = env["fab_ui"] = ui

    def request(a: argparse.Namespace, data: str | None = None) -> NS:
        assert a.wait is True
        output["request"] = {"method": a.method, "uri": a.uri}
        output["payload"] = json.loads(data)
        output["wire_body"] = data
        return NS(status_code=case.get("load_status", 200))

    env["fabric_api"] = NS(do_request=request)
    api = symbols(sources, "client/fab_api_tables.py", ["load_table"], env)
    env["tables_api"] = api
    env["onelake_api"] = NS(
        get_properties=lambda a: NS(headers={"x-ms-resource-type": case.get("source_kind", "file")})
    )
    failure = None
    try:
        operation = case["operation"]
        if operation == "load":
            loader = symbols(sources, "commands/tables/fab_tables_load.py", ["exec_command"], dict(env))
            env.update(
                tables_load=loader, Command=NS(TABLE_LOAD="table load"),
                handle_exceptions=lambda: lambda fn: fn,
                set_command_context=lambda: lambda fn: fn,
            )
            symbols(sources, "commands/tables/fab_tables.py", ["load_command"], env).load_command(args)
        elif operation in {"vacuum", "optimize"}:
            files = {}
            class Temp:
                name = "memory-only-table-config"
                def __enter__(self) -> Temp:
                    files[self.name] = ""
                    return self
                def __exit__(self, *exc: Any) -> None:
                    return None
            class Writer(io.StringIO):
                def close(self) -> None:
                    files[Temp.name] = self.getvalue()
                    super().close()
            def open_memory(path: str, mode: str, encoding: str) -> Writer:
                assert path == Temp.name and mode == "w" and encoding == "utf-8"
                return Writer()
            def job_run(a: argparse.Namespace) -> None:
                assert a.jobType == "TableMaintenance" and a.jobs_command == "run"
                assert a.configuration is None and a.params is None
                output["payload"] = json.loads(files[a.input])
            maintenance_env = dict(env, tempfile=NS(NamedTemporaryFile=lambda **k: Temp()),
                open=open_memory, jobs=NS(run_command=job_run), contextlib=contextlib,
                os=NS(unlink=lambda path: files.pop(path)))
            maintenance = symbols(sources, "commands/tables/fab_tables_opt.py", ["exec_command"], maintenance_env)
            env.update(
                tables_opt=maintenance,
                Command=NS(TABLE_VACUUM="table vacuum", TABLE_OPTIMIZE="table optimize"),
                handle_exceptions=lambda: lambda fn: fn,
                set_command_context=lambda: lambda fn: fn,
            )
            handler = operation + "_command"
            symbols(sources, "commands/tables/fab_tables.py", [handler], env).__dict__[handler](args)
            assert files == {}, "Source TABLE config file was not cleaned up"
        else:
            logs = case["logs"]
            def read(a: argparse.Namespace) -> NS:
                reads.append(a.from_path)
                name = a.from_path.split("/", 1)[1]
                return NS(status_code=200, text=next(log["text"] for log in logs if log["name"] == name))
            env["onelake_api"] = NS(
                list_tables_files_recursive=lambda a: NS(
                    status_code=200, json=lambda: {"paths": [{"name": log["name"]} for log in logs]}
                ),
                read=read,
            )
            env["utils"] = NS(
                remove_dot_suffix=lambda value: value.replace(".Shortcut", "").replace(".shortcut", "")
            )
            schema = symbols(
                sources, "commands/tables/fab_tables_schema.py",
                ["_get_commit_logs", "_extract_schema_from_commit_logs", "exec_command"], env
            )
            env.update(
                tables_schema=schema, Command=NS(TABLE_SCHEMA="table schema"),
                handle_exceptions=lambda: lambda fn: fn,
                set_command_context=lambda: lambda fn: fn,
            )
            symbols(sources, "commands/tables/fab_tables.py", ["schema_command"], env).schema_command(args)
    except errors.FabricCLIError as exc:
        failure = exc.status_code
    except (AttributeError, ValueError, KeyError, TypeError) as exc:
        failure = "UnexpectedError"
        output["unexpected_message"] = str(exc)
    output["diagnostics"] = diagnostics
    return output, failure, reads


def deny(*args: Any, **kwargs: Any) -> Any:
    raise AssertionError("Offline TABLE oracle attempted forbidden I/O")

def injected_import(name: str, *args: Any, **kwargs: Any) -> Any:
    if name in _ALLOWED_IMPORTS:
        return _ALLOWED_IMPORTS[name]
    return deny()


def observe_lifecycle(case: dict[str, Any], sources: dict[str, ast.Module]) -> dict[str, Any]:
    """Execute actual shared Python LRO handlers, including conditional waiting."""
    env = {
        "json": json, "Namespace": argparse.Namespace, "urlparse": urlparse,
        "DEFAULT_ERROR_MESSAGE": "An error occurred while processing the operation",
        "DEFAULT_ERROR_CODE": "UnknownError",
    }
    errors = symbols(sources, "core/fab_exceptions.py", ["FabricCLIError", "FabricAPIError"], env)
    common = symbols(sources, "errors/common.py", ["CommonErrors"], env)
    client = symbols(sources, "errors/client.py", ["ClientErrors"], env)
    waits, requests = [], []
    replies = iter(case.get("responses", []))
    class Headers(dict):
        def get(self, key: str, default: Any = None) -> Any:
            return next((value for name, value in self.items() if name.lower() == key.lower()), default)
    def response(value: dict[str, Any]) -> NS:
        data = value.get("body", {})
        return NS(
            status_code=value["status"], headers=Headers(value.get("headers", {})),
            text=json.dumps(data), json=lambda: data,
        )
    def request(args: argparse.Namespace, hostname: str | None = None) -> NS:
        requests.append(args.uri)
        return response(next(replies))
    env.update(
        fabric_api=None, do_request=request,
        ErrorMessages=NS(Common=common.CommonErrors, Client=client.ClientErrors),
        fab_constant=NS(
            API_ENDPOINT_FABRIC="api.fabric.microsoft.com",
            SCOPE_FABRIC_DEFAULT="fabric-scope",
            SCOPE_AZURE_DEFAULT="arm-scope",
            ERROR_OPERATION_FAILED="LongRunningOperationFailed",
            ERROR_OPERATION_CANCELLED="LongRunningOperationCancelled",
        ),
        DEFAULT_POLLING_INTERVAL=10,
        fab_logger=NS(log_debug=lambda *a: None, log_progress=lambda *a: None),
        time=NS(sleep=waits.append),
        utils_errors=NS(map_http_status_code_to_error_code=lambda code: "NotFound"),
    )
    symbols(sources, "utils/fab_http_polling_utils.py", ["get_polling_interval"], env)
    program = symbols(sources, "client/fab_api_client.py", [
        "_handle_fab_long_running_op", "_poll_operation", "_fetch_operation_result",
        "_log_operation_progress",
    ], env)
    try:
        result = program._handle_fab_long_running_op(response(case["initial"]))
        return {"status": result.status_code, "requests": requests, "waits": waits}
    except errors.FabricCLIError as exc:
        return {"error_code": exc.status_code, "requests": requests, "waits": waits}

def observe_full_load_client(case: dict[str, Any], sources: dict[str, ast.Module]) -> None:
    """Execute the actual TABLE handler, clients, full do_request and LRO stack."""
    env = {
        "json": json, "Namespace": argparse.Namespace, "urlparse": urlparse,
        "DEFAULT_ERROR_MESSAGE": "An error occurred while processing the operation",
        "DEFAULT_ERROR_CODE": "UnknownError",
    }
    errors = symbols(sources, "core/fab_exceptions.py", ["FabricCLIError", "FabricAPIError", "OnelakeAPIError"], env)
    common = symbols(sources, "errors/common.py", ["CommonErrors"], env)
    client = symbols(sources, "errors/client.py", ["ClientErrors"], env)
    table_errors = symbols(sources, "errors/table.py", ["TableErrors"], env)
    waits, requests, scopes, output = [], [], [], {}
    replies = iter([
        {"status": 200, "headers": {"x-ms-resource-type": "file"}},
        case["initial"],
        *case.get("responses", []),
    ])
    class Headers(dict):
        def get(self, key: str, default: Any = None) -> Any:
            return next((value for name, value in self.items() if name.lower() == key.lower()), default)
    class Auth:
        def get_access_token(self, scope: list[str]) -> str:
            scopes.append(list(scope))
            assert scope in (
                ["https://api.fabric.microsoft.com/.default"],
                ["https://storage.azure.com/.default"],
            )
            return "offline-reference-only"
    class RequestException(Exception):
        pass
    class Session:
        def request(self, method: str, url: str, **kwargs: Any) -> NS:
            requests.append((method.upper(), url))
            fixture = next(replies)
            assert kwargs["timeout"] == 240
            if method.lower() == "post":
                assert json.loads(kwargs["data"]) == {
                    "pathType": "File", "relativePath": "Files/input.csv",
                    "mode": "Overwrite", "formatOptions": {"format": "Csv"},
                }
            text = json.dumps(fixture["body"]) if "body" in fixture else ""
            return NS(status_code=fixture["status"], text=text, content=text.encode(),
                      headers=Headers(fixture.get("headers", {})))
    class OneLake:
        def __init__(self, path: str) -> None:
            self.local_path = path
            self.path_id = "ws/item/" + path
            self.workspace = NS(id="ws")
            self.item = NS(id="item", item_type="Lakehouse", path="/ws.Workspace/lh.Lakehouse")
        def check_command_support(self, command: Any) -> None:
            return None
    _ALLOWED_IMPORTS.update({
        "fabric_cli.core.fab_auth": NS(FabAuth=Auth),
        "fabric_cli.core.fab_context": NS(Context=lambda: NS(command="table load", fabric_skill=None)),
    })
    noop = lambda *a, **k: None
    env.update(
        ErrorMessages=NS(Common=common.CommonErrors, Client=client.ClientErrors, Table=table_errors.TableErrors),
        fab_constant=NS(
            DEBUG=False, FAB_WS_PRIVATE_LINKS_ENABLED="private-links",
            API_ENDPOINT_FABRIC="api.fabric.microsoft.com",
            API_ENDPOINT_ONELAKE="onelake.dfs.fabric.microsoft.com",
            API_VERSION_FABRIC="v1", API_ENDPOINT_AZURE="management.azure.com",
            SCOPE_FABRIC_DEFAULT=["https://api.fabric.microsoft.com/.default"],
            SCOPE_ONELAKE_DEFAULT=["https://storage.azure.com/.default"],
            SCOPE_AZURE_DEFAULT=["https://management.azure.com/.default"], ERROR_OPERATION_FAILED="LongRunningOperationFailed",
            ERROR_OPERATION_CANCELLED="LongRunningOperationCancelled", ERROR_INVALID_PATH="InvalidPath",
            ERROR_NOT_FOUND="NotFound", ERROR_UNAUTHORIZED="Unauthorized", ERROR_FORBIDDEN="Forbidden",
        ),
        OneLakeItem=OneLake, Command=NS(TABLE_LOAD="table load"),
        handle_context=NS(get_command_context=lambda path, *a, **k: OneLake(" ".join(path))),
        handle_exceptions=lambda: lambda fn: fn, set_command_context=lambda: lambda fn: fn,
        fab_state_config=NS(get_config=lambda key: "false"),
        fab_logger=NS(log_debug=noop, log_progress=noop, log_debug_http_request=noop,
                      log_debug_http_response=noop, log_debug_http_request_exception=noop),
        requests=NS(compat=NS(urlencode=urlencode), RequestException=RequestException),
        time=NS(time=lambda: 0, sleep=waits.append), DEFAULT_POLLING_INTERVAL=10,
        _build_user_agent=lambda command: "offline-reference", _get_session=Session,
        utils_errors=NS(map_http_status_code_to_error_code=lambda code: "NotFound"),
        utils_ui=NS(print_grey=noop, print_info=noop,
                    print_output_format=lambda args, **kwargs: output.update(kwargs)),
    )
    symbols(sources, "client/fab_api_types.py", ["ApiResponse"], env)
    symbols(sources, "utils/fab_http_polling_utils.py", ["get_polling_interval"], env)
    symbols(sources, "client/fab_api_client.py", [
        "do_request", "_handle_successful_response", "_transform_workspace_url_for_private_link_if_needed",
        "_handle_fab_long_running_op", "_poll_operation", "_fetch_operation_result", "_log_operation_progress",
    ], env)
    env["fabric_api"] = NS(do_request=env["do_request"])
    env["tables_api"] = symbols(sources, "client/fab_api_tables.py", ["load_table"], env)
    env["onelake_api"] = symbols(sources, "client/fab_api_onelake.py", ["get_properties"], env)
    env["utils_table"] = symbols(sources, "utils/fab_cmd_table_utils.py",
        ["add_table_props_to_args", "parse_table_format_argument"], env)
    env["tables_load"] = symbols(sources, "commands/tables/fab_tables_load.py", ["exec_command"], env)
    handler = symbols(sources, "commands/tables/fab_tables.py", ["load_command"], env)
    args = argparse.Namespace(
        path=["Tables/sales"], file=["Files/input.csv"], format=None,
        extension=None, mode="overwrite", command="table", output_format="json",
    )
    failure = None
    try:
        handler.load_command(args)
    except errors.FabricCLIError as exc:
        failure = exc.status_code
        if "expected_error_message" in case:
            assert exc.message == case["expected_error_message"], (case["id"], exc.message)
    finally:
        _ALLOWED_IMPORTS.clear()
    assert next(replies, None) is None, "Unused scripted network response"
    expected_requests = [
        ("HEAD", "https://onelake.dfs.fabric.microsoft.com/ws/item/Files/input.csv"),
        ("POST", "https://api.fabric.microsoft.com/v1/workspaces/ws/lakehouses/item/tables/sales/load"),
        *[("GET", "https://api.fabric.microsoft.com/v1/" + uri) for uri in case["expected"]["requests"]],
    ]
    assert requests == expected_requests, (case["id"], requests)
    assert scopes == [["https://storage.azure.com/.default"]] + [["https://api.fabric.microsoft.com/.default"]] * (len(requests)-1)
    assert waits == case["expected"]["waits"]
    assert failure == case["expected"].get("error_code")
    if failure is None and case["expected"]["status"] == 204:
        assert not output
    elif failure is None:
        assert output["message"] == (
            "Load table operation started" if case["expected"]["status"] == 202
            else "Table 'sales' loaded successfully"
        )

def observe_paged_schema(case: dict[str, Any], sources: dict[str, ast.Module]) -> None:
    """Execute original schema, OneLake client and shared continuation accumulation."""
    env = {
        "json": json, "Namespace": argparse.Namespace,
        "DEFAULT_ERROR_MESSAGE": "An error occurred while processing the operation",
        "DEFAULT_ERROR_CODE": "UnknownError",
    }
    symbols(sources, "core/fab_exceptions.py", ["FabricCLIError"], env)
    symbols(sources, "client/fab_api_types.py", ["ApiResponse"], env)
    reads, tokens, output = [], [], {}
    pages = case["pages"]
    constants = NS(DEBUG=False, ERROR_INVALID_DETLA_TABLE="InvalidDeltaTable")
    class OneLake:
        workspace = NS(id="ws")
        item = NS(id="item")
        local_path = "Tables/sales/_delta_log"
    env.update(
        fab_constant=constants, OneLakeItem=OneLake,
        handle_context=NS(get_command_context=lambda *a, **k: OneLake()),
        utils=NS(remove_dot_suffix=lambda value: value.replace(".Shortcut", "").replace(".shortcut", "")),
        fab_ui=NS(print_grey=lambda *a: None, print_output_format=lambda args, **kwargs: output.update(kwargs)),
    )
    def request(args: argparse.Namespace, continuation_token: str | None = None) -> Any:
        if getattr(args, "from_path", None):
            reads.append(args.from_path)
            path = args.from_path.removeprefix("ws/")
            text = next(log["text"] for log in case["logs"] if log["name"] == path)
            return env["ApiResponse"](200, text, text.encode(), {})
        index = len(tokens)
        expected_token = None if index == 0 else pages[index-1]["continuation"]
        assert continuation_token == expected_token
        tokens.append(continuation_token)
        page = pages[index]
        text = json.dumps({"paths": page["paths"]})
        headers = {"x-ms-continuation": page["continuation"]} if page.get("continuation") else {}
        response = env["ApiResponse"](200, text, text.encode(), headers)
        return env["_handle_successful_response"](args, response)
    env["do_request"] = request
    symbols(sources, "client/fab_api_client.py", ["_handle_successful_response"], env)
    env["fabric_api"] = NS(do_request=request)
    env["onelake_api"] = symbols(sources, "client/fab_api_onelake.py",
        ["list_tables_files_recursive", "read"], env)
    schema = symbols(sources, "commands/tables/fab_tables_schema.py",
        ["_get_commit_logs", "_extract_schema_from_commit_logs", "exec_command"], env)
    schema.exec_command(argparse.Namespace(path=["Tables/sales"]))
    assert output["data"] == case["expected"]
    assert reads == case["expected_reads"]
    assert len(tokens) == case["expected_pages"]


def main() -> None:
    """Assert every hand-authored case against unchanged source."""
    manifest = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
    assert manifest["source_commit"] == SOURCE_COMMIT
    assert len(manifest["cases"]) == 31
    branches = json.loads((HERE / "branches.json").read_text(encoding="utf-8"))
    assert branches["source_commit"] == SOURCE_COMMIT and len(branches["cases"]) == 20
    decimal = json.loads((HERE / "retention-unicode.json").read_text(encoding="utf-8"))
    assert decimal["source_commit"] == SOURCE_COMMIT and len(decimal["cases"]) == 5
    wire = json.loads((HERE / "wire.json").read_text(encoding="utf-8"))
    assert wire["source_commit"] == SOURCE_COMMIT and len(wire["cases"]) == 5
    sources = load_sources()
    lifecycle = json.loads((HERE / "lifecycle.json").read_text(encoding="utf-8"))
    lifecycle_extra = json.loads((HERE / "lifecycle-extra.json").read_text(encoding="utf-8"))
    assert lifecycle_extra["source_commit"] == SOURCE_COMMIT and len(lifecycle_extra["cases"]) == 4
    paging = json.loads((HERE / "storage-paging.json").read_text(encoding="utf-8"))
    error_cases = json.loads((HERE / "errors.json").read_text(encoding="utf-8"))
    assert error_cases["source_commit"] == SOURCE_COMMIT and len(error_cases["cases"]) == 5
    assert paging["source_commit"] == SOURCE_COMMIT and len(paging["cases"]) == 2
    assert lifecycle["source_commit"] == SOURCE_COMMIT and len(lifecycle["cases"]) == 8
    with (
        patch.object(builtins, "open", deny),
        patch.object(builtins, "__import__", injected_import),
        patch.object(io, "open", deny),
        patch.object(os, "open", deny),
        patch.object(socket, "socket", deny),
        patch.object(socket, "create_connection", deny),
        patch.object(socket, "getaddrinfo", deny),
        patch.object(subprocess, "Popen", deny),
    ):
        for case in manifest["cases"] + branches["cases"] + decimal["cases"]:
            result, code, reads = observe(case, sources)
            assert code == case.get("error_code"), (case["id"], code, result)
            if "expected" in case:
                actual = result.get("data") if case["operation"] == "schema" else result.get("payload")
                assert actual == case["expected"], (case["id"], actual)
            if case["operation"] == "load" and code is None:
                assert result["request"] == {
                    "method": "post", "uri": "workspaces/ws/lakehouses/item/tables/sales/load"
                }
                if case.get("expected_no_output"):
                    assert "message" not in result
                else:
                    assert result["message"] == case.get("expected_message", "Table 'sales' loaded successfully")
            if case["operation"] == "load":
                assert result["diagnostics"] == ["Loading 'sales' table. It may take some time..."]
            if case["operation"] == "schema":
                expected = ["Getting schema for 'sales' table..."]
                if code is None or case.get("extracted_notice"):
                    expected.append("Schema extracted successfully")
                assert result["diagnostics"] == expected
            if "expected_reads" in case:
                assert reads == case["expected_reads"], (case["id"], reads)
        for case in lifecycle["cases"]:
            assert observe_lifecycle(case, sources) == case["expected"], case["id"]
            observe_full_load_client(case, sources)
        for case in lifecycle_extra["cases"]:
            observe_full_load_client(case, sources)
        for case in paging["cases"]:
            observe_paged_schema(case, sources)
        for case in error_cases["cases"]:
            observe_full_load_client(case, sources)
        for case in wire["cases"]:
            output, code, _ = observe(case, sources)
            assert code is None, case["id"]
            assert output["wire_body"] == case["expected_body"], (case["id"], output["wire_body"])
            assert output["wire_body"].isascii(), case["id"]
            assert output["request"] == {
                "method": "post", "uri": "workspaces/ws/lakehouses/item/tables/sales/load"
            }
        for count in [4300, 4301]:
            output, code, _ = observe({"operation":"vacuum","hours":"0"*count}, sources)
            if count == 4300:
                assert code is None and output["payload"]["vacuumSettings"]["retentionPeriod"] == "0:00:00:00"
            else:
                assert code == "UnexpectedError"
                assert output["unexpected_message"] == (
                    "Exceeds the limit (4300 digits) for integer string conversion: "
                    "value has 4301 digits; use sys.set_int_max_str_digits() to increase the limit"
                )
    print(f"TABLE source oracle: {len(manifest['cases'])} independent cases passed")
    print(f"TABLE source LRO oracle: {len(lifecycle['cases'])} independent cases passed")
    print(f"TABLE additional full-client LRO/result branches: {len(lifecycle_extra['cases'])} cases passed")
    print(f"TABLE full handler/client pipeline: {len(lifecycle['cases'])} source-case observations passed")
    print(f"TABLE original schema/client continuation pipeline: {len(paging['cases'])} independent cases passed")
    print(f"TABLE additional source branches: {len(branches['cases'])} independent cases passed")
    print(f"TABLE original full-client error/normalization branches: {len(error_cases['cases'])} independent cases passed")
    print(f"TABLE Unicode retention branches: {len(decimal['cases'])} cases and exact/+1 digit-limit controls passed")
    print(f"TABLE unchanged load handler literal request-byte branches: {len(wire['cases'])} cases passed")


if __name__ == "__main__":
    main()

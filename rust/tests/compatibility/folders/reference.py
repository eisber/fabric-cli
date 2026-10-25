# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute pinned folder helpers, never Rust, against in-memory HTTP/auth/time."""

from __future__ import annotations

import argparse
import ast
import builtins
import hashlib
import html
import importlib.util
import json
import re
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any
from urllib.parse import urlencode
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
SCOPE = "https://api.fabric.microsoft.com/.default"

# Reuse only the existing guard/AST compiler/header test infrastructure. No item
# expectations, item observation logic or Rust-generated values are imported.
spec = importlib.util.spec_from_file_location(
    "folder_oracle_support", HERE.parent / "items" / "reference.py"
)
assert spec is not None and spec.loader is not None
support = importlib.util.module_from_spec(spec)
spec.loader.exec_module(support)

SOURCE_SHA256 = {
    **{
        name: digest
        for name, digest in support.SOURCE_SHA256.items()
        if name not in {"client/fab_api_workspace.py", "client/fab_api_item.py"}
    },
    "client/fab_api_folders.py": "c0bef18cb3dd2e2660ae7646f1b4460ac8418fcab9f19f0b8ec20b79a7cd2282",
    "utils/fab_mem_store.py": "5f1bb375ca2e1da68c454de622b1f624f8187e73c8ef8db317f553bad9f12620",
    "core/hiearchy/fab_folder.py": "f55a68eb40ffc4c7fe56ad3768a4f275f32314ae07303b418cbbeaf2aab03324",
    "core/fab_handle_context.py": "2181706fea9e330b4fb9931e38437319d69665f55af8ea5af91a377b00424eae",
    "commands/fs/get/fab_fs_get_folder.py": "232a26087417d60933ee05aecaa5787d99f80eb9fe68fbc195abd97d28c94729",
    "commands/fs/ls/fab_fs_ls_folder.py": "2f7e9906e25527d0c99702b3e5425ab718a380592820d3b7cd3eea76ff322df2",
    "utils/fab_cmd_fs_utils.py": "9743af42473a6d54bf9feae7e8775fa3452183ec3502a8b78755cbccd84f52f1",
}


@contextmanager
def offline_boundary(modules: dict[str, ModuleType]) -> Iterator[None]:
    """Keep the guard, permitting only CPython's cached decoder error import."""
    with support.offline_boundary(modules):
        injected_import = builtins.__import__

        def cached_decoder(
            name: str,
            globals: Any = None,
            locals: Any = None,
            fromlist: Any = (),
            level: int = 0,
        ) -> ModuleType:
            if name == "json.decoder" and level == 0:
                return json.decoder
            return injected_import(name, globals, locals, fromlist, level)

        with patch.object(builtins, "__import__", cached_decoder):
            yield


def load_sources() -> dict[str, ast.Module]:
    """Hash full files, then parse without importing the product or dependencies."""
    sources = {}
    for name, digest in SOURCE_SHA256.items():
        raw = ROOT.joinpath("src", "fabric_cli", *name.split("/")).read_bytes()
        if hashlib.sha256(raw.replace(b"\r\n", b"\n")).hexdigest() != digest:
            raise AssertionError(
                f"Source drift in {name}; do not regenerate expectations"
            )
        sources[name] = ast.parse(raw.decode("utf-8"), filename=name)
    return sources


def load_cases() -> dict[str, Any]:
    """Read hand-authored expectations with no recording/update path."""
    manifest = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
    assert manifest["source_commit"] == SOURCE_COMMIT
    assert manifest["_copyright"] == support.COPYRIGHT
    assert manifest["scope"] == SCOPE
    assert (len(manifest["cases"]), len(manifest["python_only"])) == (26, 13)
    cases = manifest["cases"] + manifest["python_only"]
    assert len({case["id"] for case in cases}) == len(cases)
    assert all(case["reason"] for case in manifest["python_only"])
    return manifest


def observe(case: dict[str, Any], sources: dict[str, ast.Module]) -> dict[str, Any]:
    """Execute actual list_folders/get_folder/do_request/ApiResponse source AST."""
    selections = [
        (
            "core/fab_exceptions.py",
            {
                "DEFAULT_ERROR_MESSAGE",
                "DEFAULT_ERROR_CODE",
                "FabricCLIError",
                "FabricAPIError",
                "OnelakeAPIError",
                "AzureAPIError",
            },
        ),
        ("errors/common.py", {"CommonErrors"}),
        ("client/fab_api_types.py", {"ApiResponse"}),
        (
            "client/fab_api_client.py",
            {
                "do_request",
                "_handle_successful_response",
                "_transform_workspace_url_for_private_link_if_needed",
            },
        ),
        ("client/fab_api_folders.py", {"list_folders", "get_folder"}),
    ]
    programs = [
        support._compile_symbols(sources, path, names) for path, names in selections
    ]
    constant_names = {
        "API_ENDPOINT_FABRIC",
        "API_ENDPOINT_ONELAKE",
        "API_ENDPOINT_AZURE",
        "API_ENDPOINT_POWER_BI",
        "API_VERSION_FABRIC",
        "SCOPE_FABRIC_DEFAULT",
        "SCOPE_ONELAKE_DEFAULT",
        "SCOPE_AZURE_DEFAULT",
        "FAB_WS_PRIVATE_LINKS_ENABLED",
        "DEBUG",
    }
    constant_names.update(
        target.id
        for node in sources["core/fab_constant.py"].body
        if isinstance(node, ast.Assign)
        for target in node.targets
        if isinstance(target, ast.Name) and target.id.startswith("ERROR_")
    )
    constant_program = support._compile_symbols(
        sources, "core/fab_constant.py", constant_names
    )
    constants = ModuleType("offline_folder_constants")
    constants.__dict__["validate_and_get_env_variable"] = lambda name, default: default
    requests_seen: list[dict[str, str]] = []
    scopes: list[list[str]] = []
    consumed = 0

    class Auth:
        def get_access_token(self, scope: list[str]) -> str:
            scopes.append(list(scope))
            assert scope == [SCOPE]
            return "offline-folder-source-only"

    class Session:
        def request(self, method: str, url: str, **kwargs: Any) -> Any:
            nonlocal consumed
            assert kwargs == {
                "headers": {
                    "Authorization": "Bearer offline-folder-source-only",
                    "User-Agent": "offline-reference",
                    "Content-Type": "application/json",
                },
                "timeout": 240,
            }
            assert len(scopes) == len(requests_seen) + 1
            requests_seen.append({"method": method.upper(), "url": url, "scope": SCOPE})
            if consumed >= len(case["responses"]):
                raise support.OfflineViolation("Unscripted HTTP request")
            fixture = case["responses"][consumed]
            consumed += 1
            text = fixture["text"] if "text" in fixture else json.dumps(fixture["body"])
            return SimpleNamespace(
                status_code=fixture["status"],
                text=text,
                content=text.encode(),
                headers=support._Headers(fixture.get("headers", {})),
            )

    def config(name: str) -> str:
        assert name == constants.FAB_WS_PRIVATE_LINKS_ENABLED
        return "false"

    modules = {
        name: ModuleType(name)
        for name in (
            "fabric_cli",
            "fabric_cli.core",
            "fabric_cli.core.fab_auth",
            "fabric_cli.core.fab_context",
        )
    }
    modules["fabric_cli.core.fab_auth"].FabAuth = Auth
    modules["fabric_cli.core.fab_context"].Context = lambda: SimpleNamespace(
        command="offline-folders", fabric_skill=None
    )
    env: dict[str, Any] = {
        "__name__": "offline_folder_source",
        "json": json,
        "html": html,
        "re": re,
        "fab_constant": constants,
        "fabric_api": SimpleNamespace(),
        "fab_state_config": SimpleNamespace(get_config=config),
        "fab_logger": SimpleNamespace(
            log_debug_http_request=lambda *args: None,
            log_debug_http_response=lambda *args: None,
            log_debug_http_request_exception=lambda *args: None,
        ),
        "requests": SimpleNamespace(
            compat=SimpleNamespace(urlencode=urlencode), RequestException=OSError
        ),
        "time": SimpleNamespace(time=lambda: 0, sleep=support._deny),
        "_build_user_agent": lambda command: "offline-reference",
        "_get_session": Session,
        "_handle_fab_long_running_op": support._deny,
        "_handle_azure_async_op": support._deny,
        "get_polling_interval": support._deny,
        "files_utils": SimpleNamespace(load_json_from_path=support._deny),
        "utils_ui": SimpleNamespace(print_info=support._deny),
    }
    args = argparse.Namespace(ws_id=case.get("workspace_id", "ws"))
    if not case.get("request_params_absent"):
        args.request_params = dict(case.get("query", []))
    if case["operation"] == "get":
        args.folder_id = case.get("folder_id", "f")
    elif "root_folder_id" in case:
        args.folder_id = case["root_folder_id"]
    for key in ("recursive", "wait"):
        if key in case:
            setattr(args, key, case[key])
    result: dict[str, Any] = {"requests": requests_seen, "auth_scopes": scopes}
    with offline_boundary(modules):
        exec(constant_program, constants.__dict__)
        for program in programs:
            exec(program, env)
        env["ErrorMessages"] = SimpleNamespace(Common=env["CommonErrors"])
        env["fabric_api"].do_request = env["do_request"]
        try:
            response = env[
                f"{case['operation']}_folder"
                + ("s" if case["operation"] == "list" else "")
            ](args)
            try:
                body = json.loads(response.text)
            except json.JSONDecodeError:
                result["python"] = {
                    "status": response.status_code,
                    "text": response.text,
                }
            else:
                result["python"] = {"status": response.status_code, "body": body}
                if case["operation"] == "get" and isinstance(body, dict):
                    result["rows"] = [body]
                elif isinstance(body, dict) and isinstance(body.get("value"), list):
                    result["rows"] = body["value"]
        except env["FabricCLIError"] as error:
            result["error_code"] = error.status_code
        except (ValueError, TypeError, AttributeError, KeyError) as error:
            result["python"] = {"exception": type(error).__name__}
    assert consumed == len(case["responses"]), case["id"]
    return result


def check_case(case: dict[str, Any], actual: dict[str, Any]) -> None:
    """Compare literal hand-authored URLs, rows, error codes and Python quirks."""
    expected = case["expected"]
    urls = ["https://api.fabric.microsoft.com" + url for url in expected["urls"]]
    assert actual["requests"] == [
        {"method": "GET", "url": url, "scope": SCOPE} for url in urls
    ], (case["id"], actual)
    assert actual["auth_scopes"] == [[SCOPE]] * len(urls), case["id"]
    for key in ("rows", "error_code", "python"):
        if key in expected:
            assert support._same_json(actual.get(key), expected[key]), (
                case["id"],
                key,
                actual,
            )
    if "error_code" in expected:
        assert "rows" not in actual, case["id"]


def main() -> None:
    """Check independent synthetic cases; never generate expected values."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    options = parser.parse_args()
    manifest, sources = load_cases(), load_sources()
    for case in manifest["cases"] + manifest["python_only"]:
        actual = observe(case, sources)
        check_case(case, actual)
        if not options.check:
            print(json.dumps({"id": case["id"], **actual}, ensure_ascii=True))
    print(f"Checked 26 comparable + 13 Python-only folder cases at {SOURCE_COMMIT}")


if __name__ == "__main__":
    main()

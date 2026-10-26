# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute source-pinned Python item helpers with entirely in-memory boundaries."""

from __future__ import annotations

import __future__
import argparse
import ast
import builtins
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
from collections.abc import Iterator, MutableMapping
from contextlib import contextmanager
from pathlib import Path
from types import CodeType, ModuleType, SimpleNamespace
from typing import Any
from unittest.mock import patch
from urllib.parse import urlencode

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
SCOPE = "https://api.fabric.microsoft.com/.default"
COPYRIGHT = "Copyright (c) Microsoft Corporation. Licensed under the MIT License."
SOURCE_SHA256 = {
    "client/fab_api_workspace.py": "c2512eec29b12b60a29e013ff0a296833f86e59904c76418ef406e903eb49ab8",
    "client/fab_api_item.py": "d0f257eb327f759bbdc468a8f9be1af4cce0116e7f4ee6f8e2536c10ca36d740",
    "client/fab_api_client.py": "d7d978f315921ec4559a2fcefc992435152758b74470d1d969006def088e8e0d",
    "client/fab_api_types.py": "990708f777081bb07fdd80906bcd4e49f22f4f1af6e7cd522808a89ff3b34c40",
    "core/fab_exceptions.py": "85826f456cc9f2bfd026950fc0338b04570cf56c571513f1cef1feac386db0dc",
    "core/fab_constant.py": "20dd87282310f0217b3f3724214c280c3dab123bec5f7fafa770e1c2c44bfff0",
    "errors/common.py": "f0af9183ba94b1ab89f6bd7afe1bca71538704afd5570e73b913fd45b2c1154d",
}


class OfflineViolation(RuntimeError):
    """Reject an attempted escape from the in-memory source boundary."""


class _Headers(MutableMapping[str, str]):
    """Minimal requests-compatible case-insensitive response header mapping."""

    def __init__(self, values: dict[str, str]) -> None:
        self._values: dict[str, tuple[str, str]] = {}
        self.update(values)

    def __getitem__(self, key: str) -> str:
        return self._values[key.lower()][1]

    def __setitem__(self, key: str, value: str) -> None:
        self._values[key.lower()] = (key, value)

    def __delitem__(self, key: str) -> None:
        del self._values[key.lower()]

    def __iter__(self) -> Iterator[str]:
        return (entry[0] for entry in self._values.values())

    def __len__(self) -> int:
        return len(self._values)


def _deny(*args: Any, **kwargs: Any) -> Any:
    raise OfflineViolation("I/O, real imports, sleeping, and processes are forbidden")


@contextmanager
def offline_boundary(modules: dict[str, ModuleType]) -> Iterator[None]:
    """Deny I/O and permit only the two explicitly injected source imports."""
    active = True
    violations: list[str] = []

    def deny(*args: Any, **kwargs: Any) -> Any:
        violations.append("forbidden side effect")
        return _deny()

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

    def injected_import(
        name: str,
        globals: Any = None,
        locals: Any = None,
        fromlist: Any = (),
        level: int = 0,
    ) -> ModuleType:
        if level == 0 and name in {
            "fabric_cli.core.fab_auth",
            "fabric_cli.core.fab_context",
        }:
            return modules[name]
        return deny()

    sys.addaudithook(audit)
    try:
        with (
            patch.dict(sys.modules, modules),
            patch.object(builtins, "__import__", injected_import),
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


def load_cases() -> dict[str, Any]:
    """Load independently hand-authored goldens without a recording/update mode."""
    manifest = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
    if (
        manifest["source_commit"] != SOURCE_COMMIT
        or manifest["_copyright"] != COPYRIGHT
    ):
        raise AssertionError("The pinned Python source commit changed")
    if (len(manifest["cases"]), len(manifest["python_only"])) != (30, 26):
        raise AssertionError("The independent case inventory changed")
    all_cases = manifest["cases"] + manifest["python_only"]
    ids = [case["id"] for case in all_cases]
    if (
        any(not isinstance(case_id, str) or not case_id for case_id in ids)
        or len(ids) != len(set(ids))
    ):
        raise AssertionError("Case IDs must be nonempty and globally unique")
    for case in all_cases:
        if case["operation"] not in {"list", "get"}:
            raise AssertionError("Only LIST/GET source boundaries are supported")
        if case["operation"] == "get" and "item_id" not in case:
            raise AssertionError("GET fixtures must supply item_id")
        if not isinstance(case["responses"], list):
            raise AssertionError("Responses must be an ordered script")
    for case in manifest["cases"]:
        expected = case["expected"]
        if set(expected) not in (
            {"requests", "rows"},
            {"requests", "error_code"},
        ):
            raise AssertionError("Comparable cases require requests and rows OR code")
    for case in manifest["python_only"]:
        if not case.get("reason"):
            raise AssertionError("Python-only exceptions require an explicit rationale")
    return manifest


def _verify_digests(sources: dict[str, bytes]) -> None:
    if set(sources) != set(SOURCE_SHA256):
        raise AssertionError("The executed Python source inventory changed")
    for name, expected in SOURCE_SHA256.items():
        actual = hashlib.sha256(sources[name].replace(b"\r\n", b"\n")).hexdigest()
        if actual != expected:
            raise AssertionError(f"Source drift in {name}; do not regenerate goldens")


def load_sources() -> dict[str, ast.Module]:
    """Verify full source files, then parse them without importing product code."""
    source_root = ROOT / "src" / "fabric_cli"
    sources = {
        name: source_root.joinpath(*name.split("/")).read_bytes()
        for name in SOURCE_SHA256
    }
    _verify_digests(sources)
    return {
        name: ast.parse(content.decode("utf-8"), filename=name)
        for name, content in sources.items()
    }


def _compile_symbols(
    sources: dict[str, ast.Module], path: str, names: set[str]
) -> CodeType:
    nodes: list[ast.stmt] = []
    found: set[str] = set()
    for node in sources[path].body:
        node_names: set[str] = set()
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)):
            node_names.add(node.name)
        elif isinstance(node, ast.Assign):
            node_names.update(
                target.id for target in node.targets if isinstance(target, ast.Name)
            )
        if node_names & names:
            nodes.append(node)
            found.update(node_names & names)
    if found != names:
        raise AssertionError(f"Missing pinned source symbols: {names - found}")
    return compile(
        ast.Module(body=nodes, type_ignores=[]),
        path,
        "exec",
        flags=__future__.annotations.compiler_flag,
        dont_inherit=True,
    )


def _programs(sources: dict[str, ast.Module]) -> list[CodeType]:
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
        ("client/fab_api_workspace.py", {"ls_workspace_items"}),
        ("client/fab_api_item.py", {"get_item"}),
    ]
    return [_compile_symbols(sources, path, names) for path, names in selections]


def observe(
    case: dict[str, Any], sources: dict[str, ast.Module] | None = None
) -> dict[str, Any]:
    """Call the actual extracted helpers; only transport/auth/context are fake."""
    sources = load_sources() if sources is None else sources
    programs = _programs(sources)
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
    constant_program = _compile_symbols(sources, "core/fab_constant.py", constant_names)
    constants = ModuleType("offline_constants")
    constants.__dict__["validate_and_get_env_variable"] = lambda name, default: default
    requests_seen: list[dict[str, str]] = []
    scopes: list[list[str]] = []
    response_index = 0
    env: dict[str, Any] = {}

    class SyntheticRequestException(Exception):
        pass

    class TestAuth:
        def get_access_token(self, scope: list[str]) -> str:
            scopes.append(list(scope))
            if scope != [SCOPE]:
                raise AssertionError("Unexpected source credential scope")
            if "auth_error" in case:
                raise env["FabricCLIError"](
                    "Synthetic authentication failure", case["auth_error"]
                )
            return "offline-item-reference-only"

    class ScriptedSession:
        def request(self, method: str, url: str, **kwargs: Any) -> Any:
            nonlocal response_index
            if kwargs != {
                "headers": {
                    "Authorization": "Bearer offline-item-reference-only",
                    "User-Agent": "offline-reference",
                    "Content-Type": "application/json",
                },
                "timeout": 240,
            }:
                raise AssertionError("Unexpected HTTP boundary arguments")
            if len(scopes) != len(requests_seen) + 1:
                raise AssertionError("Each HTTP attempt must acquire its scope first")
            requests_seen.append({"method": method.upper(), "url": url, "scope": SCOPE})
            if response_index >= len(case["responses"]):
                raise OfflineViolation("Unexpected additional HTTP request")
            fixture = case["responses"][response_index]
            response_index += 1
            text = json.dumps(fixture["body"])
            return SimpleNamespace(
                status_code=fixture["status"],
                headers=_Headers(fixture["headers"]),
                text=text,
                content=text.encode("utf-8"),
            )

    def private_link_config(name: str) -> str:
        if name != constants.FAB_WS_PRIVATE_LINKS_ENABLED:
            raise OfflineViolation("Unexpected ambient configuration access")
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
    modules["fabric_cli.core.fab_auth"].FabAuth = TestAuth  # type: ignore[attr-defined]
    modules["fabric_cli.core.fab_context"].Context = (  # type: ignore[attr-defined]
        lambda: SimpleNamespace(command="offline-items", fabric_skill=None)
    )
    env.update(
        {
            "__name__": "offline_item_source",
            "json": json,
            "html": html,
            "re": re,
            "fab_constant": constants,
            "fabric_api": SimpleNamespace(),
            "fab_state_config": SimpleNamespace(get_config=private_link_config),
            "fab_logger": SimpleNamespace(
                log_debug_http_request=lambda *args: None,
                log_debug_http_response=lambda *args: None,
                log_debug_http_request_exception=lambda *args: None,
            ),
            "requests": SimpleNamespace(
                compat=SimpleNamespace(urlencode=urlencode),
                RequestException=SyntheticRequestException,
            ),
            "time": SimpleNamespace(time=lambda: 0, sleep=_deny),
            "_build_user_agent": lambda command: "offline-reference",
            "_get_session": lambda: ScriptedSession(),
            "_handle_fab_long_running_op": _deny,
            "_handle_azure_async_op": _deny,
            "get_polling_interval": _deny,
            "files_utils": SimpleNamespace(load_json_from_path=_deny),
            "utils_ui": SimpleNamespace(print_info=_deny),
        }
    )
    args = argparse.Namespace(
        ws_id=case["workspace_id"],
        id=case.get("item_id"),
        request_params=dict(case.get("query", [])),
    )
    if "item_uri" in case:
        args.item_uri = case["item_uri"]
    if "ext_uri" in case:
        args.ext_uri = case["ext_uri"]
    result: dict[str, Any] = {"requests": requests_seen, "auth_scopes": scopes}
    with offline_boundary(modules):
        exec(constant_program, constants.__dict__)
        for program in programs:
            exec(program, env)
        env["ErrorMessages"] = SimpleNamespace(Common=env["CommonErrors"])
        env["fabric_api"].do_request = env["do_request"]
        try:
            if case["operation"] == "list":
                response = env["ls_workspace_items"](args, case["workspace_id"])
            else:
                response = env["get_item"](
                    args,
                    item_uri="item_uri" in case,
                    ext_uri="ext_uri" in case,
                )
            body = json.loads(response.text)
            result["python"] = {"status_code": response.status_code, "body": body}
            if case["operation"] == "get" and isinstance(body, dict):
                result["rows"] = [body]
            elif isinstance(body, dict) and isinstance(body.get("value"), list):
                result["rows"] = body["value"]
        except env["FabricCLIError"] as exc:
            result["error_code"] = exc.status_code
            result["python"] = {
                "exception_type": type(exc).__name__,
                "error_code": exc.status_code,
                "message": exc.message,
                "request_id": getattr(exc, "request_id", None),
            }
        except (ValueError, TypeError, AttributeError, KeyError) as exc:
            result["python"] = {
                "exception_type": type(exc).__name__,
                "message": str(exc),
            }
    if response_index != len(case["responses"]):
        raise AssertionError(f"Unused scripted responses in {case['id']}")
    return result


def _same_json(actual: Any, expected: Any) -> bool:
    if type(actual) is not type(expected):
        return False
    if isinstance(actual, dict):
        return actual.keys() == expected.keys() and all(
            _same_json(actual[key], expected[key]) for key in actual
        )
    if isinstance(actual, list):
        return len(actual) == len(expected) and all(
            _same_json(left, right) for left, right in zip(actual, expected)
        )
    return bool(actual == expected)


def assert_observation(case: dict[str, Any], actual: dict[str, Any]) -> None:
    """Check independent goldens, including complete raw parsed success bodies."""
    for key, value in case["expected"].items():
        if key not in actual or not _same_json(actual[key], value):
            raise AssertionError(
                f"{case['id']}: {key} differs\nexpected={value!r}\nactual={actual.get(key)!r}"
            )
    expected_python = case["python_expected"]
    if not _same_json(actual["python"], expected_python):
        raise AssertionError(
            f"{case['id']}: Python boundary differs\n"
            f"expected={expected_python!r}\nactual={actual['python']!r}"
        )
    auth_count = len(case["expected"]["requests"]) + ("auth_error" in case)
    if actual["auth_scopes"] != [[SCOPE]] * auth_count:
        raise AssertionError(f"{case['id']}: credential count/scope differs")
    if "error_code" in case["expected"] and "rows" in actual:
        raise AssertionError(f"{case['id']}: an error must not expose partial rows")


def main() -> None:
    """Check or print observations, never write or regenerate fixture expectations."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="check hand-authored goldens")
    parser.add_argument("--case", help="select one exact case ID")
    parser.add_argument(
        "--include-python-only",
        action="store_true",
        help="also exercise explicitly non-comparable source quirks",
    )
    options = parser.parse_args()
    manifest = load_cases()
    sources = load_sources()
    cases = manifest["cases"]
    if options.include_python_only:
        cases = cases + manifest["python_only"]
    if options.case:
        cases = [case for case in cases if case["id"] == options.case]
        if not cases:
            parser.error("case not found (Python-only cases require --include-python-only)")
    for case in cases:
        actual = observe(case, sources)
        assert_observation(case, actual)
        if not options.check:
            print(json.dumps({"id": case["id"], **actual}, ensure_ascii=True))
    if options.check:
        print(f"Checked {len(cases)} independent item cases against {SOURCE_COMMIT}")


if __name__ == "__main__":
    main()

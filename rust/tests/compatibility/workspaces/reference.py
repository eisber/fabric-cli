# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Run the pinned Python workspace-list helpers against independent offline cases."""

from __future__ import annotations

import argparse
import hashlib
import importlib
import importlib.abc
import io
import json
import os
import socket
import subprocess
import sys
import tempfile
from contextlib import ExitStack, contextmanager, redirect_stderr, redirect_stdout
from datetime import datetime, timezone
from pathlib import Path
from types import ModuleType
from typing import Any, Callable, Iterator
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
FIXTURES = HERE / "cases.json"
FROZEN_TIMESTAMP = "2026-01-01T00:00:00Z"
FABRIC_SCOPE = ["https://api.fabric.microsoft.com/.default"]
TEST_TOKEN = "offline-reference-only"
PACKAGES = (
    "requests",
    "cachetools",
    "jmespath",
    "yaml",
    "psutil",
    "questionary",
    "argcomplete",
)


class OfflineViolation(RuntimeError):
    """Report a forbidden side effect without allowing the operation."""


class _DenyIdentityImports(importlib.abc.MetaPathFinder):
    def __init__(self, deny: Callable[[str], None]) -> None:
        self._deny = deny

    def find_spec(self, fullname: str, path: Any = None, target: Any = None) -> None:
        if fullname.split(".")[0] in {"msal", "msal_extensions", "azure"}:
            self._deny("Real identity dependencies are forbidden")
        return None


def load_cases() -> dict[str, Any]:
    """Read immutable, hand-authored inputs and expected results."""
    manifest = json.loads(FIXTURES.read_text(encoding="utf-8"))
    cases = manifest["cases"]
    ids = [case["id"] for case in cases]
    if manifest["schema_version"] != 1 or len(ids) != 8 or len(set(ids)) != 8:
        raise ValueError("The workspace reference requires exactly eight unique cases")
    return manifest


def verify_source(provenance: dict[str, Any]) -> None:
    """Reject source drift; normalize only checkout CRLF to repository LF."""
    source = ROOT / "src"
    entries = []
    for path in sorted(
        source.rglob("*"), key=lambda entry: entry.relative_to(source).as_posix()
    ):
        if path.is_file() and path.suffix in {".py", ".yaml"}:
            content = path.read_bytes().replace(b"\r\n", b"\n")
            digest = hashlib.sha256(content).hexdigest()
            entries.append(f"{path.relative_to(source).as_posix()}\0{digest}\n")
    digest = hashlib.sha256("".join(entries).encode()).hexdigest()
    if (
        len(entries) != provenance["source_file_count"]
        or digest != provenance["source_tree_sha256"]
    ):
        raise AssertionError(
            "Python source drift: owner review is required, not a baseline update"
        )


@contextmanager
def offline_boundary() -> Iterator[tuple[Path, list[list[str]], list[str]]]:
    """Isolate ambient state and deny networking, identity discovery, and writes."""
    sys.dont_write_bytecode = True
    with tempfile.TemporaryDirectory(prefix="fab-workspace-reference-") as directory:
        home = Path(directory)
        config = home / ".config" / "fab"
        config.mkdir(parents=True)
        runtime_env = {
            key: os.environ[key]
            for key in ("SYSTEMROOT", "WINDIR")
            if key in os.environ
        }
        runtime_env.update(
            {
                "HOME": str(home),
                "USERPROFILE": str(home),
                "APPDATA": str(home),
                "LOCALAPPDATA": str(home),
                "XDG_CONFIG_HOME": str(home / ".config"),
                "TEMP": str(home),
                "TMP": str(home),
                "NO_COLOR": "1",
                "TERM": "dumb",
            }
        )
        violations: list[str] = []
        scopes: list[list[str]] = []
        active = True

        def deny(reason: str) -> None:
            violations.append(reason)
            raise OfflineViolation(reason)

        def audit(event: str, args: tuple[Any, ...]) -> None:
            if not active:
                return
            if event == "open":
                mode, flags = args[1:3]
                if (isinstance(mode, str) and any(c in mode for c in "wax+")) or (
                    isinstance(flags, int)
                    and flags
                    & (os.O_WRONLY | os.O_RDWR | os.O_CREAT | os.O_TRUNC | os.O_APPEND)
                ):
                    deny("File writes are forbidden")
            if event.startswith("socket.") and event not in {"socket.__new__"}:
                deny("Network operations are forbidden")
            if event in {
                "subprocess.Popen",
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
            }:
                deny(f"Side effect is forbidden: {event}")

        class TestAuth:
            def get_access_token(self, scope: list[str]) -> str:
                scopes.append(list(scope))
                return TEST_TOKEN

        auth = ModuleType("fabric_cli.core.fab_auth")
        setattr(auth, "FabAuth", TestAuth)
        identity_guard = _DenyIdentityImports(deny)
        sys.addaudithook(audit)
        sys.meta_path.insert(0, identity_guard)
        try:
            with ExitStack() as stack:
                stack.enter_context(patch.dict(os.environ, runtime_env, clear=True))
                stack.enter_context(patch.dict(sys.modules, {auth.__name__: auth}))
                # urllib3 otherwise binds an IPv6 socket while importing.
                stack.enter_context(patch.object(socket, "has_ipv6", False))
                stack.enter_context(
                    patch("time.sleep", side_effect=lambda _: deny("Sleeping is forbidden"))
                )
                yield home, scopes, violations
        finally:
            active = False
            sys.meta_path.remove(identity_guard)


def _prepare_imports(home: Path) -> None:
    sys.path.insert(0, str(ROOT / "src"))
    for package in PACKAGES:
        importlib.import_module(package)
    from fabric_cli.utils import fab_secure_io

    def existing_config_only(directory: str) -> None:
        if Path(directory).resolve() != (home / ".config" / "fab").resolve():
            raise OfflineViolation("Unexpected configuration directory")
        if not Path(directory).is_dir():
            raise OfflineViolation("Isolated configuration directory is missing")

    # fab_state_config creates its directory at import time. The harness has
    # already created that empty directory; product writes remain audit-denied.
    fab_secure_io.create_restricted_dir = existing_config_only


def _observe(case: dict[str, Any]) -> dict[str, Any]:
    with offline_boundary() as (home, scopes, violations):
        _prepare_imports(home)
        import requests
        from fabric_cli import main as python_main
        from fabric_cli.client import fab_api_client, fab_api_workspace
        from fabric_cli.commands.fs.ls import fab_fs_ls_workspace
        from fabric_cli.core import fab_handle_context, fab_output
        from fabric_cli.core.fab_commands import Command
        from fabric_cli.core.fab_context import Context
        from fabric_cli.core.fab_decorators import handle_exceptions, set_command_context
        from fabric_cli.core.fab_exceptions import FabricCLIError
        from fabric_cli.core.hiearchy.fab_tenant import Tenant
        from fabric_cli.core.hiearchy.fab_workspace import Workspace
        from fabric_cli.utils import fab_ui

        requests_seen: list[dict[str, str]] = []
        api_ids: list[str] | None = None
        api_total_pages = None
        exception: dict[str, Any] | None = None
        response_index = 0

        class ScriptedSession:
            def request(self, method: str, url: str, **kwargs: Any) -> Any:
                nonlocal response_index
                requests_seen.append({"method": method, "url": url})
                if response_index >= len(case["responses"]):
                    raise OfflineViolation("Unexpected additional HTTP request")
                if kwargs["headers"]["Authorization"] != f"Bearer {TEST_TOKEN}":
                    raise OfflineViolation("Unexpected credential source")
                if kwargs["timeout"] != 240:
                    raise AssertionError("Python request timeout changed")
                fixture = case["responses"][response_index]
                response_index += 1
                response = requests.Response()
                response.status_code = fixture["status"]
                response.headers.update(fixture.get("headers", {}))
                response._content = json.dumps(fixture["body"]).encode("utf-8")
                response.encoding = "utf-8"
                return response

        class FrozenDatetime(datetime):
            @classmethod
            def now(cls, tz: Any = None) -> datetime:
                return datetime(2026, 1, 1, tzinfo=timezone.utc)

        original_list = fab_api_workspace.list_workspaces
        original_error = fab_ui.print_output_error

        def capture_list(args: argparse.Namespace) -> Any:
            nonlocal api_ids, api_total_pages
            response = original_list(args)
            body = json.loads(response.text)
            value = body.get("value")
            api_ids = (
                [workspace["id"] for workspace in value]
                if isinstance(value, list)
                else None
            )
            api_total_pages = body.get("total_pages")
            return response

        def capture_error(error: FabricCLIError, *args: Any, **kwargs: Any) -> None:
            nonlocal exception
            if exception is None:
                exception = {
                    "type": type(error).__name__,
                    "code": error.status_code,
                    "request_id": getattr(error, "request_id", None),
                }
            original_error(error, *args, **kwargs)

        @handle_exceptions()
        @set_command_context()
        def invoke(args: argparse.Namespace) -> None:
            context = fab_handle_context.get_command_context(args.path)
            context.check_command_support(Command.FS_LS)
            if not isinstance(context, Tenant):
                raise AssertionError(
                    "This reference only exercises tenant workspace listing"
                )
            fab_fs_ls_workspace.exec(context, args)

        tenant = Tenant("Offline tenant", "00000000-0000-0000-0000-000000000000")
        context = Context()
        current = case.get("current_workspace")
        context.context = (
            Workspace(current["displayName"], current["id"], tenant, current["type"])
            if current
            else tenant
        )
        before = context.context.path
        args = argparse.Namespace(
            command="ls",
            command_path="ls",
            output_format="json",
            path=case.get("path", "/"),
            query=case.get("query"),
            long=False,
            all=case.get("all", False),
        )
        stdout, stderr = io.StringIO(), io.StringIO()
        with ExitStack() as stack:
            stack.enter_context(
                patch.object(
                    fab_api_client, "_get_session", return_value=ScriptedSession()
                )
            )
            stack.enter_context(
                patch.object(
                    requests.Session,
                    "request",
                    side_effect=OfflineViolation("Unscripted HTTP"),
                )
            )
            stack.enter_context(
                patch.object(fab_api_workspace, "list_workspaces", capture_list)
            )
            stack.enter_context(patch.object(fab_ui, "print_output_error", capture_error))
            stack.enter_context(patch.object(fab_output, "datetime", FrozenDatetime))
            stack.enter_context(redirect_stdout(stdout))
            stack.enter_context(redirect_stderr(stderr))
            try:
                exit_code = invoke(args) or 0
            except KeyError as error:
                exception = {"type": "KeyError", "message": str(error.args[0])}
                try:
                    python_main._handle_unexpected_error(error, args)
                except SystemExit as exit_error:
                    exit_code = exit_error.code

        if response_index != len(case["responses"]):
            raise AssertionError("Not all scripted HTTP responses were consumed")
        if violations or any(path.is_file() for path in home.rglob("*")):
            raise OfflineViolation("Offline side-effect guard failed")
        if context.context.path != before:
            raise AssertionError("Workspace listing changed current context")
        return {
            "exit_code": exit_code,
            "stdout": stdout.getvalue(),
            "stderr": stderr.getvalue(),
            "requests": requests_seen,
            "credential_scopes": scopes,
            "api_ids": api_ids,
            "api_total_pages": api_total_pages,
            "exception": exception,
            "context_before": before,
            "context_after": context.context.path,
        }


def _worker(arguments: list[str]) -> dict[str, Any]:
    environment = {
        key: value
        for key, value in os.environ.items()
        if key.upper() in {"SYSTEMROOT", "WINDIR", "TEMP", "TMP"}
    }
    process = subprocess.run(
        [sys.executable, "-I", str(HERE / "reference.py"), *arguments],
        env=environment,
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=30,
        check=False,
    )
    if process.returncode or process.stderr:
        raise AssertionError(
            f"Reference worker failed ({process.returncode}): {process.stderr or process.stdout}"
        )
    return json.loads(process.stdout)


def observe_case(case_id: str) -> dict[str, Any]:
    """Observe a case in a fresh isolated interpreter, without rewriting fixtures."""
    return _worker(["--observe", case_id])


def assert_observation(case: dict[str, Any], observed: dict[str, Any]) -> None:
    """Compare streams, exit, data, request order, errors, scopes, and context."""
    expected = case["expected"]
    output = {"timestamp": FROZEN_TIMESTAMP, **expected["output"]}
    current = case.get("current_workspace")
    before = f"/{current['displayName']}.{current['type']}" if current else "/"
    contract = {
        "exit_code": expected["exit_code"],
        "stdout": json.dumps(output, indent=4) + "\n",
        "stderr": "",
        "requests": [{"method": "get", "url": url} for url in expected["requests"]],
        "credential_scopes": [FABRIC_SCOPE for _ in expected["requests"]],
        "api_ids": expected["api_ids"],
        "api_total_pages": expected["api_total_pages"],
        "exception": expected["exception"],
        "context_before": before,
        "context_after": before,
    }
    if observed != contract:
        differing = [key for key in contract if observed.get(key) != contract[key]]
        raise AssertionError(
            f"{case['id']}: contract mismatch in {', '.join(differing)}\n"
            f"Expected: {json.dumps(contract, sort_keys=True)}\n"
            f"Observed: {json.dumps(observed, sort_keys=True)}"
        )


def probe_boundary(probe: str) -> dict[str, Any]:
    """Prove that forbidden side effects fail before doing the operation."""
    return _worker(["--probe", probe])


def _probe(probe: str) -> dict[str, Any]:
    with offline_boundary() as (home, _, _):
        try:
            if probe == "network":
                socket.getaddrinfo("offline.invalid", 443)
            elif probe == "write":
                (home / "forbidden.json").write_text("{}", encoding="utf-8")
            elif probe == "identity":
                importlib.import_module("msal")
            elif probe == "process":
                subprocess.run([sys.executable, "--version"], check=True)
            elif probe == "sleep":
                import time

                time.sleep(1)
            else:
                raise ValueError("Unknown boundary probe")
        except OfflineViolation:
            return {"probe": probe, "blocked": True}
        raise AssertionError(f"The {probe} guard did not block its operation")


def main() -> None:
    """Check the immutable reference or run one internal isolated observation."""
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument("--check", action="store_true")
    action.add_argument(
        "--observe", choices=[case["id"] for case in load_cases()["cases"]]
    )
    action.add_argument(
        "--probe", choices=["network", "write", "identity", "process", "sleep"]
    )
    args = parser.parse_args()
    manifest = load_cases()
    verify_source(manifest["provenance"])
    if args.probe:
        print(json.dumps(_probe(args.probe)))
    elif args.observe:
        case = next(case for case in manifest["cases"] if case["id"] == args.observe)
        print(json.dumps(_observe(case)))
    else:
        for case in manifest["cases"]:
            assert_observation(case, observe_case(case["id"]))
            print(f"PASS {case['id']}")
        print(
            "8/8 offline Python reference cases passed; "
            f"source {manifest['provenance']['commit']}"
        )


if __name__ == "__main__":
    main()

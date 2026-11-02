# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Check immutable capacity-list goldens against selected, pinned Python source."""

import argparse
import ast
import builtins
import copy
import hashlib
import html
import json
import re
import sys
from contextlib import contextmanager
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Iterator, Optional
from urllib.parse import urlencode

SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
CASES_SHA256 = "99dfbbddeeb5cff68510a8bd89cd486f056cb0ee6d4457f80801ed216ba88b83"
SOURCE_SHA256 = {
    "src/fabric_cli/client/fab_api_capacity.py": (
        "29ceb2c1cd1d710cf5f43c70a9b01b21b064bb6f95a9d4d4fae81ceafde576b6"
    ),
    "src/fabric_cli/client/fab_api_client.py": (
        "d7d978f315921ec4559a2fcefc992435152758b74470d1d969006def088e8e0d"
    ),
    "src/fabric_cli/client/fab_api_types.py": (
        "990708f777081bb07fdd80906bcd4e49f22f4f1af6e7cd522808a89ff3b34c40"
    ),
    "src/fabric_cli/core/fab_exceptions.py": (
        "85826f456cc9f2bfd026950fc0338b04570cf56c571513f1cef1feac386db0dc"
    ),
    "src/fabric_cli/core/fab_constant.py": (
        "20dd87282310f0217b3f3724214c280c3dab123bec5f7fafa770e1c2c44bfff0"
    ),
    "src/fabric_cli/errors/common.py": (
        "f0af9183ba94b1ab89f6bd7afe1bca71538704afd5570e73b913fd45b2c1154d"
    ),
    "src/fabric_cli/utils/fab_http_polling_utils.py": (
        "a95933f2184b71219d88f62cbde9c478b145c1ed254408aa5bfec6db8b66ecc4"
    ),
    "src/fabric_cli/utils/fab_mem_store.py": (
        "5f1bb375ca2e1da68c454de622b1f624f8187e73c8ef8db317f553bad9f12620"
    ),
}
DIRECTORY = Path(__file__).resolve().parent
ROOT = DIRECTORY.parents[3]
_active_guard: Optional[list[str]] = None


class OfflineViolation(AssertionError):
    """A source execution attempted an operation outside the offline boundary."""


def _deny(operation: str) -> None:
    if _active_guard is not None:
        _active_guard.append(operation)
    raise OfflineViolation(f"Offline source boundary denied: {operation}")


def _audit(event: str, arguments: tuple[Any, ...]) -> None:
    if _active_guard is None:
        return
    if event == "open" or event.startswith(
        ("socket.", "subprocess.", "ctypes.", "winreg.")
    ):
        _deny(event)
    if event in {
        "os.system",
        "os.exec",
        "os.fork",
        "os.forkpty",
        "os.posix_spawn",
        "os.spawn",
        "os.startfile",
        "os.startfile/2",
        "os.mkdir",
        "os.remove",
        "os.rename",
        "os.rmdir",
        "os.link",
        "os.symlink",
        "os.chmod",
        "os.chown",
        "os.truncate",
        "os.utime",
        "os.chdir",
        "os.listdir",
        "os.scandir",
        "shutil.copyfile",
        "shutil.copymode",
        "shutil.copystat",
        "shutil.rmtree",
    }:
        _deny(event)


sys.addaudithook(_audit)


@contextmanager
def source_guard() -> Iterator[list[str]]:
    """Deny filesystem, network and process operations while source executes."""
    global _active_guard
    if _active_guard is not None:
        raise AssertionError("Nested source execution is not supported")
    violations: list[str] = []
    _active_guard = violations
    try:
        yield violations
    finally:
        _active_guard = None


def normalized_sha256(contents: bytes) -> str:
    """Hash UTF-8 checkout bytes, normalizing CRLF to LF and nothing else."""
    return hashlib.sha256(contents.replace(b"\r\n", b"\n")).hexdigest()


def verify_sources(sources: dict[str, bytes]) -> None:
    """Fail closed on any missing, additional or changed pinned source file."""
    if sources.keys() != SOURCE_SHA256.keys():
        raise AssertionError("Pinned Python source file set changed")
    for relative, contents in sources.items():
        if normalized_sha256(contents) != SOURCE_SHA256[relative]:
            raise AssertionError(f"Python source drift requires review: {relative}")


def load_sources() -> dict[str, bytes]:
    """Read and verify source before activating the no-I/O execution guard."""
    sources = {
        relative: ROOT.joinpath(*relative.split("/")).read_bytes()
        for relative in SOURCE_SHA256
    }
    verify_sources(sources)
    return sources


def verify_cases(contents: bytes) -> dict[str, Any]:
    """Verify immutable hand-authored fixture bytes and their wire schema."""
    if normalized_sha256(contents) != CASES_SHA256:
        raise AssertionError("Capacity goldens changed; independent review required")
    document: dict[str, Any] = json.loads(contents)
    if document["schema_version"] != 1 or document["source_commit"] != SOURCE_COMMIT:
        raise AssertionError("Wrong capacity fixture schema or source commit")
    names = set()
    for case in document["cases"]:
        if case["name"] in names or not isinstance(case["rust_parity"], bool):
            raise AssertionError("Invalid or duplicate capacity case")
        names.add(case["name"])
        if not case["rust_parity"] and not case.get("rust_exception"):
            raise AssertionError("Python-only observations need a Rust exception")
        for response in case["responses"]:
            if ("body" in response) == ("raw_body" in response):
                raise AssertionError("A response needs exactly body or raw_body")
            if not isinstance(response["status"], int) or not isinstance(
                response["headers"], dict
            ):
                raise AssertionError("Invalid scripted response")
        expected = case["expected"]
        if ("value" in expected) == ("error_code" in expected):
            raise AssertionError("Expected exactly value or error_code")
        if not {"requests", "scopes", "sleeps"} <= expected.keys():
            raise AssertionError("Missing observable request/clock expectations")
    return document


def load_cases() -> dict[str, Any]:
    """Load pinned fixtures without a record, update or regeneration mode."""
    return verify_cases((DIRECTORY / "cases.json").read_bytes())


class _Headers(dict[str, str]):
    """Explicit case-insensitive HTTP-header double; not a requests import."""

    def __init__(self, values: dict[str, str]) -> None:
        super().__init__((key.lower(), value) for key, value in values.items())

    def __contains__(self, key: object) -> bool:
        return super().__contains__(key.lower() if isinstance(key, str) else key)

    def __getitem__(self, key: str) -> str:
        return super().__getitem__(key.lower())

    def get(self, key: str, default: Any = None) -> Any:
        return super().get(key.lower(), default)


class _RequestException(Exception):
    pass


class _ScriptedSession:
    def __init__(self, case: dict[str, Any]) -> None:
        self.responses = copy.deepcopy(case["responses"])
        self.requests: list[dict[str, str]] = []
        self.headers: dict[str, str] = {}
        self.mounts: dict[str, Any] = {}

    def mount(self, prefix: str, adapter: Any) -> None:
        self.mounts[prefix] = adapter

    def request(self, method: str, url: str, **kwargs: Any) -> SimpleNamespace:
        self.requests.append({"method": method, "url": url})
        if len(self.requests) > len(self.responses):
            raise AssertionError("Source requested an unscripted response")
        if kwargs["headers"]["Authorization"] != "Bearer synthetic-offline-token":
            raise AssertionError("Source used an unexpected credential")
        if kwargs["headers"]["Content-Type"] != "application/json":
            raise AssertionError("Unexpected capacity request content type")
        response = self.responses[len(self.requests) - 1]
        text = (
            response["raw_body"]
            if "raw_body" in response
            else json.dumps(response["body"])
        )
        return SimpleNamespace(
            status_code=response["status"],
            text=text,
            content=text.encode("utf-8"),
            headers=_Headers(response["headers"]),
        )


class _Clock:
    def __init__(self) -> None:
        self.sleeps: list[int | float] = []
        self.now = 0.0

    def time(self) -> float:
        return self.now

    def sleep(self, seconds: int | float) -> None:
        if not isinstance(seconds, (int, float)) or seconds < 0:
            raise AssertionError("Invalid virtual delay")
        self.sleeps.append(seconds)
        self.now += seconds


def _noop(*args: Any, **kwargs: Any) -> None:
    pass


def _compile_selection(
    sources: dict[str, bytes], relative: str, names: set[str]
) -> Any:
    tree = ast.parse(sources[relative].decode("utf-8"), filename=relative)
    selected: list[ast.stmt] = []
    found = set()
    for node in tree.body:
        name = getattr(node, "name", None)
        if isinstance(node, ast.Assign) and len(node.targets) == 1:
            target = node.targets[0]
            name = target.id if isinstance(target, ast.Name) else None
        if name in names:
            selected.append(node)
            found.add(name)
    if found != names:
        raise AssertionError(f"Missing selected source definitions: {names - found}")
    return compile(ast.Module(body=selected, type_ignores=[]), relative, "exec")


def _source_namespace(
    case: dict[str, Any], sources: dict[str, bytes]
) -> tuple[dict[str, Any], _ScriptedSession, _Clock, list[list[str]]]:
    session = _ScriptedSession(case)
    clock = _Clock()
    scopes: list[list[str]] = []

    class FakeAuth:
        def get_access_token(self, scope: list[str]) -> str:
            scopes.append(list(scope))
            return "synthetic-offline-token"

    class FakeContext:
        command = "capacities-reference"
        fabric_skill = None

    allowed_imports = {
        ("fabric_cli.core.fab_auth", ("FabAuth",)): SimpleNamespace(FabAuth=FakeAuth),
        ("fabric_cli.core.fab_context", ("Context",)): SimpleNamespace(
            Context=FakeContext
        ),
    }

    def source_import(
        name: str,
        globals: Any = None,
        locals: Any = None,
        fromlist: Any = (),
        level: int = 0,
    ) -> Any:
        key = (name, tuple(fromlist))
        if level != 0 or key not in allowed_imports:
            _deny(f"import:{name}")
        return allowed_imports[key]

    def denied_open(*args: Any, **kwargs: Any) -> Any:
        _deny("open")

    def default_endpoint(name: str, default: str) -> str:
        if not name.startswith("FAB_API_ENDPOINT_"):
            raise AssertionError("Unexpected endpoint configuration request")
        return default

    source_builtins = dict(vars(builtins))
    source_builtins.update(
        __import__=source_import,
        open=denied_open,
        input=lambda *args: _deny("input"),
        print=lambda *args, **kwargs: _deny("print"),
    )
    logger = SimpleNamespace(
        log_debug=_noop,
        log_debug_http_request=_noop,
        log_debug_http_response=_noop,
        log_debug_http_request_exception=_noop,
    )
    namespace: dict[str, Any] = {
        "__name__": "_offline_pinned_capacity_source",
        "__builtins__": source_builtins,
        "json": json,
        "html": html,
        "re": re,
        "Namespace": argparse.Namespace,
        "Optional": Optional,
        "CaseInsensitiveDict": _Headers,
        "requests": SimpleNamespace(
            Session=lambda: session,
            RequestException=_RequestException,
            compat=SimpleNamespace(urlencode=urlencode),
        ),
        "HTTPAdapter": lambda **kwargs: SimpleNamespace(**kwargs),
        "Retry": lambda **kwargs: SimpleNamespace(**kwargs),
        "_shared_session": None,
        "time": clock,
        "os": SimpleNamespace(environ={}),
        "platform": SimpleNamespace(
            system=lambda: "SyntheticOS",
            release=lambda: "0",
            python_version=lambda: "0.0.0",
        ),
        "fab_logger": logger,
        "utils_ui": SimpleNamespace(print_info=_noop),
        "validate_and_get_env_variable": default_endpoint,
        "__version__": "synthetic-reference",
    }

    def execute(relative: str, names: set[str]) -> None:
        exec(_compile_selection(sources, relative, names), namespace)

    constant_names = {
        "API_ENDPOINT_FABRIC",
        "API_ENDPOINT_ONELAKE",
        "API_ENDPOINT_AZURE",
        "API_ENDPOINT_POWER_BI",
        "API_VERSION_FABRIC",
        "API_USER_AGENT",
        "FAB_VERSION",
        "SCOPE_FABRIC_DEFAULT",
        "SCOPE_ONELAKE_DEFAULT",
        "SCOPE_AZURE_DEFAULT",
        "FAB_HOST_APP_ENV_VAR",
        "FAB_HOST_APP_VERSION_ENV_VAR",
        "ALLOWED_FAB_HOST_APP_VALUES",
        "FABRIC_SKILL_HEADER",
        "FAB_WS_PRIVATE_LINKS_ENABLED",
        "DEBUG",
        "ERROR_UNAUTHORIZED",
        "ERROR_FORBIDDEN",
        "ERROR_NOT_FOUND",
        "ERROR_MAX_RETRIES_EXCEEDED",
        "ERROR_INVALID_JSON",
        "ERROR_INVALID_OPERATION",
        "ERROR_UNEXPECTED_ERROR",
    }
    execute("src/fabric_cli/core/fab_constant.py", constant_names)
    namespace["fab_constant"] = SimpleNamespace(
        **{name: namespace[name] for name in constant_names}
    )

    def get_config(name: str) -> bool:
        if name != namespace["FAB_WS_PRIVATE_LINKS_ENABLED"]:
            raise AssertionError("Unexpected configuration lookup")
        return False

    namespace["fab_state_config"] = SimpleNamespace(get_config=get_config)
    execute(
        "src/fabric_cli/core/fab_exceptions.py",
        {
            "DEFAULT_ERROR_MESSAGE",
            "DEFAULT_ERROR_CODE",
            "FabricCLIError",
            "FabricAPIError",
            "OnelakeAPIError",
            "AzureAPIError",
        },
    )
    execute("src/fabric_cli/errors/common.py", {"CommonErrors"})
    namespace["ErrorMessages"] = SimpleNamespace(Common=namespace["CommonErrors"])
    execute("src/fabric_cli/client/fab_api_types.py", {"ApiResponse"})
    execute(
        "src/fabric_cli/utils/fab_http_polling_utils.py",
        {"DEFAULT_POLLING_INTERVAL", "get_polling_interval"},
    )
    execute(
        "src/fabric_cli/client/fab_api_client.py",
        {
            "_HOST_APP_VERSION_RE",
            "_get_session",
            "do_request",
            "_handle_successful_response",
            "_build_user_agent",
            "_get_host_app",
            "_transform_workspace_url_for_private_link_if_needed",
        },
    )
    namespace["fabric_api"] = SimpleNamespace(do_request=namespace["do_request"])
    execute("src/fabric_cli/client/fab_api_capacity.py", {"list_capacities"})
    return namespace, session, clock, scopes


def observe(case: dict[str, Any]) -> dict[str, Any]:
    """Execute real selected helpers with fresh fake auth, transport and time."""
    sources = load_sources()
    with source_guard() as violations:
        namespace, session, clock, scopes = _source_namespace(case, sources)
        observation: dict[str, Any] = {}
        try:
            response = namespace["list_capacities"](argparse.Namespace())
            body = response.json()
            observation["value"] = (
                body.get("value") if isinstance(body, dict) else body
            )
            observation["response_json"] = body
            if isinstance(body, dict) and "total_pages" in body:
                observation["total_pages"] = body["total_pages"]
        except namespace["FabricCLIError"] as error:
            observation.update(
                error_code=error.status_code,
                error_type=type(error).__name__,
                error_message=error.message,
            )
        except json.JSONDecodeError as error:
            observation.update(
                error_code=type(error).__name__, error_type=type(error).__name__
            )
        if len(session.requests) != len(session.responses):
            raise AssertionError(f"{case['name']}: unused scripted responses")
        if violations:
            raise AssertionError(f"Source caught offline violations: {violations}")
        observation.update(
            requests=session.requests, scopes=scopes, sleeps=clock.sleeps
        )
    return observation


def assert_matches(case: dict[str, Any], observation: dict[str, Any]) -> None:
    """Compare every expected field, list order, missing key and JSON type."""
    expected = json.dumps(
        case["expected"], sort_keys=True, ensure_ascii=False, separators=(",", ":")
    )
    actual = json.dumps(
        observation, sort_keys=True, ensure_ascii=False, separators=(",", ":")
    )
    if actual != expected:
        raise AssertionError(
            f"{case['name']}: source observation differs\n"
            f"expected: {expected}\nactual:   {actual}"
        )


def check_reference() -> None:
    """Check all immutable cases; never write observations back to disk."""
    document = load_cases()
    load_sources()
    for case in document["cases"]:
        assert_matches(case, observe(case))
        print(f"PASS {case['name']}")


def main() -> None:
    """Run only the explicit read-only reference check."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", required=True)
    parser.parse_args()
    check_reference()


if __name__ == "__main__":
    main()

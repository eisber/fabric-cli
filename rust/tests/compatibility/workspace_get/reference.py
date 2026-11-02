# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independent, frozen, offline workspace GET source oracle (no update mode)."""

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
from enum import Enum, auto
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from urllib.parse import urlencode

sys.dont_write_bytecode = True

import jmespath

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
INTEGRATION_COMMIT = "fad3ea7fee3a3e4e66205ffcf34d6b943bb416d7"
SOURCE_TREE = "e8bcb0d47d7b705d665c3202da2770eafe65e22a"
TIMESTAMP = "2000-01-01T00:00:00Z"
SCOPE = ["https://api.fabric.microsoft.com/.default"]
COPYRIGHT = "Copyright (c) Microsoft Corporation. Licensed under the MIT License."
# Final manually authored LF manifest, frozen before any source/native run.
MANIFEST_BLOB = "f63fd4307f8712504c23c38e381c6ea994696e9e"
STATUS_MANIFEST_BLOB = "fb605e652be16b395c59d51ff9f22b6726744276"
TRUTHINESS_MANIFEST_BLOB = "3cf5447ad47fe7afd23080be37961e0de16d09b9"
BLOBS = {
    "client/fab_api_client.py": "0e896e27ff9fe0e6446448d721679db422f283dd",
    "client/fab_api_types.py": "09b81e62b57965b3e39075f2ce55d80b3bc849d3",
    "client/fab_api_workspace.py": "386ae6aac06cc9c2505cdd13859e62b0b8aea05c",
    "commands/fs/fab_fs.py": "8de0d21acab9abf1d2d012008bfb2d1fc0a9899b",
    "commands/fs/fab_fs_get.py": "6487458e9e875bb21a7dc8aec342e59624756178",
    "commands/fs/get/fab_fs_get_workspace.py": "e412f271341fc42a2166299771fa0688a95388b1",
    "core/fab_constant.py": "d333baa7182245546b9ef1b6b572f77f210eab14",
    "core/fab_commands.py": "65c40ef3ef0ea8af4f1a13f745cb53196eb505b1",
    "core/fab_decorators.py": "6a466e8fb421c5cf5753b84c34088bf218fb6b0b",
    "core/fab_exceptions.py": "b3d6cd5e4eed8240271d1c423b69e4cd806b2d66",
    "core/fab_output.py": "4a6ea38ad912a2cce486568badf6b013329d7cb0",
    "core/fab_types.py": "ceba9685ea693078f64faf8303c8a62c198f9c1b",
    "core/hiearchy/fab_element.py": "b3f82aacb100bd76c150ff42595283793db6bb0b",
    "core/hiearchy/fab_tenant.py": "044d2c55592860a25fff9e9744e133aa7d034353",
    "core/hiearchy/fab_workspace.py": "1ed8902de53f874cb18705038cf68ac8551bd6ce",
    "errors/common.py": "6fcfc00098d71517d8bb345015b48d7527544640",
    "main.py": "9aa5d6b3e7b878ffa748b9c2c93d277343f97dec",
    "utils/fab_cmd_get_utils.py": "4d69ad98696e1e788c56ec52d9355d0308e53830",
    "utils/fab_jmespath.py": "9ba5ee2b2c59bb945606bd54f01554e399751d89",
    "utils/fab_ui.py": "1ff741b5838ca0c7d8c003fd60233804bd6df15f",
    "utils/fab_util.py": "ce2c976d31b8d56996c2f0a7db9ab9abe7fcd145",
}
SUPPORT_BLOBS = {
    "commands": "dc36a0e7fdc193f2c60385fbe75f6187dd42ba99",
    "items": "6859da774a4f5c01375cf2570c3170d4cfb0dc8e",
    "output": "52c487552c16f4b6e5d2af25dcca1b4087696599",
    "workspaces": "2e71a4ebbcbc245010efc1333a67f6b63df73acb",
}


def blob_identity(raw: bytes) -> str:
    """Compute the Git blob identity with only checkout CRLF normalization."""
    raw = raw.replace(b"\r\n", b"\n")
    return hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()


def _support() -> Any:
    for area, expected in SUPPORT_BLOBS.items():
        path = HERE.parent / area / "reference.py"
        if blob_identity(path.read_bytes()) != expected:
            raise AssertionError(f"Protected read-only reference changed: {area}")
    spec = importlib.util.spec_from_file_location(
        "_workspace_get_commands", HERE.parent / "commands" / "reference.py"
    )
    if spec is None or spec.loader is None:
        raise AssertionError("Missing read-only command oracle")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


COMMANDS = _support()
OUTPUT = COMMANDS.OUTPUT
OfflineViolation = COMMANDS.OfflineViolation


def _git(*args: str) -> str:
    result = subprocess.run(
        ["git", "--no-pager", "-C", str(ROOT), *args],
        capture_output=True,
        check=True,
        timeout=30,
    )
    return result.stdout.decode("utf-8").strip()


def verify_source() -> None:
    """Enforce original complete-tree, executed-file, and Git object provenance."""
    COMMANDS.verify_source()
    for commit in (SOURCE_COMMIT, INTEGRATION_COMMIT):
        if _git("rev-parse", f"{commit}:src/fabric_cli") != SOURCE_TREE:
            raise AssertionError("Pinned Python Git tree changed")
    if _git("rev-parse", "HEAD:src/fabric_cli") != SOURCE_TREE:
        raise AssertionError("Current committed Python tree changed")
    for path, digest in BLOBS.items():
        if _git("rev-parse", f"{SOURCE_COMMIT}:src/fabric_cli/{path}") != digest:
            raise AssertionError(f"Pinned executed Git blob changed: {path}")
        raw = ROOT.joinpath("src", "fabric_cli", *path.split("/")).read_bytes()
        if blob_identity(raw) != digest:
            raise AssertionError(f"Executed working-tree source changed: {path}")


def load_sources() -> dict[str, ast.Module]:
    """Parse complete original source files after all provenance checks."""
    verify_source()
    return {
        path: ast.parse(
            ROOT.joinpath("src", "fabric_cli", *path.split("/"))
            .read_bytes()
            .replace(b"\r\n", b"\n"),
            filename=path,
        )
        for path in BLOBS
    }


def _validate_manifest(
    manifest: dict[str, Any], raw: bytes, expected_blob: str
) -> None:
    if blob_identity(raw) != expected_blob or json.loads(raw) != manifest:
        raise AssertionError("Frozen workspace GET manifest changed")
    if (
        manifest["_copyright"] != COPYRIGHT
        or manifest["schema_version"] != 1
        or manifest["source_commit"] != SOURCE_COMMIT
        or manifest["integration_commit"] != INTEGRATION_COMMIT
    ):
        raise AssertionError("Workspace GET provenance changed")
    cases = manifest["cases"]
    ids = [case["id"] for case in cases]
    if len(ids) != len(set(ids)) or len(ids) != manifest["case_count"]:
        raise AssertionError("Workspace GET case inventory changed")
    for case in cases:
        category = case["category"]
        if category not in {"exact", "native-safety", "unsupported"}:
            raise AssertionError("Unknown comparison category")
        if not case.get("purpose") or not case.get("expected"):
            raise AssertionError("Missing independently authored source expectation")
        if category != "exact" and (
            not case.get("boundary") or not case.get("native_expected")
        ):
            raise AssertionError("Native differences require exact separate evidence")


def validate_manifest(manifest: dict[str, Any], raw: bytes) -> None:
    """Reject any rewritten original expectation, input, category or provenance."""
    _validate_manifest(manifest, raw, MANIFEST_BLOB)


def validate_status_manifest(manifest: dict[str, Any], raw: bytes) -> None:
    """Protect additive status evidence without modifying the original fixture."""
    _validate_manifest(manifest, raw, STATUS_MANIFEST_BLOB)
    if len(manifest["cases"]) != 6:
        raise AssertionError("Additive status inventory changed")
    for case in manifest["cases"]:
        if "native_input" in case and (
            case["id"] != "status_optional_201_then_200"
            or case["category"] != "native-safety"
            or set(case["native_input"]) != {"responses"}
        ):
            raise AssertionError("Undeclared native response-script difference")


def load_cases() -> dict[str, Any]:
    """Load hand-authored expectations; never record or regenerate observations."""
    raw = (HERE / "cases.json").read_bytes()
    manifest = json.loads(raw)
    validate_manifest(manifest, raw)
    return manifest


def load_status_cases() -> dict[str, Any]:
    """Read independently frozen additive status contracts, never rewrite originals."""
    raw = (HERE / "status-cases.json").read_bytes()
    manifest = json.loads(raw)
    validate_status_manifest(manifest, raw)
    return manifest


def load_truthiness_cases() -> dict[str, Any]:
    """Load frozen complete-dispatch truthiness evidence without old fixture edits."""
    raw = (HERE / "truthiness-cases.json").read_bytes()
    manifest = json.loads(raw)
    _validate_manifest(manifest, raw, TRUTHINESS_MANIFEST_BLOB)
    if len(manifest["cases"]) != 3 or any(
        case.get("source_dispatch") is not True for case in manifest["cases"]
    ):
        raise AssertionError("Complete-dispatch truthiness inventory changed")
    return manifest


def inputs_for(
    manifest: dict[str, Any], case: dict[str, Any], *, native: bool = False
) -> dict[str, Any]:
    """Adapt synthetic data to the existing read_probe JSONL input protocol."""
    inputs = copy.deepcopy(
        case["native_input"]
        if native and "native_input" in case
        else case.get("input", {})
    )
    inputs["id"] = case["id"]
    inputs.setdefault("current_workspace", copy.deepcopy(manifest["workspace"]))
    command = ["get", ".", "--output_format", "json"]
    query = case.get("query", ".")
    if query is not None:
        command.extend(["--query", query])
    inputs.setdefault("command", command)
    if "responses" not in inputs:
        inputs["responses"] = copy.deepcopy(manifest["scripts"][case["script"]])
    return inputs


def expected_observation(
    manifest: dict[str, Any], case: dict[str, Any], *, native: bool = False
) -> dict[str, Any]:
    """Encode only handwritten envelopes/routes, never output from either engine."""
    expected = (
        case["native_expected"]
        if native and case["category"] != "exact"
        else case["expected"]
    )
    routes = expected.get("requests")
    if routes is None:
        routes = manifest["requests"][expected.get("request_set", "all")]
    output = expected.get("output")
    return {
        "id": case["id"],
        "exit_code": expected.get("exit_code", 0),
        "stdout": (
            ""
            if output is None
            else json.dumps({"timestamp": TIMESTAMP, **output}, indent=4) + "\n"
        ),
        "stderr": expected.get("stderr", ""),
        "requests": routes,
        "scopes": [list(SCOPE) for _ in routes],
    }


def _arguments(inputs: dict[str, Any]) -> argparse.Namespace:
    argv = inputs["command"]
    if argv[:4] != ["get", ".", "--output_format", "json"]:
        raise ValueError("Oracle supports injected current-workspace JSON GET only")
    if len(argv) == 4:
        query = None
    elif len(argv) == 6 and argv[4] == "--query":
        query = [argv[5]]
    else:
        raise ValueError("Only one processed query expression is supported")
    return argparse.Namespace(
        command="get",
        command_path="get",
        output_format="json",
        path=".",
        query=query,
        output=None,
        deep_traversal=False,
    )


def observe(
    inputs: dict[str, Any],
    sources: dict[str, ast.Module],
    returned: list[Any] | None = None,
    *,
    dispatch: bool = False,
) -> dict[str, Any]:
    """Run actual workspace exec/API/HTTP/paging/query/output/error definitions."""
    args = _arguments(inputs)
    output = OUTPUT.Reference(TIMESTAMP)
    env = output.namespace
    requests_seen: list[str] = []
    scopes: list[list[str]] = []
    violations: list[str] = []
    response_index = 0

    def deny(*unused: Any, **kwargs: Any) -> Any:
        violations.append("Unapproved source collaborator")
        raise OfflineViolation("Unapproved source collaborator")

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
                "API_VERSION_FABRIC", "FAB_WS_PRIVATE_LINKS_ENABLED", "DEBUG",
                "FAB_CONTEXT_PERSISTENCE_ENABLED",
                "FAB_MODE_COMMANDLINE", "FAB_MODE_INTERACTIVE",
            }
        )
    }
    constants: dict[str, Any] = {
        "validate_and_get_env_variable": lambda name, default: default
    }
    exec(
        COMMANDS.ITEMS._compile_symbols(
            sources, "core/fab_constant.py", constant_names
        ),
        constants,
    )
    env["fab_constant"].__dict__.update(
        {name: constants[name] for name in constant_names}
    )

    def get_config(name: str) -> str:
        if name not in {
            env["fab_constant"].FAB_WS_PRIVATE_LINKS_ENABLED,
            env["fab_constant"].FAB_CONTEXT_PERSISTENCE_ENABLED,
        }:
            return deny()
        return "false"

    class RequestException(Exception):
        pass

    class TestAuth:
        def get_access_token(self, scope: list[str]) -> str:
            if scope != SCOPE:
                raise AssertionError("Unexpected credential audience")
            scopes.append(list(scope))
            return "offline-workspace-get-only"

    class Session:
        def request(self, method: str, url: str, **kwargs: Any) -> Any:
            nonlocal response_index
            if method != "get" or kwargs != {
                "headers": {
                    "Authorization": "Bearer offline-workspace-get-only",
                    "User-Agent": "offline-reference",
                    "Content-Type": "application/json",
                },
                "timeout": 240,
            }:
                raise AssertionError("Actual HTTP boundary changed")
            if len(scopes) != len(requests_seen) + 1:
                raise AssertionError("Every request must acquire exactly one scope")
            if response_index >= len(inputs["responses"]):
                return deny()
            fixture = inputs["responses"][response_index]
            response_index += 1
            requests_seen.append(url)
            body = fixture["body"]
            text = body if isinstance(body, str) else json.dumps(body)
            return SimpleNamespace(
                status_code=fixture["status"],
                headers=COMMANDS.ITEMS._Headers(fixture.get("headers", {})),
                text=text,
                content=text.encode("utf-8"),
            )

    runtime_mode = [env["fab_constant"].FAB_MODE_COMMANDLINE]
    context = SimpleNamespace(
        command="get", fabric_skill=None, get_runtime_mode=lambda: runtime_mode[0]
    )
    modules = {
        "fabric_cli.core.fab_auth": SimpleNamespace(FabAuth=TestAuth),
        "fabric_cli.core.fab_context": SimpleNamespace(
            Context=lambda: context
        ),
        "fabric_cli.utils.fab_util": SimpleNamespace(dumps=env["dumps"]),
    }

    def injected_import(name: str, *unused: Any, **kwargs: Any) -> Any:
        if name not in modules:
            return deny()
        return modules[name]

    env["__builtins__"]["__import__"] = injected_import
    env.update(
        {
            "ABC": ABC,
            "abstractmethod": abstractmethod,
            "Enum": Enum,
            "auto": auto,
            "re": re,
            "Namespace": argparse.Namespace,
            "fab_state_config": SimpleNamespace(get_config=get_config),
            "fab_logger": SimpleNamespace(
                log_debug_http_request=lambda *unused: None,
                log_debug_http_response=lambda *unused: None,
                log_debug_http_request_exception=lambda *unused: None,
            ),
            "requests": SimpleNamespace(
                compat=SimpleNamespace(urlencode=urlencode),
                RequestException=RequestException,
            ),
            "time": SimpleNamespace(time=lambda: 0, sleep=deny),
            "_build_user_agent": lambda command: "offline-reference",
            "_get_session": lambda: Session(),
            "_handle_fab_long_running_op": deny,
            "_handle_azure_async_op": deny,
            "get_polling_interval": deny,
            "files_utils": SimpleNamespace(load_json_from_path=deny),
            "utils_storage": SimpleNamespace(do_output=deny),
            "jmespath": jmespath,
        }
    )
    selections = [
        ("errors/common.py", {"CommonErrors"}),
        ("client/fab_api_types.py", {"ApiResponse"}),
        ("core/fab_types.py", {"FabricElementType", "WorkspaceType"}),
        ("core/hiearchy/fab_element.py", {"FabricElement"}),
        ("core/hiearchy/fab_tenant.py", {"Tenant"}),
        ("core/hiearchy/fab_workspace.py", {"_BaseWorkspace", "Workspace"}),
        (
            "client/fab_api_client.py",
            {
                "do_request",
                "_handle_successful_response",
                "_transform_workspace_url_for_private_link_if_needed",
            },
        ),
        (
            "client/fab_api_workspace.py",
            {
                "get_workspace",
                "ls_workspace_managed_private_endpoints",
                "get_workspace_spark_settings",
                "acl_list_from_workspace",
            },
        ),
        ("utils/fab_jmespath.py", {"search", "_get_json_paths"}),
        ("utils/fab_cmd_get_utils.py", {"query_and_export"}),
        ("utils/fab_util.py", {"process_nargs"}),
    ]
    for path, names in selections:
        definitions(path, names)
    env["ErrorMessages"] = SimpleNamespace(Common=env["CommonErrors"])
    env["fabric_api"] = SimpleNamespace(do_request=env["do_request"])
    env["workspace_api"] = SimpleNamespace(
        **{
            name: env[name]
            for name in (
                "get_workspace",
                "ls_workspace_managed_private_endpoints",
                "get_workspace_spark_settings",
                "acl_list_from_workspace",
            )
        }
    )
    env["utils_ui"] = SimpleNamespace(
        print_output_format=env["print_output_format"], print_info=deny
    )
    env["utils_jmespath"] = SimpleNamespace(search=env["search"])
    env["utils_get"] = SimpleNamespace(query_and_export=env["query_and_export"])
    definitions("commands/fs/get/fab_fs_get_workspace.py", {"exec"})
    command_exec = env["exec"]
    ws = inputs["current_workspace"]
    tenant = env["Tenant"]("Offline tenant", "offline-tenant")
    workspace = env["Workspace"](ws["displayName"], ws["id"], tenant, ws["type"])
    before = workspace.__dict__.copy()

    @env["handle_exceptions"]()
    def invoke(arguments: argparse.Namespace) -> None:
        value = command_exec(workspace, arguments)
        if returned is not None:
            returned.append(value)

    class DispatchExit(BaseException):
        def __init__(self, code: int) -> None:
            super().__init__(code)
            self.code = code

    if dispatch:
        def terminate(code: int) -> None:
            raise DispatchExit(code)

        def set_mode(mode: str) -> None:
            if mode != env["fab_constant"].FAB_MODE_COMMANDLINE:
                return deny()
            runtime_mode[0] = mode

        def resolve_context(path: str) -> Any:
            if path != ".":
                return deny()
            return workspace

        def supported_elements(command: Any) -> list[Any]:
            if command != env["Command"].FS_GET:
                return deny()
            return [env["FabricElementType"].WORKSPACE]

        def workspace_handler(element: Any, arguments: argparse.Namespace) -> Any:
            value = command_exec(element, arguments)
            if returned is not None:
                returned.append(value)
            return value

        parser = SimpleNamespace(
            parse_args=lambda: args,
            get_mode=lambda: runtime_mode[0],
            set_mode=set_mode,
            error=deny,
        )
        subparsers = SimpleNamespace(choices={"get": SimpleNamespace()})
        for name in (
            "FAB_CONTEXT_PERSISTENCE_ENABLED",
            "FAB_MODE_COMMANDLINE",
            "FAB_MODE_INTERACTIVE",
        ):
            env[name] = constants[name]
        env["sys"].exit = terminate
        env["fab_state_config"].init_defaults = lambda: None
        env["fab_logger"].print_log_file_path = lambda: None
        env["platform"] = SimpleNamespace(system=lambda: "Windows")
        env["argcomplete"] = SimpleNamespace(autocomplete=lambda *a, **k: None)
        env["get_global_parser_and_subparsers"] = lambda: (parser, subparsers)
        env["cmd"] = SimpleNamespace(get_supported_elements=supported_elements)
        env["handle_context"] = SimpleNamespace(get_command_context=resolve_context)
        env["fab_util"] = SimpleNamespace(process_nargs=env["process_nargs"])
        env["get_workspace"] = SimpleNamespace(exec=workspace_handler)
        definitions("core/fab_commands.py", {"Command"})
        definitions("core/fab_decorators.py", {"set_command_context"})
        definitions("commands/fs/fab_fs_get.py", {"exec_command"})
        env["fs_get"] = SimpleNamespace(exec_command=env["exec_command"])
        definitions("commands/fs/fab_fs.py", {"get_command"})
        args.func = env["get_command"]
        definitions("main.py", {"main", "_execute_command"})

    with COMMANDS.offline_boundary():
        try:
            if dispatch:
                try:
                    env["main"]()
                except DispatchExit as caught:
                    exit_code = caught.code
                else:
                    raise AssertionError("Single-command main did not exit")
            else:
                args.query = env["process_nargs"](args.query)
                exit_code = invoke(args) or 0
        except OfflineViolation:
            raise
        except Exception as error:
            try:
                env["_handle_unexpected_error"](error, args)
            except OUTPUT.ObservedExit as caught:
                exit_code = caught.code
    if violations:
        raise OfflineViolation("Forbidden collaborator attempted, even if caught")
    if workspace.__dict__ != before or args.ws_id != ws["id"]:
        raise AssertionError("GET context changed or workspace ID was not injected")
    if response_index != len(inputs["responses"]):
        raise AssertionError(f"{inputs['id']}: unused scripted replies")
    return {
        "id": inputs["id"],
        "exit_code": exit_code,
        "stdout": output.stdout.getvalue(),
        "stderr": output.stderr.getvalue(),
        "requests": requests_seen,
        "scopes": scopes,
    }


def assert_observation(
    manifest: dict[str, Any],
    case: dict[str, Any],
    actual: dict[str, Any],
    *,
    native: bool = False,
) -> None:
    """Compare complete protocol including exact stream bytes and ordered scopes."""
    expected = expected_observation(manifest, case, native=native)
    if actual != expected:
        fields = [key for key in expected if actual.get(key) != expected[key]]
        raise AssertionError(
            f"{case['id']}: {'native' if native else 'source'} mismatch in {fields}\n"
            f"Expected: {expected!r}\nObserved: {actual!r}"
        )


def main() -> None:
    """Validate all frozen source outcomes and optionally the existing native probe."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-probe", type=Path)
    parser.add_argument("--case")
    parser.add_argument(
        "--suite", choices=("all", "original", "status", "truthiness"), default="all"
    )
    options = parser.parse_args()
    manifests = []
    if options.suite in {"all", "original"}:
        manifests.append(load_cases())
    if options.suite in {"all", "status"}:
        manifests.append(load_status_cases())
    if options.suite in {"all", "truthiness"}:
        manifests.append(load_truthiness_cases())
    sources = load_sources()
    records = [
        (manifest, case)
        for manifest in manifests
        for case in manifest["cases"]
        if options.case in (None, case["id"])
    ]
    if not records:
        parser.error("unknown frozen case")
    source = []
    for manifest, case in records:
        returned: list[Any] = []
        actual = observe(
            inputs_for(manifest, case), sources, returned,
            dispatch=case.get("source_dispatch", False),
        )
        assert_observation(manifest, case, actual)
        if "returned" in case["expected"] and returned != [
            case["expected"]["returned"]
        ]:
            raise AssertionError(f"{case['id']}: actual command return value changed")
        source.append(actual)
    if options.rust_probe:
        native = COMMANDS.run_rust(
            options.rust_probe,
            [
                inputs_for(manifest, case, native=True)
                for manifest, case in records
            ],
        )
        counts = {"exact": 0, "native-safety": 0, "unsupported": 0}
        for (manifest, case), original, actual in zip(records, source, native):
            assert_observation(manifest, case, actual, native=True)
            if case["category"] == "exact" and original != actual:
                raise AssertionError(
                    "Exact cases require complete source/native equality"
                )
            if case["category"] != "exact" and original == actual:
                raise AssertionError("A native boundary must not be reported as parity")
            counts[case["category"]] += 1
        print(f"Native actual assertions (not blanket parity): {counts}")
    else:
        print("Native outcomes NOT RUN; no native parity or boundary claim.")
    print(
        f"Checked {len(records)} frozen workspace GET source cases at "
        f"{SOURCE_COMMIT}; selected suite={options.suite}, "
        f"case={options.case or 'all'}. Selection is not full-slice parity."
    )


if __name__ == "__main__":
    main()

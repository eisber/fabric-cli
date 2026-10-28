# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture pinned Python output offline, without generating native expectations.

Run from the repository root with Python 3.10+ and -B. Only the existing output
oracle and complete blob-pinned command definitions are executed. Fake service
responses, settings and logout effects are private; no product module, identity,
environment, cache, network, terminal or clock is accessed.
"""

import argparse
import ast
import hashlib
import json
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any

ROOT = Path(__file__).resolve().parents[3]
ORACLE = ROOT / "rust" / "tests" / "compatibility" / "output"
sys.path.insert(0, str(ORACLE))
from reference import Reference, SOURCE_COMMIT, fixtures  # noqa: E402
from text_reference import TextReference  # noqa: E402

sys.path.pop(0)

COMMAND_BLOBS = {
    "commands/config/fab_config_get.py": "dd25e31c88f3a83b4232712dea37daa52e6b1b18",
    "commands/jobs/fab_jobs_run_list.py": "d5e729cdd31f329e9957c4fe67f332112d5bb411",
    "commands/tables/fab_tables_schema.py": "74f3b39ddfba1616bc041bf0d84dc6109332a6d5",
    "commands/acls/fab_acls_ls.py": "0516c72f069b01b6ee44ebd581f215b748572b2d",
    "commands/auth/fab_auth.py": "e40908b0bd333995c465da58815d369f4c5af8f1",
    "utils/fab_cmd_ls_utils.py": "f843a06e03ecf6cf1892f79793d0b3132e5bdd22",
    "utils/fab_cmd_get_utils.py": "4d69ad98696e1e788c56ec52d9355d0308e53830",
}


def load(relative: str, symbol: str, namespace: dict[str, Any]) -> Any:
    """Execute a complete definition only after checking its immutable source blob."""
    raw = (ROOT / "src" / "fabric_cli" / relative).read_bytes()
    digest = hashlib.sha1(
        b"blob " + str(len(raw)).encode("ascii") + b"\0" + raw
    ).hexdigest()
    if digest != COMMAND_BLOBS[relative]:
        raise AssertionError(f"{relative}: source differs from {SOURCE_COMMIT}")
    nodes = [
        node
        for node in ast.parse(raw).body
        if isinstance(node, ast.FunctionDef) and node.name == symbol
    ]
    if len(nodes) != 1:
        raise AssertionError(f"{relative}: missing {symbol}")
    exec(compile(ast.Module(body=nodes, type_ignores=[]), relative, "exec"), namespace)
    return namespace[symbol]


def observe(case: dict[str, Any]) -> dict[str, Any]:
    """Capture source streams and real decorator exit at the stated boundary."""
    if case["boundary"] == "json-output":
        result = Reference(case.get("timestamp", fixtures()["timestamp"])).observe(
            {"name": case["id"], "kind": case.get("kind", "success"), "input": case["input"]}
        )
        return {**result, "boundary": case["boundary"]}
    reference = TextReference(
        {"name": case["id"], "input": {}, "config": case.get("format", "text")}
    )
    ns = reference.reference.namespace
    values = case["input"]
    command = values["command"]
    args = argparse.Namespace(
        command=command,
        command_path=values.get("command_path", command),
        output_format=case.get("format", "text"),
        **{
            key: value
            for key, value in values.items()
            if key not in {"command", "command_path"}
        },
    )
    ui = SimpleNamespace(
        print_output_format=ns["print_output_format"],
        print_grey=ns["print_grey"],
        print_warning=ns["print_warning"],
    )
    namespace = {
        "Namespace": argparse.Namespace,
        "Item": object,
        "Workspace": object,
        "json": json,
        "FabricCLIError": ns["FabricCLIError"],
        "fab_constant": SimpleNamespace(
            FAB_MODE="mode",
            FAB_CONFIG_KEYS_TO_VALID_VALUES={"output_format": ["text", "json"]},
            ERROR_INVALID_DETLA_TABLE="InvalidDeltaTable",
        ),
        "fab_ui": ui,
        "utils_ui": ui,
    }
    response = SimpleNamespace(
        status_code=200, text=json.dumps(values.get("response", {"value": []}))
    )
    boundary = case["boundary"]
    effects: list[Any] = []
    if boundary in {"job-command", "acl-command", "ls-output-utility"}:
        formatter_namespace = {"utils_ui": ui}
        formatter = load(
            "utils/fab_cmd_ls_utils.py",
            "format_and_print_output",
            formatter_namespace,
        )
        namespace["utils_ls"] = SimpleNamespace(format_and_print_output=formatter)
    if boundary == "config-command":
        namespace["fab_state_config"] = SimpleNamespace(
            get_config=lambda key: values.get("setting")
        )
        execute = load("commands/config/fab_config_get.py", "exec_command", namespace)
        invocation = lambda: execute(args)
    elif boundary == "job-command":
        namespace["jobs_api"] = SimpleNamespace(
            list_item_runs=lambda args: response,
            list_item_schedules=lambda args: response,
        )
        execute = load("commands/jobs/fab_jobs_run_list.py", "exec_command", namespace)
        invocation = lambda: execute(args, object())
    elif boundary == "acl-command":
        namespace["api_workspaces"] = SimpleNamespace(
            acl_list_from_workspace=lambda args: response
        )
        execute = load("commands/acls/fab_acls_ls.py", "_ls_acls_workspace", namespace)
        invocation = lambda: execute(SimpleNamespace(id="offline-workspace"), args)
    elif boundary == "table-command":
        namespace["_extract_schema_from_commit_logs"] = lambda args: values.get("schema")
        execute = load("commands/tables/fab_tables_schema.py", "exec_command", namespace)
        invocation = lambda: execute(args)
    elif boundary == "auth-logout-command":
        namespace["FabAuth"] = lambda: SimpleNamespace(
            logout_session=lambda: effects.append("synthetic application logout")
        )
        execute = load("commands/auth/fab_auth.py", "logout", namespace)
        invocation = lambda: execute(args)
    elif boundary == "ls-output-utility":
        invocation = lambda: formatter(
            values["data"],
            args,
            values.get("show_details", False),
            values.get("columns", ["name"]),
            values.get("hidden_data"),
        )
    elif boundary == "get-output-utility":
        namespace["Any"] = Any
        namespace["utils_jmespath"] = SimpleNamespace(
            search=lambda data, query, deep: values["projection"]
        )
        namespace["utils_storage"] = SimpleNamespace(
            do_output=lambda **kwargs: effects.append(
                {
                    "synthetic_file_output": {
                        "data": kwargs["data"],
                        "file_name": kwargs["file_name"],
                        "output": kwargs["args"].output,
                    }
                }
            )
        )
        execute = load("utils/fab_cmd_get_utils.py", "query_and_export", namespace)
        invocation = lambda: execute({}, args, "offline.json")
    elif boundary == "decorated-error":
        error = ns["FabricCLIError"](values["message"], values["error_code"])

        def invocation() -> None:
            raise error

    elif boundary == "text-output":
        invocation = lambda: ns["print_output_format"](
            args,
            data=values.get("data"),
            message=values.get("message"),
            show_key_value_list=values.get("key_value", False),
        )
    else:
        raise AssertionError(f"unknown boundary: {boundary}")

    @ns["handle_exceptions"]()
    def command_boundary(args: Any) -> None:
        invocation()

    exit_code = command_boundary(args)
    result = {
        "name": case["id"],
        "boundary": boundary,
        "stdout": reference.stdout.getvalue(),
        "stderr": reference.stderr.getvalue(),
        "exit": 0 if exit_code is None else exit_code,
        "effects": effects,
    }
    return result


def main() -> None:
    """Print observations; do not write expectations or invoke Rust."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    cases = json.loads(Path(__file__).with_name("output-cases.json").read_bytes())
    observations = [observe(case) for case in cases]
    capture = {
        "python_pin": SOURCE_COMMIT,
        "timestamp": fixtures()["timestamp"],
        "source_blobs": COMMAND_BLOBS,
        "observations": observations,
    }
    if args.check:
        frozen = json.loads(
            Path(__file__).with_name("python-output-captures.json").read_bytes()
        )
        if capture != frozen:
            raise AssertionError("source observations differ from frozen Python capture")
        print(f"PASS {len(observations)} offline source captures; no Rust run")
    else:
        print(json.dumps(capture, indent=2, ensure_ascii=True))


if __name__ == "__main__":
    main()

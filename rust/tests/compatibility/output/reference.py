# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute pinned real Python definitions with isolated, headless collaborators.

Only source/fixture/observation reads and caller-requested console output occur.
No product imports, sys.modules patching, environment changes, network, auth,
subprocess, file writes, sleeps, or ambient timestamps are used.
"""

import argparse
import ast
import builtins
import hashlib
import html
import io
import json
from datetime import datetime, timezone
from enum import Enum
from functools import wraps
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Dict, List, Optional

DIRECTORY = Path(__file__).resolve().parent
ROOT = DIRECTORY.parents[3]
SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
BLOBS = {
    "core/fab_output.py": "4a6ea38ad912a2cce486568badf6b013329d7cb0",
    "core/fab_exceptions.py": "b3d6cd5e4eed8240271d1c423b69e4cd806b2d66",
    "core/fab_constant.py": "d333baa7182245546b9ef1b6b572f77f210eab14",
    "core/fab_decorators.py": "6a466e8fb421c5cf5753b84c34088bf218fb6b0b",
    "utils/fab_ui.py": "1ff741b5838ca0c7d8c003fd60233804bd6df15f",
    "utils/fab_util.py": "ce2c976d31b8d56996c2f0a7db9ab9abe7fcd145",
    "main.py": "9aa5d6b3e7b878ffa748b9c2c93d277343f97dec",
}


def source_tree(relative: str) -> ast.Module:
    """Reject any source drift, including line-ending changes, before execution."""
    source = ROOT / "src" / "fabric_cli" / relative
    raw = source.read_bytes()
    digest = hashlib.sha1(
        b"blob " + str(len(raw)).encode("ascii") + b"\0" + raw
    ).hexdigest()
    if digest != BLOBS[relative]:
        raise AssertionError(f"{relative}: source differs from {SOURCE_COMMIT}")
    return ast.parse(raw, filename=str(source))


def definitions(relative: str, names: set[str], namespace: dict[str, Any]) -> None:
    """Execute complete named source definitions, not translated source algorithms."""
    selected = [
        node
        for node in source_tree(relative).body
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)) and node.name in names
    ]
    if {node.name for node in selected} != names:
        raise AssertionError(f"{relative}: missing definitions {names}")
    body: list[ast.stmt] = [*selected]
    exec(
        compile(ast.Module(body=body, type_ignores=[]), relative, "exec"),
        namespace,
    )


class ObservedExit(Exception):
    """Isolated substitute for sys.exit, without terminating the oracle process."""

    def __init__(self, code: int) -> None:
        super().__init__(code)
        self.code = code


class Reference:
    """One source-execution namespace with private streams, config, and frozen time."""

    def __init__(self, timestamp: str) -> None:
        self.stdout = io.StringIO(newline="\n")
        self.stderr = io.StringIO(newline="\n")

        class FrozenDatetime:
            @staticmethod
            def now(zone: Any) -> datetime:
                if zone is not timezone.utc:
                    raise AssertionError("source requested a non-UTC clock")
                return datetime.fromisoformat(timestamp.replace("Z", "+00:00"))

        def headless_print(text: str, style: Any = None, file: Any = None) -> None:
            if file not in (self.stdout, self.stderr):
                raise AssertionError("unexpected stream")
            builtins.print(text, file=file)

        def observed_exit(code: int) -> None:
            raise ObservedExit(code)

        constants: dict[str, Any] = {}
        assignments: list[ast.stmt] = [
            node
            for node in source_tree("core/fab_constant.py").body
            if isinstance(node, ast.Assign)
            and any(
                isinstance(target, ast.Name)
                and (
                    target.id.startswith(("ERROR_", "EXIT_CODE_"))
                    or target.id in ("WARNING_INVALID_JSON_FORMAT", "FAB_OUTPUT_FORMAT")
                )
                for target in node.targets
            )
        ]
        exec(
            compile(ast.Module(body=assignments, type_ignores=[]), "constants", "exec"),
            constants,
        )
        constant_object = SimpleNamespace(
            **{
                key: value
                for key, value in constants.items()
                if not key.startswith("__")
            }
        )
        namespace: dict[str, Any] = {
            "__name__": "isolated_output_reference",
            "json": json,
            "html": html,
            "Enum": Enum,
            "Any": Any,
            "Dict": Dict,
            "List": List,
            "Optional": Optional,
            "Namespace": argparse.Namespace,
            "datetime": FrozenDatetime,
            "timezone": timezone,
            "wraps": wraps,
            "fab_constant": constant_object,
            "fab_state_config": SimpleNamespace(get_config=lambda key: "json"),
            "fab_lazy_load": SimpleNamespace(
                questionary=lambda: SimpleNamespace(print=headless_print)
            ),
            "sys": SimpleNamespace(
                stdout=self.stdout, stderr=self.stderr, exit=observed_exit
            ),
            "DEFAULT_ERROR_MESSAGE": "An error occurred while processing the operation",
            "DEFAULT_ERROR_CODE": "UnknownError",
        }
        namespace.update(
            {key: value for key, value in constants.items() if not key.startswith("__")}
        )
        definitions("utils/fab_util.py", {"dumps"}, namespace)
        util = SimpleNamespace(dumps=namespace["dumps"])

        def controlled_import(name: str, *args: Any, **kwargs: Any) -> Any:
            if name != "fabric_cli.utils.fab_util":
                raise AssertionError(f"unapproved source import: {name}")
            return util

        namespace["__builtins__"] = {
            **vars(builtins),
            "__import__": controlled_import,
        }
        definitions(
            "core/fab_exceptions.py", {"FabricCLIError", "FabricAPIError"}, namespace
        )
        definitions(
            "core/fab_output.py",
            {"OutputStatus", "OutputResult", "FabricCLIOutput"},
            namespace,
        )
        definitions(
            "utils/fab_ui.py",
            {
                "print_output_format",
                "print_output_error",
                "_print_output_format_json",
                "_print_error_format_json",
                "_safe_print",
            },
            namespace,
        )
        namespace["fab_ui"] = SimpleNamespace(
            print_output_error=namespace["print_output_error"]
        )
        definitions("core/fab_decorators.py", {"handle_exceptions"}, namespace)
        definitions(
            "main.py",
            {"_handle_unexpected_error", "_handle_keyboard_interrupt"},
            namespace,
        )
        self.namespace = namespace

    def observe(self, case: dict[str, Any]) -> dict[str, Any]:
        """Run actual constructors, UI JSON branches, and error/exit handlers."""
        ns = self.namespace
        values = case["input"]
        kind = case.get("kind", "envelope")
        command = values.get("command")
        exit_code = ns["EXIT_CODE_SUCCESS"]
        if kind == "envelope":
            hidden = values.get("hidden_data")
            if hidden is not None and values.get("hidden_kind") == "enum":
                hidden = [SimpleNamespace(value=value) for value in hidden]
            output = ns["FabricCLIOutput"](
                command=command,
                status=ns["OutputStatus"](values.get("status", "Success")),
                data=values.get("data"),
                hidden_data=hidden,
                message=values.get("message"),
                error_code=values.get("error_code"),
            )
            ns["_print_output_format_json"](
                output.to_json(indent=case.get("indent", 4))
            )
            if values.get("status") == "Failure":
                exit_code = (
                    ns["EXIT_CODE_AUTHORIZATION_REQUIRED"]
                    if output.result._error_code == ns["ERROR_UNAUTHORIZED"]
                    else ns["EXIT_CODE_ERROR"]
                )
        elif kind == "success":
            args = argparse.Namespace(
                command=command, output_format="json", **{f"{command}_subcommand": "ls"}
            )
            ns["print_output_format"](
                args,
                data=values.get("data"),
                message=values.get("message"),
                show_headers=True,
                show_key_value_list=True,
            )
        elif kind in ("error", "decorated", "invalid-json"):
            if kind == "invalid-json":
                try:
                    ns["FabricCLIOutput"](data=[object()]).to_json()
                except ns["FabricCLIError"] as caught:
                    error = caught
                else:
                    raise AssertionError("unsupported source object did not fail")
            elif "request_id" in values:
                error = ns["FabricAPIError"](
                    json.dumps(
                        {
                            "message": values.get("message"),
                            "errorCode": values.get("error_code"),
                            "requestId": values.get("request_id"),
                            "moreDetails": values.get("details", []),
                        }
                    )
                )
            else:
                error = ns["FabricCLIError"](
                    values.get("message"), values.get("error_code")
                )

            @ns["handle_exceptions"]()
            def execute(args: Any) -> None:
                raise error

            exit_code = execute(
                argparse.Namespace(command_path=command, output_format="json")
            )
        elif kind in ("unexpected", "cancelled"):
            args = argparse.Namespace(output_format="json")

            class Unprintable:
                def __str__(self) -> str:
                    raise ValueError("controlled text extraction failure")

            try:
                if kind == "unexpected":
                    text = (
                        Unprintable()
                        if values.get("text_extraction_fails")
                        else values.get("message", "")
                    )
                    ns["_handle_unexpected_error"](Exception(text), args)
                else:
                    ns["_handle_keyboard_interrupt"](args)
            except ObservedExit as error:
                exit_code = error.code
            else:
                raise AssertionError("main handler did not exit")
        else:
            raise AssertionError(f"unknown fixture kind: {kind}")
        return {
            "name": case["name"],
            "stream": "stdout",
            "exit": exit_code,
            "stdout": self.stdout.getvalue(),
            "stderr": self.stderr.getvalue(),
        }


def fixtures() -> dict[str, Any]:
    """Read hand-authored synthetic cases; never rewrite expected values."""
    result = json.loads((DIRECTORY / "cases.json").read_bytes())
    if result["source_commit"] != SOURCE_COMMIT:
        raise AssertionError("fixture source pin differs")
    return result


def assert_observation(
    case: dict[str, Any], actual: dict[str, Any], expected: dict[str, Any]
) -> None:
    """Check full observations, complete trees, byte payloads, key presence and order."""
    name = case["name"]
    if actual != expected:
        raise AssertionError(
            f"{name}: stream/exit/payload mismatch\n{actual!r}\n{expected!r}"
        )
    tree = json.loads(actual["stdout"])
    expected_tree = {
        "timestamp": fixtures()["timestamp"],
        "status": "Success" if case["exit"] == 0 else "Failure",
    }
    if "command" in case:
        expected_tree["command"] = case["command"]
    expected_tree["result"] = case["result"]
    if tree != expected_tree or list(tree) != list(expected_tree):
        raise AssertionError(f"{name}: complete envelope differs: {tree!r}")
    if list(tree["result"]) != list(case["result"]):
        raise AssertionError(f"{name}: result ordering differs")
    if actual["stream"] != "stdout" or actual["stderr"] != "":
        raise AssertionError(f"{name}: JSON must only use stdout")
    if actual["exit"] != case["exit"]:
        raise AssertionError(f"{name}: exit differs from hand-authored expectation")
    payload = actual["stdout"].encode("utf-8")
    if b"\r" in payload or not payload.endswith(b"\n") or payload.endswith(b"\n\n"):
        raise AssertionError(f"{name}: controlled LF byte contract differs")
    if "json" in case and payload != (case["json"] + "\n").encode("utf-8"):
        raise AssertionError(f"{name}: hand-authored byte expectation differs")


def check_reference(observations: Optional[list[dict[str, Any]]] = None) -> int:
    """Compare source to hand-authored expectations and optional Rust observations."""
    inputs = fixtures()
    cases = inputs["cases"]
    if observations is not None and [x["name"] for x in observations] != [
        x["name"] for x in cases
    ]:
        raise AssertionError("observations must contain exactly all cases in order")
    for index, case in enumerate(cases):
        expected = Reference(inputs["timestamp"]).observe(case)
        actual = expected if observations is None else observations[index]
        assert_observation(case, actual, expected)
    return len(cases)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--observed", type=Path)
    arguments = parser.parse_args()
    observed = (
        json.loads(arguments.observed.read_bytes()) if arguments.observed else None
    )
    count = check_reference(observed)
    print(f"PASS {count} source-pinned output cases (semantic + exact LF bytes)")

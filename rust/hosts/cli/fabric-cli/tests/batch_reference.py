# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture unchanged pinned main.py batch orchestration with synthetic handlers."""

from __future__ import annotations

import argparse
import ast
from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import platform
import sys
from types import SimpleNamespace
from typing import Any

ROOT = Path(__file__).resolve().parents[5]
HELPER = ROOT / "rust/tests/compatibility/argv_review/capture.py"
SPEC = importlib.util.spec_from_file_location("pinned_argv_capture", HELPER)
assert SPEC is not None and SPEC.loader is not None
SOURCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SOURCE)
PIN = SOURCE.PIN

CASES = [
    ("single", ["version"]),
    ("repeated", ["version", "version"]),
    ("empty-list-members", ["", "version", "  ", "version", ""]),
    ("only-empty", ["", " \t\r\n"]),
    ("trailing-empty", ["version", ""]),
    ("handled-error-then-success", ["config set key return-1", "version"]),
    ("success-then-handled-error", ["version", "config set key return-4"]),
    ("error-then-error", ["config set key return-4", "config set key return-1"]),
    ("unknown-stops", ["version", "unknown-private-value", "version"]),
    ("parse-error-stops", ["version", "get", "version"]),
    ("help-stops", ["version", "ls -help", "version"]),
    ("bare-group-returns", ["config", "version"]),
    ("cancellation-stops", ["version", "config set key cancel", "version"]),
    ("unexpected-stops", ["version", "config set key raise", "version"]),
    ("quotes-not-shell", ['get "Demo Workspace/Book.Notebook"', "version"]),
    ("shell-characters-not-operators", ["get item;other | next && end", "version"]),
    ("unicode-whitespace", ["get\u001cDemo\u0085Workspace\u00a0Book", "version"]),
    ("aliases", ["dir /", "config dir"]),
    ("state-survives", ["config set key remember", "config get key"]),
    ("outer-format-not-inherited", ["version", "config get output_format"]),
    ("inner-format-independent", ["version --output_format text", "version"]),
]


def observe(commands: list[str], output_format: str | None) -> dict[str, Any]:
    """Run original main/_execute/error handlers; only I/O and handlers are injected."""
    events: list[list[Any]] = []
    root = SOURCE.parser([])
    subparsers = root._subparsers._group_actions[0]
    setup_tree = ast.parse(SOURCE.source("core/fab_parser_setup.py"))
    create = next(
        node for node in setup_tree.body
        if isinstance(node, ast.FunctionDef) and node.name == "create_parser_and_subparsers"
    )
    root_flags = [
        node for node in create.body
        if isinstance(node, ast.Expr)
        and isinstance(node.value, ast.Call)
        and isinstance(node.value.func, ast.Attribute)
        and isinstance(node.value.func.value, ast.Name)
        and node.value.func.value.id == "parser"
        and node.value.func.attr == "add_argument"
    ]
    namespace = {"parser": root}
    exec(compile(ast.Module(body=root_flags, type_ignores=[]), "pinned-root-flags", "exec"), namespace)
    version = subparsers.add_parser("version")
    pending: list[str] = []
    state: dict[str, str] = {}

    def handler(args: argparse.Namespace) -> int | None:
        events.append(["execute", [args.command, *pending]])
        value = getattr(args, "value", None)
        if value == "cancel":
            raise KeyboardInterrupt
        if value == "raise":
            raise RuntimeError("synthetic failure")
        if value == "remember":
            state["key"] = value
        if getattr(args, "key", None) == "key" and value is None:
            events.append(["state", state.get("key")])
        return {"return-1": 1, "return-4": 4}.get(value)

    version.set_defaults(func=handler)
    seen: set[int] = set()

    def inject(parser: argparse.ArgumentParser) -> None:
        if id(parser) in seen:
            return
        seen.add(id(parser))
        original = parser.parse_args

        def parse(args: list[str], *rest: Any, **kwargs: Any) -> Any:
            pending[:] = args
            return original(args, *rest, **kwargs)

        parser.parse_args = parse
        if "func" in parser._defaults:
            parser.set_defaults(func=handler)
        for action in parser._actions:
            if isinstance(action, argparse._SubParsersAction):
                for child in action.choices.values():
                    inject(child)

    for parser in subparsers.choices.values():
        inject(parser)
    constants = SOURCE.constants()
    ui = SimpleNamespace(
        print_grey=lambda _: events.append(["separator"]),
        print=lambda _: events.append(["blank"]),
        print_output_format=lambda args, **_: events.append(
            ["summary", len(args.command), args.output_format]
        ),
        print_output_error=lambda error, **_: events.append(["error", error.args[1]]),
    )
    main = SOURCE.load("main.py", {
        "sys": sys,
        "argcomplete": SimpleNamespace(autocomplete=lambda *_, **__: None),
        "fab_state_config": SimpleNamespace(init_defaults=lambda: None),
        "fab_logger": SimpleNamespace(print_log_file_path=lambda: None),
        "fab_constant": constants,
        "Command": SimpleNamespace(get_command_path=lambda args: args.command),
        "FabricCLIError": Exception,
        "fab_ui": ui,
        "get_global_parser_and_subparsers": lambda: (root, subparsers),
    }, {"main", "_execute_command", "_handle_keyboard_interrupt", "_handle_unexpected_error"})
    argv = ["fab"]
    if output_format is not None:
        argv.extend(["--output_format", output_format])
    for command in commands:
        argv.extend(["-c", command])
    before = sys.argv
    try:
        sys.argv = argv
        with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            try:
                main.main()
                exit_code = 0
            except SystemExit as error:
                exit_code = error.code
    finally:
        sys.argv = before
    return {"events": events, "exit": exit_code}


def capture() -> dict[str, Any]:
    """Capture source-derived expectations without running any product handler."""
    cases = []
    for name, commands in CASES:
        output_format = "json" if name == "outer-format-not-inherited" else None
        cases.append({
            "id": name, "commands": commands, "output_format": output_format,
            **observe(commands, output_format),
        })
    return {
        "python_pin": PIN,
        "python_version": platform.python_version(),
        "source_sha256": SOURCE.SOURCES,
        "scope": "Original pinned main.py and actual bounded argv schemas. Synthetic handlers/state/UI only; no auth, files, service, prompt_toolkit or product imports. Terminal styling and source error text are not captured.",
        "cases": cases,
    }


def main() -> None:
    """Create new evidence or verify an existing capture without rewriting it."""
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--output", type=Path)
    group.add_argument("--check", type=Path)
    options = parser.parse_args()
    actual = capture()
    if options.check:
        expected = json.loads(options.check.read_text(encoding="utf-8"))
        for key in ["python_pin", "source_sha256", "cases"]:
            if actual[key] != expected[key]:
                raise AssertionError(f"Pinned batch evidence differs: {key}")
        print(f"Verified {len(actual['cases'])} pinned batch observations")
    else:
        with options.output.open("x", encoding="utf-8") as output:
            json.dump(actual, output, ensure_ascii=True, indent=2)
            output.write("\n")


if __name__ == "__main__":
    main()

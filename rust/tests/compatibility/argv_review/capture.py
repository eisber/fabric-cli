# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture pinned Python argv behavior without product imports or service I/O."""

from __future__ import annotations

import argparse
import ast
import base64
from contextlib import redirect_stderr, redirect_stdout
from functools import cache
import hashlib
import html
import io
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
from types import SimpleNamespace
from typing import Any

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
RUST_PIN = "03fab20577a1016178de227eac0ee178921bb8d0"
ROOT = Path(__file__).resolve().parents[4]
SOURCES: dict[str, str] = {}
ARCHIVE: dict[str, Any] | None = None

CASES = [
    ("job-missing-path", ["job", "run-list"]),
    ("job-missing-status-id", ["job", "run-status", "Book.Notebook"]),
    ("job-invalid-polling", ["job", "run", "Book.Notebook", "--polling_interval=0"]),
    ("job-secret-unknown-option", ["job", "run-list", "Book.Notebook", "--token=synthetic-secret-marker"]),
    ("job-valid-list", ["job", "run-list", "Book.Notebook"]),
    ("job-valid-status", ["job", "run-status", "Book.Notebook", "--id=run"]),
    ("job-valid-start", ["job", "start", "Book.Notebook"]),
    ("job-valid-negative-timeout", ["job", "run", "Book.Notebook", "--timeout=-1"]),
    ("job-group-default", ["job"]),
    ("job-long-help", ["job", "run-list", "--help"]),
    ("job-legacy-help", ["job", "run-list", "-help"]),
    ("local-missing-path", ["cd"]),
    ("local-clear-alias-help", ["cls", "--help"]),
    ("local-describe", ["desc", ".Notebook"]),
    ("config-missing-value", ["config", "set", "output_format"]),
    ("config-valid-set", ["config", "set", "output_format", "json"]),
    ("config-list-alias", ["config", "dir"]),
    ("config-group-default", ["config"]),
    ("read-list-alias", ["dir", "/"]),
    ("read-missing-path", ["get"]),
    ("read-invalid-output-format", ["ls", "--output_format=xml"]),
    ("read-joined-query", ["get", "Book.Notebook", "-q", "name", "extra"]),
    ("resource-create-alias", ["create", "Demo.Workspace"]),
    ("resource-delete-alias", ["del", "Demo.Workspace", "-f"]),
    ("acl-group-default", ["acl"]),
    ("acl-list-alias", ["acl", "dir", "Demo.Workspace"]),
    ("label-group-default", ["label"]),
    ("label-missing-name", ["label", "set", "Book.Notebook"]),
    ("raw-api-missing-endpoint", ["api"]),
    ("raw-api-valid-endpoint", ["api", "workspaces"]),
    ("transfer-missing-output", ["export", "Book.Notebook"]),
    ("transfer-valid-export", ["export", "Book.Notebook", "-o", "output", "-f"]),
]


@cache
def source(path: str) -> str:
    """Read only the exact immutable Python Git object."""
    if ARCHIVE is None:
        data = subprocess.check_output(
            ["git", "show", f"{PIN}:src/fabric_cli/{path}"], cwd=ROOT, timeout=10,
        )
    else:
        blob = ARCHIVE["blobs"][path]
        data = base64.b64decode(blob["base64"], validate=True)
        if hashlib.sha256(data).hexdigest() != blob["sha256"]:
            raise ValueError(f"Source archive digest mismatch: {path}")
    SOURCES[path] = hashlib.sha256(data).hexdigest()
    return data.decode("utf-8")


def load(path: str, namespace: dict[str, Any], names: set[str] | None = None) -> SimpleNamespace:
    """Execute unchanged definitions with explicit import-boundary injection."""
    tree = ast.parse(source(path), filename=path)
    tree.body = [
        node for node in tree.body
        if not isinstance(node, (ast.Import, ast.ImportFrom))
        and (names is None or getattr(node, "name", None) in names)
    ]
    tree.body.insert(0, ast.ImportFrom(
        module="__future__", names=[ast.alias(name="annotations")], level=0,
    ))
    ast.fix_missing_locations(tree)
    namespace = dict(namespace)
    exec(compile(tree, f"{PIN}:{path}", "exec"), namespace)
    return SimpleNamespace(**namespace)


class Constants(SimpleNamespace):
    """Only documentation metadata may be substituted."""

    def __getattr__(self, name: str) -> str:
        if name.endswith("_DESCRIPTION"):
            return name
        raise AttributeError(name)


def constants() -> Constants:
    """Retain all literal source constants, especially platform/mode semantics."""
    values = {}
    for node in ast.parse(source("core/fab_constant.py")).body:
        if isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name):
            try:
                values[node.targets[0].id] = ast.literal_eval(node.value)
            except (ValueError, TypeError):
                continue
    return Constants(**values)


def lazy_command(module: str, function: str) -> Any:
    """Retain dispatch identity without importing or invoking the handler."""
    def dispatch(_: Any) -> None:
        raise AssertionError("A source command handler must never execute")
    dispatch.source_handler = f"{module}.{function}"
    return dispatch


def parser(events: list[dict[str, Any]]) -> argparse.ArgumentParser:
    """Compose original parser classes and registrations, not a copied schema."""
    def terminal(formatted: str, plain: str, to_stderr: bool = False) -> None:
        plain = html.unescape(plain)
        events.append({"kind": "terminal", "stream": "stderr" if to_stderr else "stdout", "plain": plain})
        print(plain, file=sys.stderr if to_stderr else sys.stdout)

    ui = load("utils/fab_ui.py", {
        "html": html, "_safe_print_formatted_text": terminal,
    }, {"print_warning"})
    ui.display_help = lambda commands, **_: events.append({"kind": "group-help", "commands": commands})
    logger = load("core/fab_logger.py", {"utils_ui": ui}, {"log_warning"})
    errors = load("utils/fab_error_parser.py", {
        "argparse": argparse, "fab_logger": logger, "utils_ui": ui,
    })
    config = constants()
    os_command = load("utils/fab_util.py", {
        "platform": platform, "fab_constant": config,
    }, {"get_os_specific_command"}).get_os_specific_command
    validators = load("parsers/fab_parser_validators.py", {"argparse": argparse})
    common = {
        "argparse": argparse, "os": os, "fab_constant": config,
        "utils_error_parser": errors, "utils_ui": ui, "fab_ui": ui,
        "lazy_command": lazy_command, "get_os_specific_command": os_command,
        "validate_positive_int": validators.validate_positive_int,
        "fab_config_completers": SimpleNamespace(
            complete_config_keys=lambda *_: [], complete_config_values=lambda *_: [],
        ),
    }
    modules = {
        name: load(f"parsers/fab_{stem}_parser.py", common)
        for name, stem in [
            ("jobs_parser", "jobs"), ("config_parser", "config"),
            ("fs_parser", "fs"), ("acls_parser", "acls"),
            ("labels_parser", "labels"), ("api_parser", "api"),
            ("describe_parser", "describe"),
        ]
    }
    setup = load("core/fab_parser_setup.py", {
        **modules, "argparse": argparse, "re": re, "sys": sys,
        "fab_constant": config, "fab_logger": logger, "fab_ui": ui,
        "utils_error_parser": errors,
        "fab_global_params": load("parsers/fab_global_params.py", {"argparse": argparse}),
    }, {"CustomHelpFormatter", "CustomArgumentParser"})
    root = setup.CustomArgumentParser(prog="fab")
    subparsers = root.add_subparsers(dest="command")
    for name in ["jobs_parser", "config_parser", "acls_parser", "labels_parser", "api_parser", "describe_parser"]:
        modules[name].register_parser(subparsers)
    for name in [
        "ls", "get", "cd", "pwd", "open", "clear", "mkdir", "rm",
        "export", "bulk_export", "import",
    ]:
        getattr(modules["fs_parser"], f"register_{name}_parser")(subparsers)
    return root


def observe_python(argv: list[str]) -> dict[str, Any]:
    """Capture parser exits and actual help; project only the terminal boundary."""
    events: list[dict[str, Any]] = []
    stdout, stderr = io.StringIO(), io.StringIO()
    outcome: dict[str, Any] = {}
    with redirect_stdout(stdout), redirect_stderr(stderr):
        root = parser(events)
        try:
            args = root.parse_args(argv)
            fields = vars(args).copy()
            function = fields.pop("func", None)
            outcome["accepted"] = True
            outcome["arguments"] = fields
            outcome["handler"] = getattr(function, "source_handler", getattr(function, "__name__", None))
            if outcome["handler"] == "show_help":
                function(args)
            outcome["exit"] = 0
        except SystemExit as error:
            outcome["accepted"] = error.code == 0
            outcome["exit"] = error.code
    return {
        **outcome, "stdout": stdout.getvalue(), "stderr": stderr.getvalue(),
        "events": events, "requests": 0, "handlers_executed": False,
        "terminal_boundary": "Original print_warning selects stream/text; rich terminal writer injected as plain text. Not full CLI byte parity.",
    }


def observe_rust(binary: Path, argv: list[str]) -> dict[str, Any]:
    """Optionally capture an existing exact-build binary without credentials."""
    environment = {
        key: value for key, value in os.environ.items()
        if key.upper() in ("SYSTEMROOT", "WINDIR", "TEMP", "TMP")
    }
    run = subprocess.run(
        [str(binary.resolve(strict=True)), *argv], env=environment,
        capture_output=True, text=True, timeout=10, check=False,
    )
    return {"exit": run.returncode, "stdout": run.stdout, "stderr": run.stderr}


def main() -> None:
    """Write a new immutable evidence capture; never update an existing oracle."""
    global ARCHIVE
    options = argparse.ArgumentParser(description=__doc__)
    options.add_argument("--output", type=Path, required=True)
    options.add_argument("--rust-binary", type=Path)
    options.add_argument("--source-archive", type=Path)
    args = options.parse_args()
    if args.output.exists():
        raise FileExistsError("Evidence capture must use a new destination")
    if args.source_archive:
        ARCHIVE = json.loads(args.source_archive.read_text(encoding="utf-8"))
        if ARCHIVE["pin"] != PIN:
            raise ValueError("Source archive must use the exact Python pin")
    records = [
        {"id": name, "argv": argv, "python": observe_python(argv)}
        for name, argv in CASES
    ]
    for record in records:
        record["rust"] = (
            observe_rust(args.rust_binary, record["argv"])
            if args.rust_binary else {"execution": "not run; no local heavy grant"}
        )
    result = {
        "python_pin": PIN, "rust_reference_pin": RUST_PIN,
        "python_version": platform.python_version(), "platform": platform.system(),
        "source_sha256": SOURCES, "cases": records,
        "scope": "Parser/formatter/error/dispatch-boundary capture only; no auth, service, local command execution or 52-command parity claim.",
    }
    with args.output.open("x", encoding="utf-8") as output:
        json.dump(result, output, ensure_ascii=True, indent=2)
        output.write("\n")
    print(f"Captured {len(records)} actual pinned Python parser observations; Rust subprocess execution={bool(args.rust_binary)}")


if __name__ == "__main__":
    main()

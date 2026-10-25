# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Actual pinned read registration and nargs semantics, without product imports."""

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
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import patch

import jmespath

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
MANIFEST_BLOB = "8ecdbf2f8f197a884efb6cec6ae9c69c5e587ca1"
CONSUMERS_BLOB = "d4a3d204da9fcada745938d09dde824fa2946766"
BLOBS = {
    "parsers/fab_fs_parser.py": "4747e212731a4d75ee7f69ea1c21cae552b2562c",
    "core/fab_parser_setup.py": "e65e37c2aa82f94b84ae4f76f97b49f6d385d8f5",
    "parsers/fab_global_params.py": "4288dfbaa5a4486f0fc27c5fda4d74b5436fcbe1",
    "utils/fab_util.py": "ce2c976d31b8d56996c2f0a7db9ab9abe7fcd145",
}


def support(area: str) -> Any:
    """Reuse the unchanged independent references, never product imports."""
    spec = importlib.util.spec_from_file_location(
        f"_query_arguments_{area}", HERE.parent / area / "reference.py"
    )
    if spec is None or spec.loader is None:
        raise AssertionError(f"Missing reference support: {area}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


COMMANDS = support("commands")


def source_tree(relative: str, raw: bytes | None = None) -> ast.Module:
    """Reject source drift before compiling complete original definitions."""
    if raw is None:
        raw = ROOT.joinpath("src", "fabric_cli", *relative.split("/")).read_bytes()
    raw = raw.replace(b"\r\n", b"\n")
    blob = hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()
    if blob != BLOBS[relative]:
        raise AssertionError(f"Source drift: {relative}")
    return ast.parse(raw, filename=relative)


class Reference:
    """Original argparse subclasses, read registrations, globals and process_nargs."""

    def __init__(self) -> None:
        COMMANDS.verify_source()
        trees = {relative: source_tree(relative) for relative in BLOBS}
        self.namespace: dict[str, Any] = {
            "__builtins__": vars(builtins),
            "__name__": "isolated_read_argument_reference",
            "argparse": argparse,
            "_SubParsersAction": argparse._SubParsersAction,
            "re": re,
            "sys": sys,
            "_fs_module_path": "fabric_cli.commands.fs.fab_fs",
            "fab_constant": SimpleNamespace(
                COMMAND_FS_LS_DESCRIPTION="LS",
                COMMAND_FS_GET_DESCRIPTION="GET",
                FAB_MODE_COMMANDLINE="commandline",
            ),
            "utils_error_parser": SimpleNamespace(
                get_usage_prog=lambda parser: parser.prog,
                invalid_choice=lambda *args: None,
                unrecognized_arguments=lambda *args: None,
                missing_required_arguments=lambda *args: None,
            ),
            "fab_logger": SimpleNamespace(log_warning=lambda *args: None),
            # Registration stores a callback; no command/auth module is loaded.
            "lazy_command": lambda *args: None,
        }
        ns = self.namespace
        for relative, names in (
            ("parsers/fab_global_params.py", {"add_global_flags"}),
            (
                "core/fab_parser_setup.py",
                {"CustomHelpFormatter", "CustomArgumentParser"},
            ),
            (
                "parsers/fab_fs_parser.py",
                {"register_ls_parser", "register_get_parser"},
            ),
            ("utils/fab_util.py", {"process_nargs"}),
        ):
            selected = [
                node
                for node in trees[relative].body
                if isinstance(node, (ast.FunctionDef, ast.ClassDef))
                and node.name in names
            ]
            if {node.name for node in selected} != names:
                raise AssertionError(f"Missing source definition: {relative}")
            exec(
                compile(ast.Module(body=selected, type_ignores=[]), relative, "exec"),
                ns,
            )
            if relative == "parsers/fab_global_params.py":
                ns["fab_global_params"] = SimpleNamespace(
                    add_global_flags=ns["add_global_flags"]
                )

    def arguments(self, argv: list[str]) -> argparse.Namespace | None:
        """Parse real argv under the offline guard, retaining scalar/list shape."""
        ns = self.namespace
        formatter_init = argparse.HelpFormatter.__init__

        def fixed_width(formatter: Any, *args: Any, **kwargs: Any) -> None:
            kwargs["width"] = 80
            formatter_init(formatter, *args, **kwargs)

        with (
            patch.object(argparse, "_", lambda text: text),
            patch.object(argparse.HelpFormatter, "__init__", fixed_width),
            COMMANDS.offline_boundary(),
        ):
            parser = ns["CustomArgumentParser"](prog="fab")
            subparsers = parser.add_subparsers(dest="command", required=False)
            ns["register_ls_parser"](subparsers)
            ns["register_get_parser"](subparsers)
            try:
                args = parser.parse_args(copy.deepcopy(argv))
            except SystemExit as error:
                if error.code != 2:
                    raise AssertionError("Unexpected source help/version execution")
                return None
            if args.command not in {"ls", "dir", "get"}:
                raise AssertionError("Only bounded read commands are measured")
            return args

    def observe(self, argv: list[str]) -> dict[str, Any]:
        """Normalize actual source parse meaning; never evaluate expected fields."""
        args = self.arguments(argv)
        if args is None:
            return {"error": True}
        ns = self.namespace
        with COMMANDS.offline_boundary():
            query = ns["process_nargs"](args.query)
            is_list = args.command in {"ls", "dir"}
            return {
                "read": {
                    "kind": "list" if is_list else "get",
                    "name": args.command,
                    "path": args.path or [],
                    "query": query,
                    "long": getattr(args, "long", False),
                    "all": getattr(args, "all", False),
                    "output": ns["process_nargs"](getattr(args, "output", None)),
                    "verbose": getattr(args, "verbose", False),
                    "force": getattr(args, "force", False),
                    "output_format": args.output_format,
                }
            }


def load_cases() -> list[dict[str, Any]]:
    """Read hand-authored expectations with an independently fixed inventory."""
    raw = (HERE / "cases.json").read_bytes()
    manifest = json.loads(raw)
    validate_manifest(manifest, raw)
    result = []
    for value in manifest["cases"]:
        case = copy.deepcopy(value)
        case["expected"] = (
            {"error": True}
            if case.get("error")
            else {"read": {**manifest["defaults"], **case["read"]}}
        )
        result.append(case)
    return result


def validate_manifest(manifest: dict[str, Any], raw: bytes) -> None:
    """Reject edits, removed cases or rewritten expectations after source freeze."""
    raw = raw.replace(b"\r\n", b"\n")
    blob = hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()
    if blob != MANIFEST_BLOB:
        raise AssertionError("Immutable argument fixture changed")
    if json.loads(raw) != manifest:
        raise AssertionError("Argument fixture content differs from protected bytes")
    if manifest["source_commit"] != SOURCE_COMMIT:
        raise AssertionError("Argument fixture source pin changed")
    ids = [case["id"] for case in manifest["cases"]]
    if len(ids) != 60 or len(set(ids)) != 60:
        raise AssertionError("Argument inventory changed")


def native_observations(probe: Path, cases: list[dict[str, Any]]) -> list[Any]:
    """Execute a development-only raw argv probe outside the source I/O guard."""
    inputs = [
        {"id": case["id"], "parse_only": True, "command": case["argv"]}
        for case in cases
    ]
    result = subprocess.run(
        [str(probe.resolve(strict=True))],
        input="".join(json.dumps(value) + "\n" for value in inputs),
        text=True,
        capture_output=True,
        check=True,
        timeout=120,
    )
    if result.stderr:
        raise AssertionError("Native argument probe wrote to stderr")
    return [json.loads(line) for line in result.stdout.splitlines()]


def compare(
    cases: list[dict[str, Any]], observations: list[dict[str, Any]]
) -> list[str]:
    """Compare exact parse meaning, not a compiling parser or expected replay."""
    if [value["id"] for value in observations] != [case["id"] for case in cases]:
        raise AssertionError("Native argument inventory/order changed")
    reference = Reference()
    failures = []
    for case, native in zip(cases, observations):
        source = reference.observe(case["argv"])
        if source != case["expected"]:
            failures.append(f"{case['id']}: frozen expectation != source: {source!r}")
        if native["parse"] != source:
            failures.append(
                f"{case['id']}: native {native['parse']!r} != source {source!r}"
            )
    return failures


def consumer_inputs(manifest: dict[str, Any], case: dict[str, Any]) -> dict[str, Any]:
    """Supply only synthetic HTTP and host context, never expected output."""
    inputs: dict[str, Any] = {
        "id": case["id"],
        "raw_argv": True,
        "command": copy.deepcopy(case["argv"]),
        "current_workspace": copy.deepcopy(manifest["workspace"]),
        "config": {
            "folder_listing_enabled": case["probe"] == "folder",
            "show_hidden": False,
            "item_sort": "byname",
        },
    }
    if case.get("get"):
        inputs["current_item"] = copy.deepcopy(manifest["item"])
        bodies = [manifest["metadata"]]
    else:
        bodies = [manifest["items"]]
        if case["probe"] == "folder":
            bodies.append(manifest["folders"])
    if case.get("folder_context"):
        inputs["current_folders"] = [{"id": "f", "displayName": "F"}]
    inputs["responses"] = [
        {"status": 200, "body": copy.deepcopy(body)} for body in bodies
    ]
    return inputs


def consumer_expected(case: dict[str, Any]) -> dict[str, Any]:
    """Serialize hand-authored expected protocol fields, independently of native."""
    exit_code = case.get("exit_code", 0)
    request = "https://api.fabric.microsoft.com/v1/workspaces/w/"
    requests = [request + ("notebooks/n" if case.get("get") else "items")]
    if case["probe"] == "folder" and not case.get("get"):
        requests.append(request + "folders?recursive=True")
    envelope = {
        "timestamp": COMMANDS.TIMESTAMP,
        "status": "Failure" if exit_code else "Success",
        "command": case["argv"][0],
        "result": case["result"],
    }
    return {
        "id": case["id"],
        "exit_code": exit_code,
        "stdout": json.dumps(envelope, indent=4) + "\n",
        "stderr": "",
        "requests": requests,
        "scopes": [list(COMMANDS.SCOPE) for _ in requests],
    }


def consumer_source(
    reference: Reference,
    folder_reference: Any,
    inputs: dict[str, Any],
    case: dict[str, Any],
    sources: dict[str, ast.Module],
) -> dict[str, Any]:
    """Feed actual argparse Namespace to existing complete source helper chains."""
    args = reference.arguments(case["argv"])
    if args is None:
        raise AssertionError("A consumer fixture must parse successfully in source")
    args.command_path = args.command
    args.output = getattr(args, "output", None)
    args.deep_traversal = False
    if case.get("get"):
        with COMMANDS.offline_boundary():
            args.query = reference.namespace["process_nargs"](args.query)
            args.output = reference.namespace["process_nargs"](args.output)
    module = COMMANDS if case.get("get") else folder_reference
    namespace = module.SimpleNamespace

    def collaborators(*positional: Any, **kwargs: Any) -> SimpleNamespace:
        # The original generic GET oracle disables only the language provider.
        # Inject the actual pinned development library, not an output callback.
        if (
            case.get("get")
            and set(kwargs) == {"search"}
            and getattr(kwargs["search"], "__name__", None) == "deny"
        ):
            if jmespath.__version__ != "1.0.1":
                raise AssertionError("Expected development jmespath 1.0.1")
            kwargs["search"] = jmespath.search
        return namespace(*positional, **kwargs)

    with (
        patch.object(module, "_arguments", lambda inputs: copy.deepcopy(args)),
        patch.object(module, "SimpleNamespace", collaborators),
    ):
        return module.observe(inputs, sources)


def load_consumers(raw: bytes | None = None) -> dict[str, Any]:
    """Protect the new source-verified consumer inputs and expected protocol."""
    if raw is None:
        raw = (HERE / "consumers.json").read_bytes()
    raw = raw.replace(b"\r\n", b"\n")
    blob = hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()
    if blob != CONSUMERS_BLOB:
        raise AssertionError("Immutable consumer fixture changed")
    manifest = json.loads(raw)
    if manifest["source_commit"] != SOURCE_COMMIT or len(manifest["cases"]) != 16:
        raise AssertionError("Consumer source/inventory changed")
    return manifest


def source_consumers() -> list[tuple[dict[str, Any], dict[str, Any], dict[str, Any]]]:
    """Validate every hand-authored consumer against source before any native run."""
    manifest = load_consumers()
    folder_reference = support("folder_commands")
    sources = folder_reference.load_sources()
    reference = Reference()
    observed = []
    for case in manifest["cases"]:
        inputs = consumer_inputs(manifest, case)
        expected = consumer_expected(case)
        source = consumer_source(reference, folder_reference, inputs, case, sources)
        if source != expected:
            raise AssertionError(
                f"{case['id']}: consumer expectation != actual source\n"
                f"Expected: {expected!r}\nSource: {source!r}"
            )
        observed.append((case, inputs, source))
    return observed


def compare_consumers(read_probe: Path, folder_probe: Path) -> dict[str, int]:
    """Assert actual parser -> host -> HTTP SDK -> query -> output representatives."""
    sources = source_consumers()
    native_count = 0
    for case, inputs, expected in sources:
        probes = (
            [read_probe, folder_probe]
            if case["probe"] == "both"
            else [folder_probe if case["probe"] == "folder" else read_probe]
        )
        for probe in probes:
            native = COMMANDS.run_rust(probe, [inputs])[0]
            if native != expected:
                raise AssertionError(
                    f"{case['id']}: raw argv native consumer != source\n"
                    f"Expected: {expected!r}\nNative: {native!r}"
                )
            native_count += 1
    return {"source_consumers": len(sources), "native_consumers": native_count}


def main() -> None:
    """Require independently authored expectations and actual source/native runs."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-probe", type=Path)
    parser.add_argument("--folder-probe", type=Path)
    args = parser.parse_args()
    cases = load_cases()
    reference = Reference()
    sources = [
        {"id": case["id"], "parse": reference.observe(case["argv"])} for case in cases
    ]
    failures = compare(
        cases,
        native_observations(args.rust_probe, cases) if args.rust_probe else sources,
    )
    if failures:
        raise AssertionError("\n".join(failures))
    consumers = {"source_consumers": len(source_consumers())}
    if args.folder_probe:
        if not args.rust_probe:
            parser.error("--folder-probe requires --rust-probe")
        folder_arguments = native_observations(args.folder_probe, cases)
        failures = compare(cases, folder_arguments)
        if failures:
            raise AssertionError("\n".join(failures))
        consumers = compare_consumers(args.rust_probe, args.folder_probe)
    print(
        json.dumps(
            {
                "source_commit": SOURCE_COMMIT,
                "argument_cases": len(cases),
                "source_exact": len(cases),
                "native_compared": args.rust_probe is not None,
                **consumers,
            }
        )
    )


if __name__ == "__main__":
    main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual pinned CLI deploy wrapper boundaries with injected capabilities."""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import re
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Any

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"


def source(
    root: Path, relative: str, names: set[str], namespace: dict[str, Any]
) -> str:
    """Execute selected immutable definitions, not Python package initialization."""
    data = subprocess.run(
        ["git", "--no-pager", "-C", str(root), "show", f"{PIN}:{relative}"],
        check=True,
        capture_output=True,
        timeout=10,
    ).stdout
    tree = ast.parse(data.decode("utf-8-sig"))
    nodes = [
        node
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name in names
    ]
    if len(nodes) != len(names):
        raise ValueError(f"Missing selected functions: {relative}")
    exec(compile(ast.Module(body=nodes, type_ignores=[]), relative, "exec"), namespace)
    return hashlib.sha256(data).hexdigest()


class WrapperError(Exception):
    """Only the source wrapper's injected CLI exception type."""


def capture(root: Path) -> dict[str, Any]:
    """Invoke original wrapper code with synthetic service/UI/config callbacks."""
    namespace: dict[str, Any] = {"json": json, "re": re, "FabricCLIError": WrapperError}
    util = "src/fabric_cli/utils/fab_util.py"
    hashes = {
        util: source(
            root,
            util,
            {"get_dict_from_params", "get_dict_from_parameter", "merge_dicts"},
            namespace,
        ),
    }
    parameter_cases = []
    for name, tokens in [
        ("empty", []),
        ("json-typing", ['truth=true,count=12,unset=null,quoted="12",plain=yes']),
        (
            "dotted-depth-one",
            ['core.workspace_id=id,config_override={"unpublish":{"skip":true}}'],
        ),
        ("backslash-normalization", [r"path=C:\synthetic\repo"]),
        (
            "quoted-and-object",
            [
                'config_override={"core":{"item_types_in_scope":["Notebook"]}},host_app="untrusted"'
            ],
        ),
        ("last-exact-key", ["count=1,count=2"]),
        ("comma-inside-quotes-source-regex-boundary", ['text="first,count=2"']),
        ("comma-whitespace", ["one=1,   two=2"]),
    ]:
        params = namespace["get_dict_from_params"](tokens, max_depth=1)
        for key in params:
            if isinstance(params[key], str):
                try:
                    params[key] = json.loads(params[key])
                except json.JSONDecodeError:
                    pass
        parameter_cases.append({"id": name, "tokens": tokens, "expected": params})

    fs = "src/fabric_cli/commands/fs/fab_fs_deploy.py"
    wrapper = "src/fabric_cli/commands/fs/deploy/fab_fs_deploy_config_file.py"
    hashes[fs] = source(root, fs, {"exec_command"}, namespace)
    hashes[wrapper] = source(
        root,
        wrapper,
        {
            "deploy_with_config_file",
            "_apply_bulk_publish_feature_flags",
            "_remove_bulk_publish_feature_flags",
        },
        namespace,
    )
    wrappers = []
    for name, force, confirmed, bulk, fail in [
        ("decline", False, False, False, False),
        ("confirm", False, True, False, False),
        ("force", True, False, False, False),
        ("bulk-cleanup-success", True, False, True, False),
        ("bulk-cleanup-failure", True, False, True, True),
    ]:
        calls = []
        flags: set[str] = set()

        def append(flag: str) -> None:
            flags.add(flag)
            calls.append(["append", flag])

        def remove(flag: str) -> None:
            flags.discard(flag)
            calls.append(["remove", flag])

        def confirm(message: str) -> bool:
            calls.append(["confirm", message])
            return confirmed

        def deploy(**kwargs: Any) -> Any:
            kwargs.pop("token_credential")
            calls.append(["deploy", kwargs])
            if fail:
                raise ValueError("synthetic failure")
            return SimpleNamespace(message="Deployment completed successfully")

        namespace.update(
            {
                "fab_constant": SimpleNamespace(
                    FAB_DEBUG_ENABLED="debug",
                    API_USER_AGENT="ms-fabric-cli",
                    FAB_VERSION="0.1.0",
                    ERROR_IN_DEPLOYMENT="DeploymentFailed",
                ),
                "fab_state_config": SimpleNamespace(get_config=lambda key: "false"),
                "disable_file_logging": lambda: calls.append(["disable_file_logging"]),
                "append_feature_flag": append,
                "remove_feature_flag": remove,
                "create_fabric_token_credential": lambda: object(),
                "deploy_with_config": deploy,
                "fab_ui": SimpleNamespace(
                    prompt_confirm=confirm,
                    print_warning=lambda message: calls.append(["warning", message]),
                    print_output_format=lambda args, message: calls.append(
                        ["output", message]
                    ),
                ),
            }
        )
        args = SimpleNamespace(
            config="synthetic.yml",
            target_env="dev",
            force=force,
            bulk_publish=bulk,
            params=['host_app="untrusted",config_override={"unpublish":{"skip":true}}'],
        )
        error = None
        try:
            namespace["exec_command"](args)
        except WrapperError as exception:
            error = list(exception.args)
        wrappers.append(
            {
                "id": name,
                "force": force,
                "confirmed": confirmed,
                "bulk": bulk,
                "serviceFailure": fail,
                "calls": calls,
                "remainingFlags": sorted(flags),
                "error": error,
            }
        )
    return {
        "source": f"microsoft/fabric-cli@{PIN}",
        "hashes": hashes,
        "boundary": "Actual pinned max-depth-one parameter parser and deploy force/confirm/wrapper/JSON typing/host_app/finally lifecycle. All external capability callbacks are synthetic; no package, auth or service initialization.",
        "parameters": parameter_cases,
        "wrappers": wrappers,
    }


def main() -> None:
    """Capture once before native expectations, otherwise check the fixed receipt."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = capture(args.reference_root)
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__)
        .with_name("deploy_wrapper_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError("Pinned CLI deploy wrapper differs from immutable receipt")
    print(
        json.dumps(
            {
                "source": PIN,
                "parameters": len(result["parameters"]),
                "wrappers": len(result["wrappers"]),
                "status": "passed",
            }
        )
    )


if __name__ == "__main__":
    main()

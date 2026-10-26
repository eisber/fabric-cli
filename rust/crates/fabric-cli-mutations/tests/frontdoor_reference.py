# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Original registrations/handlers vs argv -> real Rust resource SDK/HTTP consumer."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from types import SimpleNamespace
from typing import Any

from metadata_set_reference import reference as set_reference
from operation_reference import functions
from python_reference import CLIError, PIN, load, reference
from shortcut_reference import reference as shortcut_reference


class Descriptions:
    """Supply help strings only, never command behavior."""

    def __getattr__(self, _: str) -> str:
        return "source help"


def source_parser() -> argparse.ArgumentParser:
    """Execute the five unchanged original registrations and global flag function."""
    namespace = functions(
        "parsers/fab_fs_parser.py",
        {
            "register_exists_parser",
            "register_mkdir_parser",
            "register_rm_parser",
            "register_set_parser",
            "register_ln_parser",
        },
        {
            "fab_constant": Descriptions(),
            "utils_error_parser": SimpleNamespace(get_usage_prog=lambda _: "fab"),
            "lazy_command": lambda *_: None,
            "_fs_module_path": "unused-offline",
        },
    )
    globals_ = load("parsers/fab_global_params.py", {"argparse": argparse})
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    for name in ["exists", "mkdir", "rm", "set", "ln"]:
        namespace[f"register_{name}_parser"](subparsers)
    for child in set(subparsers.choices.values()):
        globals_.add_global_flags(child)
    return parser


def expected(case: dict[str, Any], parser: argparse.ArgumentParser) -> dict[str, Any]:
    """Obtain expected behavior from the actual source, not a Rust-shaped golden."""
    args = parser.parse_args(case["argv"])
    canonical = {"create": "mkdir", "del": "rm", "mklink": "ln"}.get(
        args.command, args.command
    )
    original = dict(
        case,
        command={"mkdir": "create", "rm": "remove"}.get(canonical, canonical),
        force=getattr(args, "force", False),
        hard=getattr(args, "hard", False),
    )
    if canonical == "ln":
        trace = shortcut_reference(
            dict(
                path="Files/Chosen.Shortcut",
                type=args.type,
                input=args.input,
                force=args.force,
                confirm=case.get("confirm", True),
                replies=case["replies"],
            )
        )
        # The existing independent shortcut fixture deliberately uses different
        # synthetic IDs. Compare the same route with this consumer's selected IDs.
        trace["requests"] = [
            dict(
                request,
                url=request["url"].replace(
                    "/workspaces/workspace/items/item/", "/workspaces/ws/items/id/"
                ),
            )
            for request in trace["requests"]
        ]
        trace["outputs"] = [
            dict(message=message, data=None) for message in trace["outputs"]
        ]
    elif canonical == "set":
        normalized = functions(
            "utils/fab_util.py", {"process_nargs"}, {}
        )["process_nargs"]
        trace = set_reference(
            dict(
                original,
                query=normalized(args.query),
                input=normalized(args.input),
                status=case["replies"][0]["status"]
                if case["replies"]
                else 200,
            )
        )
    else:
        if canonical == "mkdir":
            helpers = functions(
                "utils/fab_util.py",
                {
                    "get_dict_from_params",
                    "get_dict_from_parameter",
                    "try_get_json_value_from_string",
                    "merge_dicts",
                },
                {
                    "re": re,
                    "json": json,
                    "FabricCLIError": CLIError,
                    "fab_constant": SimpleNamespace(ERROR_INVALID_INPUT="InvalidInput"),
                    "ErrorMessages": SimpleNamespace(
                        Common=SimpleNamespace(
                            invalid_parameter_format=lambda _: "Invalid parameter format"
                        )
                    ),
                },
            )
            original["params"] = helpers["get_dict_from_params"](args.params)
        if case["replies"]:
            original.update(case["replies"][0])
        trace = reference(original)
    requests = []
    for request in trace["requests"]:
        requests.append(
            dict(
                method=request["method"],
                url=request.get("url")
                or "https://api.fabric.microsoft.com/v1/" + request["uri"],
                payload=request["payload"],
            )
        )
    result = dict(
        command=canonical,
        output_format=args.output_format,
        requests=requests,
        outputs=trace["outputs"],
        error=trace["error"],
    )
    if canonical in ("set", "ln"):
        result.update(notices=trace["notices"], prompts=trace["prompts"])
    if canonical == "set":
        result["metadata_reads"] = trace["metadata_reads"]
    return result


def cases() -> list[dict[str, Any]]:
    """Cover every claimed canonical leaf with original-source happy invocations."""
    result: list[dict[str, Any]] = []
    for kind in ["Workspace", "Folder", "Lakehouse"]:
        path = (
            "Team.Workspace"
            if kind == "Workspace"
            else f"Team.Workspace/Thing.{kind}"
        )
        for identifier in [None, "id"]:
            result.append(
                dict(argv=["exists", path], kind=kind, id=identifier, replies=[])
            )
        params = ["capacityName=none"] if kind == "Workspace" else ["description=hello"]
        result.append(
            dict(
                argv=["mkdir", path, "-P", *params],
                kind=kind,
                replies=[dict(status=201, data={"id": "created"})],
            )
        )
    for force, confirm, status in [
        (False, False, 200),
        (False, True, 200),
        (True, True, 202),
        (True, True, 204),
    ]:
        for kind in ["Folder", "Notebook"]:
            result.append(
                dict(
                    argv=["rm", f"Team.Workspace/Thing.{kind}"]
                    + (["-f"] if force else []),
                    kind=kind,
                    id="id",
                    confirm=confirm,
                    replies=[dict(status=status)] if force or confirm else [],
                )
            )
        result.append(
            dict(
                argv=[
                    "set",
                    "Team.Workspace/Thing.Notebook",
                    "-q",
                    "displayName",
                    "-i",
                    "'After",
                    "Name'",
                ]
                + (["-f"] if force else []),
                kind="Notebook",
                id="id",
                metadata={"displayName": "Thing"},
                confirm=confirm,
                replies=[dict(status=status)] if force or confirm else [],
            )
        )
        result.append(
            dict(
                argv=[
                    "ln",
                    "Team.Workspace/Thing.Lakehouse/Files/Chosen.Shortcut",
                    "--type",
                    "oneLake",
                    "-i",
                    "{'workspaceId':'other','itemId':'target','path':'Files'}",
                ]
                + (["-f"] if force else []),
                kind="Lakehouse",
                id="id",
                confirm=confirm,
                replies=[dict(status=status, data={"name": "Chosen"})]
                if force or confirm
                else [],
            )
        )
    return result


def main() -> None:
    """Compare immutable Python source with the compiled public Rust consumer."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True)
    args = parser.parse_args()
    inputs = cases()
    originals = [expected(case, source_parser()) for case in inputs]
    run = subprocess.run(
        [args.probe],
        input="".join(json.dumps(case) + "\n" for case in inputs),
        text=True,
        encoding="utf-8",
        capture_output=True,
        check=True,
    )
    actual = [json.loads(line) for line in run.stdout.splitlines()]
    for case, old, new in zip(inputs, originals, actual, strict=True):
        observed = {key: new[key] for key in old}
        if observed != old:
            raise AssertionError(
                json.dumps(dict(case=case, python=old, rust=observed), indent=2)
            )
        if new["remaining_replies"] != 0:
            raise AssertionError("Unused synthetic response")
        for plan in new["plans"]:
            if plan["stream"] != "stdout" or plan["exit"] != 0:
                raise AssertionError("Incorrect source output stream or exit")
            envelope = json.loads(plan["payload"])
            if envelope["command"] != old["command"]:
                raise AssertionError("Incorrect source command envelope")
    print(
        json.dumps(
            dict(source=PIN, canonical_leaves=5, frontdoor_cases=len(inputs), result="passed")
        )
    )


if __name__ == "__main__":
    main()

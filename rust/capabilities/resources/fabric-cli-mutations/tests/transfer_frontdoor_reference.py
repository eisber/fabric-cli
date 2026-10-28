# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Pinned original cp/mv registrations and handlers vs real native storage adapter."""

from __future__ import annotations

import argparse
import io
import json
import subprocess
from types import SimpleNamespace
from typing import Any

from onelake_write_reference import reference
from frontdoor_parser_reference import source_root_parser
from operation_reference import functions
from python_reference import PIN

PREFIX = "/Team.Workspace/Lake.Lakehouse/"


class Descriptions:
    """Provide help metadata only."""

    def __getattr__(self, _: str) -> str:
        return "source help"


def source_parser() -> argparse.ArgumentParser:
    """Execute unchanged original copy/move registrations."""
    namespace = functions(
        "parsers/fab_fs_parser.py",
        {"register_cp_parser", "register_mv_parser"},
        {
            "fab_constant": Descriptions(),
            "utils_error_parser": SimpleNamespace(get_usage_prog=lambda _: "fab"),
            "lazy_command": lambda *_: None,
            "_fs_module_path": "unused-offline",
        },
    )
    parser = source_root_parser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    namespace["register_cp_parser"](subparsers)
    namespace["register_mv_parser"](subparsers)
    return parser


def source_local_copy(case: dict[str, Any], target: dict[str, str]) -> dict[str, Any]:
    """Run original local orchestration with an isolated in-memory file boundary."""
    notices: list[dict[str, str]] = []
    uploads: list[bytes] = []
    destination = SimpleNamespace(
        path=PREFIX + target["path"],
        path_id="/workspace/item/" + target["path"],
    )
    source = SimpleNamespace(
        path="local.csv",
        name="local.csv",
        is_directory=lambda: False,
        is_file=lambda: True,
    )
    namespace = functions(
        "commands/fs/cp/fab_fs_cp_onelake.py",
        {"copy_local_2_onelake"},
        {
            "cp_utils": SimpleNamespace(
                check_onelake_destination=lambda _: None,
                get_onelake_file_destination=lambda context, _: context,
                upload_file_onelake=lambda _, content: uploads.append(content),
            ),
            "utils_ui": SimpleNamespace(
                print_grey=lambda message: notices.append(
                    dict(kind="progress", message=message)
                )
            ),
            "open": lambda *_: io.BytesIO(bytes(case["bytes"])),
        },
    )
    namespace["copy_local_2_onelake"](source, destination, argparse.Namespace())
    if uploads != [bytes(case["bytes"])]:
        raise AssertionError("Original local handler did not read the injected file")
    trace = reference(
        dict(
            command="upload",
            source_name="local.csv",
            target=target,
            bytes=list(uploads[0]),
            replies=case["replies"],
        )
    )
    trace["notices"] = notices + trace["notices"]
    return trace


def expected(case: dict[str, Any]) -> dict[str, Any]:
    """Use original parsed tokens to choose original synthetic context inputs."""
    args = source_parser().parse_args(case["argv"])
    process = functions("utils/fab_util.py", {"process_nargs"}, {})["process_nargs"]
    source_path = process(args.from_path)
    destination_path = process(args.to_path)
    target = dict(
        path=destination_path.removeprefix(PREFIX),
        kind="directory" if destination_path == PREFIX + "Files" else "undefined",
    )
    if source_path == "local.csv":
        trace = source_local_copy(case, target)
    else:
        trace = reference(
            dict(
                command={"copy": "cp", "move": "mv"}.get(args.command, args.command),
                source=dict(path=source_path.removeprefix(PREFIX), kind="file"),
                target=target,
                force=args.force,
                confirm=case.get("confirm", True),
                replies=case["replies"],
            )
        )
    return trace


def cases() -> list[dict[str, Any]]:
    """Real happy invocations plus source confirmation/accepted branches."""
    result: list[dict[str, Any]] = []
    for destination in ["Files/to.csv", "Files"]:
        result.append(
            dict(
                argv=["cp", PREFIX + "Files/from.csv", PREFIX + destination],
                replies=[
                    dict(
                        status=200,
                        body=[0, 255, 1],
                        headers={"Content-Length": "3", "Content-Type": "text/csv"},
                    ),
                    dict(status=201),
                    dict(status=202),
                    dict(status=200),
                ],
            )
        )
    for force, confirm, status in [
        (False, False, 200),
        (False, True, 200),
        (True, True, 202),
    ]:
        result.append(
            dict(
                argv=["mv", PREFIX + "Files/from.csv", PREFIX + "Files/to.csv"]
                + (["-f"] if force else []),
                confirm=confirm,
                replies=[dict(status=status)] if force or confirm else [],
            )
        )
    result.append(
        dict(
            argv=["copy", "local.csv", PREFIX + "Files/to.csv"],
            bytes=[1, 2, 255],
            replies=[dict(status=201), dict(status=202), dict(status=200)],
        )
    )
    return result


def main() -> None:
    """Compare actual native requests, bytes, confirmations and output to Python."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe", required=True)
    args = parser.parse_args()
    inputs = cases()
    originals = [expected(case) for case in inputs]
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
            raise AssertionError("Unused synthetic storage reply")
        for plan in new["plans"]:
            if plan["stream"] != "stdout" or plan["exit"] != 0:
                raise AssertionError("Incorrect storage command output")
    print(
        json.dumps(
            dict(
                source=PIN,
                canonical_leaves=2,
                transfer_frontdoor_cases=len(inputs),
                result="passed",
            )
        )
    )


if __name__ == "__main__":
    main()

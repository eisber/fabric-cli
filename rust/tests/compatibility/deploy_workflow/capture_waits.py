# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual pinned lifecycle wait calculation with injected clock/environment."""

import argparse
import ast
import json
import logging
from pathlib import Path
from types import SimpleNamespace


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    tree = ast.parse(args.source.read_text(encoding="utf-8"))
    function = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "handle_retry")
    sleeps = []
    namespace = {
        "Optional": __import__("typing").Optional,
        "os": SimpleNamespace(environ={}),
        "time": SimpleNamespace(sleep=sleeps.append, time=lambda: 0),
        "logger": logging.getLogger("offline-oracle"),
        "constants": SimpleNamespace(
            INDENT="->",
            EnvVar=SimpleNamespace(RETRY_DELAY_OVERRIDE_SECONDS=SimpleNamespace(value="FABRIC_CICD_RETRY_DELAY_OVERRIDE_SECONDS")),
        ),
    }
    exec(compile(ast.fix_missing_locations(ast.Module(body=[function], type_ignores=[])), "source:handle_retry", "exec"), namespace)
    cases = []
    for maximum in (30, 120):
        sleeps.clear()
        for attempt in range(1, 7):
            namespace["handle_retry"](attempt=attempt, base_delay=5, response_retry_after=maximum)
        cases.append({"maximum": maximum, "seconds": list(sleeps)})
    args.output.write_text(json.dumps({
        "source_commit": "5ca096c00a9a878c646c59fe537b883eba951329",
        "source_function": "_common/_fabric_endpoint.py:handle_retry",
        "cases": cases,
    }, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()

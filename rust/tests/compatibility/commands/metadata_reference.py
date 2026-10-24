# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Additive, hand-authored raw-metadata cases through the pinned real GET helper."""

from __future__ import annotations

import argparse
import importlib.util
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("_metadata_commands", HERE / "reference.py")
if SPEC is None or SPEC.loader is None:
    raise AssertionError("Cannot load pinned command reference")
REFERENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REFERENCE)

# These are parsed-JSON contracts, not promises to preserve whitespace or numeric
# spellings. Duplicate cases observe json.loads (last value, first key position).
SAMPLES = (
    (
        "unknown_before_known",
        r'{"future":{"z":null,"a":[1,1.0,-0.0,9007199254740993]},"displayName":"\u96ea","id":"item","type":null,"tail":false}',
        [
            "future.z", "future.a[0]", "future.a[1]", "future.a[2]",
            "future.a[3]", "displayName", "id", "type", "tail",
        ],
    ),
    (
        "reordered_keys",
        r'{"type":17,"displayName":"N","id":"item","nothing":null,"nested":{"b":2,"a":1},"integer":1,"float":1.0}',
        ["type", "displayName", "id", "nothing", "nested.b", "nested.a", "integer", "float"],
    ),
    (
        "missing_type",
        r'{"empty":[],"displayName":"N","id":"item","unknown":{"z":null,"a":true},"object":{}}',
        ["empty", "displayName", "id", "unknown.z", "unknown.a"],
    ),
    (
        "parser_duplicate_keys",
        r'{"before":1,"displayName":"old","id":"old","nested":{"z":0,"a":1,"z":2.0},"displayName":"N","id":"item","type":null,"before":1.0}',
        ["before", "displayName", "id", "nested.z", "nested.a", "type"],
    ),
)


def cases() -> list[dict[str, Any]]:
    """Return fixed synthetic inputs and independently hand-authored path lists."""
    return [
        {
            "id": name,
            "comparison": "specialized-uri",
            "input": {
                "id": name,
                "command": ["get", ".", "--output_format", "json"],
                "current_workspace": {
                    "id": "ws", "displayName": "Current", "type": "Workspace",
                },
                "current_item": {
                    "id": "item", "displayName": "Synthetic", "type": "Notebook",
                },
                "responses": [{"status": 200, "body": wire}],
            },
            "python_requests": [
                "https://api.fabric.microsoft.com/v1/workspaces/ws/notebooks/item",
            ],
            "expected": {
                "exit_code": 0,
                "output": {
                    "status": "Success", "command": "get", "result": {"data": paths},
                },
                "requests": [
                    "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item",
                ],
            },
        }
        for name, wire, paths in SAMPLES
    ]


def main() -> None:
    """Execute source cases and optionally compare actual SDK-backed native GET."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-probe", type=Path)
    options = parser.parse_args()
    sources = REFERENCE.load_sources()
    samples = cases()
    observed = [
        REFERENCE.observe(case["input"], sources) for case in samples
    ]
    for case, actual in zip(samples, observed):
        REFERENCE.assert_observation(case, actual, python=True)
    if options.rust_probe:
        native = REFERENCE.run_rust(
            options.rust_probe, [case["input"] for case in samples]
        )
        for case, python, actual in zip(samples, observed, native):
            REFERENCE.compare_rust(case, python, actual)
    print(
        f"Checked {len(samples)} additive raw-metadata source cases; "
        f"native outcomes={f'asserted {len(samples)}' if options.rust_probe else 'not requested'}; "
        "generic/specialized URI differences retained, not full GET parity."
    )


if __name__ == "__main__":
    main()

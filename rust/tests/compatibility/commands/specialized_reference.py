# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Additive exact source-helper observations for every pinned metadata collection."""

from __future__ import annotations

import argparse
import ast
import copy
import importlib.util
import json
import re
from enum import Enum, auto
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("_specialized_metadata", HERE / "metadata_reference.py")
if SPEC is None or SPEC.loader is None:
    raise AssertionError("Cannot load raw metadata reference")
METADATA = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(METADATA)
REFERENCE = METADATA.REFERENCE


def routes(sources: dict[str, ast.Module]) -> list[list[Any]]:
    """Check the hand-authored inventory against actual source enums and mapping."""
    fixture = json.loads((HERE / "specialized-routes.json").read_text(encoding="utf-8"))
    if fixture["source_commit"] != REFERENCE.SOURCE_COMMIT:
        raise AssertionError("Specialized source pin changed")
    rows = fixture["routes"]
    env = REFERENCE.OUTPUT.Reference(REFERENCE.TIMESTAMP).namespace
    env.update(Enum=Enum, auto=auto)
    exec(REFERENCE.ITEMS._compile_symbols(sources, "core/fab_types.py", {
        "_BaseItemType", "ItemType", "format_mapping",
    }), env)
    actual = [(str(kind), segment) for kind, segment in env["format_mapping"].items()]
    if [(kind, segment) for kind, segment, _ in rows] != actual or len(rows) != 44:
        raise AssertionError("Hand-authored routes differ from complete source format_mapping")
    # Read only the pinned FS_GET inventory, not an installed product/YAML parser.
    policy = (REFERENCE.ROOT / "src" / "fabric_cli" / "core" / "fab_config" / "command_support.yaml").read_text(encoding="utf-8")
    fs = re.search(r"^  fs:\s*\n(.*?)(?=^  \w+:|\Z)", policy, re.M | re.S)
    get = re.search(r"^      get:\s*\n(.*?)(?=^      \w+:|\Z)", fs.group(1), re.M | re.S) if fs else None
    denied = re.search(r"^        unsupported_items:\s*\n((?:^          - [^\n]+\n)+)", get.group(1), re.M) if get else None
    if denied is None:
        raise AssertionError("Cannot read pinned FS_GET rejection inventory")
    names = re.findall(r"^          - (\w+)", denied.group(1), re.M)
    normalized = {name.replace("_", "").lower() for name in names}
    if (
        len(names) != 13
        or {kind.lower() for kind, _, allowed in rows if not allowed} != normalized
        or sum(allowed is True for _, _, allowed in rows) != 31
    ):
        raise AssertionError("Separate CLI rejection policy differs from source")
    return rows


def cases(rows: list[list[Any]]) -> list[tuple[dict[str, Any], bool]]:
    """Reuse protected error observations and fixed raw-body paths without editing them."""
    old = {case["id"]: case for case in REFERENCE.load_cases()["cases"]}
    result = []
    for kind, segment, allowed in rows:
        for status, template in (
            (200, METADATA.cases()[2]),
            (401, old["get_unauthorized"]),
            (403, old["get_forbidden"]),
            (404, old["get_not_found"]),
        ):
            case = copy.deepcopy(template)
            case["id"] = f"route_{kind}_{status}"
            case["input"]["id"] = case["id"]
            case["input"]["current_item"]["type"] = kind
            case["python_requests"] = [
                f"https://api.fabric.microsoft.com/v1/workspaces/ws/{segment}/item"
            ]
            result.append((case, allowed))
    return result


def main() -> None:
    """Observe every helper route and compare supported commands through real ReadHost."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rust-probe", type=Path)
    options = parser.parse_args()
    sources = REFERENCE.load_sources()
    inventory = cases(routes(sources))
    native_cases = []
    source = []
    for case, allowed in inventory:
        actual = REFERENCE.observe(case["input"], sources)
        REFERENCE.assert_observation(case, actual, python=True)
        if allowed:
            native_cases.append(case)
            source.append(actual)
    if options.rust_probe:
        native = REFERENCE.run_rust(options.rust_probe, [
            case["input"] for case in native_cases
        ])
        for case, python, actual in zip(native_cases, source, native):
            REFERENCE.compare_rust(case, python, actual, "source-routed")
    print(
        f"Checked {len(inventory)} actual helper observations (44 collections x 200/401/403/404); "
        f"native source-route outcomes={len(native_cases) if options.rust_probe else 'not requested'} "
        "(31 command-supported collections x 4); 13 source command rejections remain separate."
    )


if __name__ == "__main__":
    main()

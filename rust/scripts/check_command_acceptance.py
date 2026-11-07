# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Validate declared command acceptance boundaries without claiming test execution."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


SOURCE = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
LIVE = {"unvalidated", "offline_only", "manual_read_smoke", "manual_auth_smoke"}


def require(condition: bool, message: str) -> None:
    """Reject invalid or incomplete inventories rather than silently skipping rows."""
    if not condition:
        raise ValueError(message)


def closed(value: Any, keys: set[str], label: str) -> dict[str, Any]:
    """Require the exact documented keys for an inventory object."""
    require(isinstance(value, dict) and set(value) == keys, f"Invalid {label} fields")
    return value


def strings(value: Any, label: str) -> list[str]:
    """Require nonempty, bounded, unique string declarations."""
    require(isinstance(value, list) and bool(value), f"Empty {label}")
    require(
        all(isinstance(item, str) and 0 < len(item) <= 1024 for item in value)
        and len(set(value)) == len(value),
        f"Invalid or duplicate {label}",
    )
    return value


def validate(
    document: dict[str, Any], source: dict[str, Any], metadata: dict[str, Any]
) -> dict[str, Any]:
    """Check all command names, resource declarations and actual Cargo test targets."""
    closed(document, {
        "copyright", "schema", "python_source", "assessment_base",
        "evidence_policy", "groups", "surfaces", "additional_surfaces",
    }, "inventory")
    require(document["schema"] == 1, "Unsupported acceptance schema")
    require(document["python_source"] == source.get("python_source_commit") == SOURCE,
            "Python source pin differs")
    require(isinstance(document["assessment_base"], str)
            and len(document["assessment_base"]) == 40
            and all(c in "0123456789abcdef" for c in document["assessment_base"]),
            "Assessment base must be an immutable source commit")
    require(isinstance(document["evidence_policy"], str)
            and "not recorded passes" in document["evidence_policy"],
            "Inventory must distinguish target declarations from execution evidence")
    groups = set(strings(document["groups"], "groups"))
    expected_groups = {row["command"] for row in source["commands"] if row["kind"] == "group"}
    require(groups == expected_groups, "Source command groups differ")
    expected = {row["command"] for row in source["commands"] if row["kind"] != "group"}
    members = set(metadata["workspace_members"])
    targets = {
        f"{package['name']}/{target['name']}"
        for package in metadata["packages"] if package["id"] in members
        for target in package["targets"] if target["kind"] == ["test"]
    }
    require(bool(targets), "Cargo metadata has no test target inventory")
    require(isinstance(document["surfaces"], list) and document["surfaces"],
            "Acceptance surfaces are missing")
    commands: set[str] = set()
    identifiers: set[str] = set()
    for row in document["surfaces"]:
        closed(row, {
            "id", "commands", "resource_branches", "option_branches",
            "failure_branches", "rust_targets", "known_differences", "live_acceptance",
        }, "surface")
        require(isinstance(row["id"], str) and row["id"] not in identifiers,
                "Duplicate or invalid surface identifier")
        identifiers.add(row["id"])
        current = set(strings(row["commands"], "commands"))
        require(not current & commands, "A canonical leaf is declared more than once")
        commands.update(current)
        for key in ["resource_branches", "option_branches", "failure_branches", "known_differences"]:
            strings(row[key], key)
        require(row["live_acceptance"] in LIVE, "Unknown live-acceptance classification")
        for target in strings(row["rust_targets"], "Rust targets"):
            require(target in targets, f"Cargo test target does not exist: {target}")
    require(commands == expected,
            f"Canonical command mismatch; missing={sorted(expected - commands)}, extra={sorted(commands - expected)}")
    require(isinstance(document["additional_surfaces"], list), "Missing additional surfaces")
    extra_ids: set[str] = set()
    for row in document["additional_surfaces"]:
        closed(row, {"id", "commands", "boundaries", "rust_targets"}, "additional surface")
        require(row["id"] in {"batch", "repl", "update"} and row["id"] not in extra_ids,
                "Unknown or duplicate additional surface")
        extra_ids.add(row["id"])
        strings(row["commands"], "additional commands")
        strings(row["boundaries"], "additional boundaries")
        for target in strings(row["rust_targets"], "additional Rust targets"):
            require(target in targets, f"Cargo test target does not exist: {target}")
    require(extra_ids == {"batch", "repl", "update"}, "Incomplete mode/update inventory")
    return {
        "schema": 1,
        "python_source": SOURCE,
        "canonical_leaves": len(commands),
        "groups": len(groups),
        "resource_option_surfaces": len(document["surfaces"]),
        "additional_surfaces": sorted(extra_ids),
        "classification": "source-and-target-inventory-valid",
        "tests_executed": False,
        "parity_percentage": None,
    }


def load(path: Path) -> dict[str, Any]:
    """Read a bounded JSON object and reject duplicate keys."""
    require(path.stat().st_size <= 16 * 1024 * 1024, "Inventory input exceeds 16 MiB")

    def unique(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            require(key not in result, f"Duplicate JSON key: {key}")
            result[key] = value
        return result

    result = json.loads(path.read_text(encoding="utf-8-sig"), object_pairs_hook=unique)
    require(isinstance(result, dict), "Inventory input must be an object")
    return result


def main() -> int:
    """Validate a resource/option inventory against frozen commands and Cargo metadata."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--metadata", type=Path, required=True)
    parser.add_argument("--inventory", type=Path)
    args = parser.parse_args()
    root = args.root.resolve(strict=True)
    result = validate(
        load(args.inventory or root / "rust" / "validation" / "command-acceptance.json"),
        load(root / "rust" / "tests" / "compatibility" / "commands" / "inventory.json"),
        load(args.metadata),
    )
    print(json.dumps(result, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, KeyError) as error:
        raise SystemExit(f"Command acceptance inventory failed: {error}") from error

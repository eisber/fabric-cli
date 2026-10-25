# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Controls for the full registration inventory, separate from read fixtures."""

from __future__ import annotations

import json
import unittest
from pathlib import Path

from inventory import inventory


class InventoryTests(unittest.TestCase):
    """Keep hidden, aliased and grouped source registrations in the denominator."""

    def test_all_registered_groups_and_leaves_are_counted(self) -> None:
        """An omitted family or unadvertised leaf must not silently disappear."""
        result = inventory(Path(__file__).resolve().parents[4])
        groups = {
            row["command"] for row in result["commands"] if row["kind"] == "group"
        }
        self.assertEqual(groups, {"auth", "config", "acl", "label", "job", "table"})
        self.assertEqual(result["leaves"], 52)
        self.assertEqual(len(result["commands"]), 58)
        by_name = {row["command"]: row for row in result["commands"]}
        expected_leaves = set(
            "ls mkdir cd rm mv cp exists pwd open export bulk-export import deploy "
            "set get clear ln start stop assign unassign api desc find extension "
            "version".split()
        )
        for group, children in {
            "auth": "login logout status",
            "config": "set get ls clear-cache",
            "acl": "ls rm get set",
            "label": "set rm list-local",
            "job": "start run run-list run-cancel run-sch run-update run-rm run-status",
            "table": "schema load vacuum optimize",
        }.items():
            expected_leaves.update(f"{group} {child}" for child in children.split())
        self.assertEqual(set(by_name) - groups, expected_leaves)
        for leaf in ("deploy", "extension", "label list-local", "version", "clear"):
            self.assertIn(leaf, by_name)
        self.assertEqual(by_name["config ls"]["aliases"], ["config dir"])
        self.assertEqual(by_name["mkdir"]["aliases"], ["create"])
        self.assertEqual(by_name["ls"]["aliases"], ["dir"])
        self.assertEqual(by_name["clear"]["aliases"], ["cls"])
        self.assertEqual(sum(len(row["aliases"]) for row in by_name.values()), 11)
        self.assertEqual(
            by_name["api"]["handler"], "fabric_cli.commands.api.fab_api.request_command"
        )

    def test_argument_cardinality_and_required_fields_are_not_flattened(self) -> None:
        """Flags, defaults and token cardinalities are part of the source contract."""
        result = inventory(Path(__file__).resolve().parents[4])
        by_name = {row["command"]: row for row in result["commands"]}
        arguments = {row["names"][-1]: row for row in by_name["mkdir"]["arguments"]}
        self.assertEqual(arguments["--params"]["nargs"], "*")
        self.assertEqual(arguments["--params"]["default"], ["run=true"])
        arguments = {row["names"][-1]: row for row in by_name["ln"]["arguments"]}
        self.assertTrue(arguments["--type"]["required"])
        self.assertEqual(len(arguments["--type"]["choices"]), 6)
        self.assertEqual(
            next(
                row for row in by_name["get"]["arguments"] if "--query" in row["names"]
            )["nargs"],
            "+",
        )

    def test_root_and_hidden_global_arguments_are_included(self) -> None:
        """Batch/version modes and hidden skill output are not new commands."""
        result = inventory(Path(__file__).resolve().parents[4])
        self.assertEqual(
            [row["names"] for row in result["root_arguments"]],
            [["-c", "--command"], ["-v", "--version"]],
        )
        self.assertEqual(
            [row["names"] for row in result["global_arguments"]],
            [["-help"], ["--output_format"], ["--skill"]],
        )

    def test_committed_inventory_matches_actual_registration_source(self) -> None:
        """Source drift needs explicit inventory review, never a silent lost row."""
        expected = json.loads(
            Path(__file__).with_name("inventory.json").read_text(encoding="utf-8-sig")
        )
        self.assertEqual(inventory(Path(__file__).resolve().parents[4]), expected)


if __name__ == "__main__":
    unittest.main()

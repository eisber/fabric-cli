# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Offline non-vacuity tests for the declared command acceptance inventory."""

from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import tempfile
import unittest

import check_command_acceptance as checker


ROOT = Path(__file__).resolve().parents[2]


class InventoryTests(unittest.TestCase):
    """Exercise exact command/target coverage without running product code."""

    def setUp(self) -> None:
        self.document = checker.load(ROOT / "rust" / "validation" / "command-acceptance.json")
        self.source = checker.load(ROOT / "rust" / "tests" / "compatibility" / "commands" / "inventory.json")
        target_names = {
            target
            for row in self.document["surfaces"] + self.document["additional_surfaces"]
            for target in row["rust_targets"]
        }
        packages: dict[str, list[str]] = {}
        for target in sorted(target_names):
            name, test = target.split("/")
            packages.setdefault(name, []).append(test)
        self.metadata = {
            "workspace_members": list(packages),
            "packages": [
                {"id": name, "name": name,
                 "targets": [{"name": test, "kind": ["test"]} for test in tests]}
                for name, tests in packages.items()
            ],
        }

    def validate(self, document: dict | None = None) -> dict:
        """Validate one fixture with a fake target catalog, not actual execution."""
        return checker.validate(document or self.document, self.source, self.metadata)

    def test_inventory_covers_all_frozen_leaves_and_separates_modes(self) -> None:
        result = self.validate()
        self.assertEqual(result["canonical_leaves"], 52)
        self.assertEqual(result["groups"], 6)
        self.assertEqual(result["resource_option_surfaces"], 24)
        self.assertFalse(result["tests_executed"])
        self.assertIsNone(result["parity_percentage"])

    def test_missing_extra_or_duplicate_commands_are_rejected(self) -> None:
        for mode in ["missing", "extra", "duplicate"]:
            with self.subTest(mode=mode):
                document = copy.deepcopy(self.document)
                if mode == "missing":
                    document["surfaces"][0]["commands"] = ["does-not-exist"]
                elif mode == "extra":
                    document["surfaces"][0]["commands"].append("imaginary")
                else:
                    document["surfaces"][1]["commands"].append("ls")
                with self.assertRaises(ValueError):
                    self.validate(document)

    def test_missing_targets_and_empty_behavior_boundaries_are_rejected(self) -> None:
        for field in ["rust_targets", "resource_branches", "option_branches", "failure_branches"]:
            document = copy.deepcopy(self.document)
            document["surfaces"][0][field] = []
            with self.assertRaises(ValueError):
                self.validate(document)
        document = copy.deepcopy(self.document)
        document["surfaces"][0]["rust_targets"] = ["fabric-cli/imaginary-test"]
        with self.assertRaisesRegex(ValueError, "does not exist"):
            self.validate(document)

    def test_unknown_status_schema_and_source_pin_are_rejected(self) -> None:
        for key, value in [("schema", 2), ("python_source", "a" * 40),
                           ("assessment_base", "moving-branch"),
                           ("evidence_policy", "all commands fully tested")]:
            document = copy.deepcopy(self.document)
            document[key] = value
            with self.assertRaises(ValueError):
                self.validate(document)
        document = copy.deepcopy(self.document)
        document["surfaces"][0]["live_acceptance"] = "passed-by-declaration"
        with self.assertRaises(ValueError):
            self.validate(document)

    def test_complete_mode_inventory_and_closed_fields_are_required(self) -> None:
        document = copy.deepcopy(self.document)
        document["additional_surfaces"].pop()
        with self.assertRaisesRegex(ValueError, "Incomplete"):
            self.validate(document)
        document = copy.deepcopy(self.document)
        document["surfaces"][0]["execute"] = "arbitrary shell command"
        with self.assertRaisesRegex(ValueError, "fields"):
            self.validate(document)

    def test_json_duplicate_keys_are_not_silently_overwritten(self) -> None:
        parent = os.environ.get("IMPACT_TEST_ROOT") or os.environ.get("RUNNER_TEMP")
        with tempfile.TemporaryDirectory(prefix="acceptance-unit-", dir=parent) as directory:
            path = Path(directory) / "duplicate.json"
            path.write_text('{"schema":1,"schema":2}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "Duplicate"):
                checker.load(path)
            path.write_text(json.dumps([]), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "object"):
                checker.load(path)


if __name__ == "__main__":
    unittest.main()

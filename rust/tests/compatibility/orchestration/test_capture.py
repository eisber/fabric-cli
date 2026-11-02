# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Small source-only assertions; native execution requires its own CI receipt."""

import unittest

import capture


class SourceCaptureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.observed = {
            case["id"]: capture.observe(case, sources)
            for sources in [capture.load_sources()]
            for case in capture.cases()
        }

    def test_inventory_and_request_denominators(self) -> None:
        """Bind the full command denominator instead of the metadata-only subset."""
        self.assertEqual(len(self.observed), 18)
        expected = [1, 2, 2, 3, 5, 5, 7, 5, 7, 2, 2, 2, 3, 1, 1, 3, 3, 5]
        for index, count in enumerate(expected, 1):
            with self.subTest(case=index):
                result = self.observed[f"REQ-{index:03}"]
                self.assertEqual(len(result["requests"]), count)
                logical_reads = count - (1 if index == 12 else 0)
                self.assertEqual(result["scopes"], [capture.SCOPE] * logical_reads)
                self.assertEqual(result["current_before"], result["current_after"])

    def test_metadata_projection_does_not_skip_outer_enrichment(self) -> None:
        """Output equality must not hide missing connection/schedule reads."""
        result = self.observed["REQ-005"]
        self.assertEqual(
            result["requests"][-3:],
            [
                capture.ORIGIN + "workspaces/w/notebooks/n",
                capture.ORIGIN + "workspaces/w/items/n/connections",
                capture.ORIGIN + "workspaces/w/items/n/jobs/RunNotebook/schedules",
            ],
        )
        self.assertEqual(
            result["returned"][0],
            {
                "id": "n",
                "displayName": "Book",
                "description": "offline",
                "connections": [{"id": "connection"}],
                "schedules": [{"id": "schedule"}],
            },
        )
        self.assertEqual(result["exit"], 0)
        self.assertIn('"offline"', result["stdout"])
        optional_failure = self.observed["REQ-006"]["returned"][0]
        self.assertNotIn("connections", optional_failure)
        self.assertEqual(optional_failure["schedules"], [{"id": "schedule"}])

    def test_item_and_workspace_accepted_responses_have_distinct_source_rules(
        self,
    ) -> None:
        """Headerless item metadata is consumed; workspace primary 202 is ignored."""
        for identifier in ("REQ-007", "REQ-008", "REQ-009"):
            self.assertEqual(self.observed[identifier]["exit"], 0)
            self.assertTrue(self.observed[identifier]["stdout"])
        self.assertEqual(self.observed["REQ-007"]["waits"], [0])
        self.assertEqual(self.observed["REQ-009"]["waits"], [0])
        self.assertEqual(self.observed["REQ-010"]["stdout"], "")
        self.assertEqual(self.observed["REQ-010"]["returned"], [{}])

    def test_midpage_failure_is_not_partial_success(self) -> None:
        """Both discovery and rendering must remain incomplete on a later failure."""
        result = self.observed["REQ-011"]
        self.assertEqual(result["exit"], 1)
        self.assertIn('"Failure"', result["stdout"])
        self.assertNotIn('"data"', result["stdout"])

    def test_absolute_root_ignores_stale_current_workspace(self) -> None:
        """The source root special case never resolves the irrelevant saved path."""
        result = self.observed["REQ-015"]
        self.assertEqual(result["current_before"], "/Gone.Workspace")
        self.assertEqual(result["requests"], [capture.ORIGIN + "workspaces"])
        self.assertEqual(result["exit"], 0)
        self.assertIn("Demo.Workspace", result["stdout"])


if __name__ == "__main__":
    unittest.main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Small standard-library regression checks for the WSL cap verifier."""

import unittest

from validator_runner import validate_limits


class LimitTests(unittest.TestCase):
    """Exercise exact thresholds and fail-closed handling."""

    def test_exact_and_reduced_limits(self) -> None:
        """Accept authorized upper bounds and smaller memory/task caps."""
        for memory, tasks in ((6 * 1024**3, 128), (1024**3, 32)):
            validate_limits(
                {
                    "cpu.max": "200000 100000",
                    "memory.max": str(memory),
                    "memory.swap.max": "0",
                    "pids.max": str(tasks),
                }
            )

    def test_unlimited_and_over_budget_limits_fail(self) -> None:
        """Reject every unbounded field and each exceeded threshold."""
        limits = {
            "cpu.max": "200000 100000",
            "memory.max": str(6 * 1024**3),
            "memory.swap.max": "0",
            "pids.max": "128",
        }
        for key, value in (
            ("cpu.max", "max 100000"),
            ("cpu.max", "200001 100000"),
            ("cpu.max", "0 100000"),
            ("cpu.max", "200000 0"),
            ("memory.max", "max"),
            ("memory.max", str(6 * 1024**3 + 1)),
            ("memory.max", "0"),
            ("memory.swap.max", "1"),
            ("memory.swap.max", "max"),
            ("pids.max", "129"),
            ("pids.max", "0"),
            ("pids.max", "max"),
        ):
            with self.subTest(key=key, value=value):
                with self.assertRaises((ValueError, RuntimeError)):
                    validate_limits({**limits, key: value})


if __name__ == "__main__":
    unittest.main()

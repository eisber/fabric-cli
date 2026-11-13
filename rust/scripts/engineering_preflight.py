# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Run the exact offline engineering inventory and retain its native receipt."""

from __future__ import annotations

import argparse
import importlib
import json
import os
import platform
import re
import sys
import unittest
from pathlib import Path
from typing import Any
from unittest.mock import patch

_HOSTS = {"win32": "windows", "linux": "linux", "darwin": "macos"}
_INVENTORY = json.loads(
    Path(__file__).with_name("engineering-inventory.json").read_text(encoding="utf-8")
)
_PATTERN, _COUNT = _INVENTORY["pattern"], _INVENTORY["count"]
_WINDOWS_SKIPS = [tuple(skip) for skip in _INVENTORY["windows_skips"]]


def _preflight(source: Path, artifacts: Path, head: str, host: str, shard: str) -> int:
    source = source.resolve(strict=True)
    artifacts = artifacts.resolve()
    if not source.is_dir() or artifacts == source or source in artifacts.parents:
        raise ValueError("Engineering artifacts must be outside the source directory")
    if host != _HOSTS.get(sys.platform):
        raise ValueError("Engineering platform must match the actual native host")
    if not re.fullmatch(r"[a-fA-F0-9]{40}", head):
        raise ValueError("Engineering receipt requires the exact source head")
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9_-]*", shard):
        raise ValueError("Engineering receipt requires a valid native shard")
    artifacts.mkdir(parents=True, exist_ok=False)
    report: dict[str, Any] = {
        "kind": "engineering-unit-preflight",
        "head": head,
        "platform": host,
        "shard": shard,
        "host": platform.system(),
        "suites": [],
        "pty_receipts": [],
        "passed": False,
    }

    def persist() -> None:
        (artifacts / "engineering-receipt.json").write_text(
            json.dumps(report, indent=2) + "\n", encoding="utf-8"
        )
        print(json.dumps(report))

    persist()
    previous = Path.cwd()
    try:
        os.chdir(artifacts)
        suite = unittest.defaultTestLoader.discover(str(source), pattern=_PATTERN)
        discovered = suite.countTestCases()
        if discovered != _COUNT:
            report["suites"].append(
                {
                    "pattern": _PATTERN,
                    "expected": _COUNT,
                    "discovered": discovered,
                    "run": 0,
                    "passed": False,
                    "reason": "inventory_mismatch",
                }
            )
            persist()
            return 1
        acceptance = importlib.import_module("native_acceptance")
        original_execute = acceptance.execute

        def observe_execute(*args: Any, **kwargs: Any) -> Any:
            captured = original_execute(*args, **kwargs)
            if kwargs.get("terminal") is not None and acceptance.os is os:
                report["pty_receipts"].append(captured[0])
            return captured

        with (artifacts / "native-acceptance.log").open("w", encoding="utf-8") as log:
            with patch.object(acceptance, "execute", observe_execute):
                result = unittest.TextTestRunner(stream=log, verbosity=2).run(suite)
        skips = [(case.id(), reason) for case, reason in result.skipped]
        expected = _WINDOWS_SKIPS if host == "windows" else []
        success = (
            result.wasSuccessful()
            and result.testsRun == _COUNT
            and not result.expectedFailures
            and skips == expected
        )
        report["suites"].append(
            {
                "pattern": _PATTERN,
                "expected": _COUNT,
                "discovered": discovered,
                "run": result.testsRun,
                "failures": len(result.failures),
                "errors": len(result.errors),
                "expected_failures": len(result.expectedFailures),
                "skipped": skips,
                "passed": success,
            }
        )
        report["passed"] = success
        persist()
        return 0 if success else 1
    finally:
        os.chdir(previous)


def main(argv: list[str] | None = None) -> int:
    """Run the exact engineering preflight with explicit source and receipt scope."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--head", required=True)
    parser.add_argument("--platform", choices=tuple(_HOSTS.values()), required=True)
    parser.add_argument("--shard", required=True)
    args = parser.parse_args(argv)
    return _preflight(args.source, args.artifacts, args.head, args.platform, args.shard)


if __name__ == "__main__":
    raise SystemExit(main())

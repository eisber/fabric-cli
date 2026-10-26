# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Verify additive job-operation outputs using unchanged HTTP/exception/decorator bodies."""

import importlib.util
import json
import sys
from pathlib import Path
from typing import Any


def check_reference() -> None:
    """Reuse the released source oracle without duplicating its error formatter."""
    root = Path(__file__).resolve().parents[4]
    released = root / "rust/crates/fabric-cli-mutations/tests"
    sys.path.insert(0, str(released))
    spec = importlib.util.spec_from_file_location("released_error_reference", released / "error_reference.py")
    assert spec and spec.loader
    source = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(source)
    cases = json.loads(Path(__file__).with_name("operation_output.json").read_text(encoding="utf-8"))
    assert cases["python_source_commit"] == source.PIN
    accepted = {"status": 202, "data": {}, "headers": {"x-ms-operation-id": source.OP}}
    checked = 0
    for state in ("Failed", "Cancelled"):
        for case in cases["terminal"]:
            actual = source.reference({"replies": [
                accepted, {"status": 200, "data": {"status": state, "error": case["detail"]}},
            ]})
            expected = {
                "code": "LongRunningOperationCancelled" if state == "Cancelled" else "LongRunningOperationFailed",
                "message": ("The operation was cancelled: " if state == "Cancelled" else "The operation failed: ") + case["repr"],
                "exit": 1,
            }
            assert {key: actual[key] for key in expected} == expected, (state, case["name"], actual)
            checked += 1
    for case in cases["http"]:
        reply: dict[str, Any] = {"status": case["status"], "data": case["body"], "headers": case.get("headers", {})}
        actual = source.reference({"replies": [accepted, reply]})
        expected = {key: case[key] for key in ("code", "message", "exit")}
        assert {key: actual[key] for key in expected} == expected, (case["name"], actual)
        checked += 1
    print(f"PASS {checked} unchanged Python job-operation error output contracts at {source.PIN}")


if __name__ == "__main__":
    check_reference()

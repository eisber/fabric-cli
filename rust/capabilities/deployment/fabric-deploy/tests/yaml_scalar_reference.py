# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture additive scalar contracts from the pinned PyYAML safe loader."""

from __future__ import annotations

import argparse
import datetime
import json
import math
from pathlib import Path
from typing import Any

import yaml


def capture() -> dict[str, Any]:
    """Run the actual source YAML loader, retaining non-JSON scalar type evidence."""
    if yaml.__version__ != "6.0.3":
        raise ValueError("This source capture requires PyYAML6.0.3")
    cases = []
    for text in [
        "y",
        "n",
        "yes",
        "YES",
        "Yes",
        "yEs",
        "on",
        "OFF",
        "null",
        "Null",
        "NULL",
        "nUll",
        "~",
        "0",
        "-0",
        "+0",
        "012",
        "0_12",
        "0x10",
        "0x_10",
        "0_x10",
        "0b10",
        "0b_10",
        "0b102",
        "0o12",
        "08",
        "0_8",
        "_1",
        "1_",
        "1__2",
        "1:20",
        "1_:20",
        "1:2_0",
        "01:20",
        "1:02",
        "1:60",
        "-1:20",
        ".5",
        "1.",
        "1.e+2",
        "1.2e3",
        "1.2e+3",
        "1e3",
        "1e+3",
        "1:20.5",
        ".inf",
        "-.INF",
        ".NaN",
        "2026-01-02",
        "2026-1-2",
        "2026-01-02T03:04:05Z",
        "'on'",
        "'012'",
        "!!str on",
        "18446744073709551616",
        "-9223372036854775808",
        "-9223372036854775809",
    ]:
        value = yaml.safe_load(text)
        unsupported = (
            isinstance(value, (datetime.date, datetime.datetime))
            or isinstance(value, float)
            and not math.isfinite(value)
            or isinstance(value, int)
            and not (-(2**63) <= value <= 2**64 - 1)
            or ":" in text
            and isinstance(value, float)
        )
        cases.append(
            {
                "input": text,
                "pythonType": type(value).__name__,
                "expected": None if unsupported else value,
                "unsupportedNativeDomain": unsupported,
                "pythonText": str(value),
            }
        )
    return {
        "source": "microsoft/fabric-cicd@5ca096c00a9a878c646c59fe537b883eba951329",
        "boundary": "The PyYAML6.0.3 safe_load used by pinned ConfigValidator._validate_yaml_content; source-compatible type evidence is not JSON coercion.",
        "pyyaml": yaml.__version__,
        "cases": cases,
    }


def main() -> None:
    """Capture once before native expectations, otherwise compare fixed cases."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = capture()
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__).with_name("yaml_scalar_source.json").read_text(encoding="utf-8")
    ):
        raise AssertionError("Pinned YAML scalar source differs from immutable receipt")
    print(
        json.dumps(
            {
                "pyyaml": yaml.__version__,
                "cases": len(result["cases"]),
                "status": "passed",
            }
        )
    )


if __name__ == "__main__":
    main()

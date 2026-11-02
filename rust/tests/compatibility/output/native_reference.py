# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independent actual Python numeric/string runtime behavior, with pinned dumps helper."""

import argparse
import json
import struct
from pathlib import Path
from typing import Any, Optional

from reference import DIRECTORY, Reference, fixtures


def native_value(specification: dict[str, Any]) -> Any:
    """Construct actual Python values without passing through JSON numeric/string coercion."""
    kind = specification["kind"]
    if kind == "none":
        return None
    if kind == "bool":
        return specification["value"]
    if kind == "int":
        return int(specification["value"])
    if kind == "float":
        return struct.unpack(">d", bytes.fromhex(specification["bits"]))[0]
    if kind == "string":
        return "".join(chr(point) for point in specification["codepoints"])
    if kind == "list":
        return [native_value(item) for item in specification["items"]]
    if kind == "dict":
        return {
            "".join(chr(point) for point in item["key"]): native_value(item["value"])
            for item in specification["items"]
        }
    raise AssertionError(f"unknown native fixture kind {kind}")


def observe(case: dict[str, Any]) -> dict[str, Any]:
    """Run real runtime repr/str/truth and actual source dumps, not a translated algorithm."""
    source = Reference(fixtures()["timestamp"]).namespace
    value = native_value(case["value"])
    result = {
        "name": case["name"],
        "json": source["dumps"](value),
        "pretty0": source["dumps"](value, indent=0),
        "pretty2": source["dumps"](value, indent=2),
        "pretty4": source["dumps"](value, indent=4),
        "repr": repr(value),
        "str_codepoints": [ord(point) for point in str(value)],
        "truth": bool(value),
    }
    if result["json"] != json.dumps(value):
        raise AssertionError("source dumps differs from runtime default JSON")
    unicode_output = source["dumps"](value, ensure_ascii=False)
    try:
        unicode_output.encode("utf-8")
    except UnicodeEncodeError:
        result["unicode"] = {"error": "surrogate"}
    else:
        result["unicode"] = {"encoded": unicode_output}
    return result


def check(
    observations: Optional[list[dict[str, Any]]] = None, encoding_only: bool = False
) -> int:
    """Compare all codepoints, bytes and truthiness; do not normalize lossy text output."""
    cases = json.loads((DIRECTORY / "native-cases.json").read_bytes())
    if observations is not None and [item["name"] for item in observations] != [
        case["name"] for case in cases
    ]:
        raise AssertionError(
            "native observations must contain exactly all cases in order"
        )
    for index, case in enumerate(cases):
        expected = observe(case)
        for key in ("json", "repr", "truth"):
            if expected[key] != case[key]:
                raise AssertionError(
                    f"{case['name']}: handwritten {key} expectation differs"
                )
        text = case.get("str_codepoints", [ord(point) for point in case.get("str", "")])
        if expected["str_codepoints"] != text:
            raise AssertionError(
                f"{case['name']}: handwritten string codepoints differ"
            )
        if encoding_only:
            expected = {
                key: expected[key]
                for key in (
                    "name",
                    "json",
                    "pretty0",
                    "pretty2",
                    "pretty4",
                    "truth",
                    "unicode",
                )
            }
        if observations is not None and observations[index] != expected:
            raise AssertionError(
                f"{case['name']}: native source mismatch\n"
                f"{observations[index]!r}\n{expected!r}"
            )
    return len(cases)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--observed", type=Path)
    parser.add_argument("--encoding-only", action="store_true")
    args = parser.parse_args()
    values = json.loads(args.observed.read_bytes()) if args.observed else None
    print(f"PASS {check(values, args.encoding_only)} lossless Python runtime cases")

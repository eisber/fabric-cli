# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Compare explicit ensure_ascii=False output to real json.dumps and pinned dumps."""

import argparse
import ast
import hashlib
import io
import json
from pathlib import Path
from typing import Any, Optional

from reference import DIRECTORY, ROOT, Reference, fixtures


def storage_dump(data: Any) -> str:
    """Execute the exact pinned storage json.dump call against a private StringIO."""
    raw = (ROOT / "src" / "fabric_cli" / "utils" / "fab_storage.py").read_bytes()
    digest = hashlib.sha1(
        b"blob " + str(len(raw)).encode("ascii") + b"\0" + raw
    ).hexdigest()
    if digest != "8176b23594241c6a623e4feb2ad6138214bae081":
        raise AssertionError("storage source differs from immutable Python pin")
    function = next(
        node
        for node in ast.parse(raw).body
        if isinstance(node, ast.FunctionDef) and node.name == "_write_to_local"
    )
    calls = [
        node
        for node in ast.walk(function)
        if isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and isinstance(node.func.value, ast.Name)
        and node.func.value.id == "json"
        and node.func.attr == "dump"
    ]
    if len(calls) != 1:
        raise AssertionError("expected one source local-storage JSON dump")
    output = io.StringIO(newline="")
    expression = ast.Expression(body=calls[0])
    eval(
        compile(expression, "fab_storage.py::_write_to_local/json.dump", "eval"),
        {"__builtins__": {}, "json": json, "processed_data": data, "f": output},
    )
    return output.getvalue()


def check(observations: Optional[list[dict[str, Any]]] = None) -> int:
    """Assert full UTF-8 bytes, order, indentation and unchanged ASCII behavior."""
    cases = json.loads((DIRECTORY / "unicode-json-cases.json").read_bytes())
    source = Reference(fixtures()["timestamp"]).namespace
    if observations is not None and [value["name"] for value in observations] != [
        case["name"] for case in cases
    ]:
        raise AssertionError("missing, reordered or extra Unicode observations")
    for index, case in enumerate(cases):
        indent = case.get("indent", 4)
        expected = json.dumps(case["data"], ensure_ascii=False, indent=indent)
        source_output = source["dumps"](case["data"], ensure_ascii=False, indent=indent)
        if expected.encode("utf-8") != source_output.encode("utf-8"):
            raise AssertionError(f"{case['name']}: pinned source/runtime differ")
        if expected.encode("utf-8") != case["expected"].encode("utf-8"):
            raise AssertionError(f"{case['name']}: handwritten bytes differ")
        if indent == 4 and storage_dump(case["data"]).encode(
            "utf-8"
        ) != expected.encode("utf-8"):
            raise AssertionError(
                f"{case['name']}: actual storage json.dump bytes differ"
            )
        ascii_expected = json.dumps(case["data"], indent=indent)
        if observations is not None:
            actual = observations[index]
            if set(actual) != {"name", "encoded", "ascii"}:
                raise AssertionError(f"{case['name']}: observation shape differs")
            if actual["encoded"].encode("utf-8") != expected.encode("utf-8"):
                raise AssertionError(
                    f"{case['name']}: Unicode source/Rust bytes differ"
                )
            if actual["ascii"].encode("utf-8") != ascii_expected.encode("utf-8"):
                raise AssertionError(f"{case['name']}: legacy ASCII changed")
        if expected.endswith("\n") or "\r" in expected:
            raise AssertionError(f"{case['name']}: formatting added a newline or CR")
    return len(cases)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--observed", type=Path)
    args = parser.parse_args()
    observed = json.loads(args.observed.read_bytes()) if args.observed else None
    print(f"PASS {check(observed)} UTF-8 JSON source byte cases")

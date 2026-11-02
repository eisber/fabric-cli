# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual source literal transforms followed by source File payload encoding."""

from __future__ import annotations

import argparse
import ast
import base64
import hashlib
import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from literal_parameter_reference import capture as capture_parameters, oracle


def capture(root: Path) -> dict[str, Any]:
    """Execute the unchanged source property on actual transform observations."""
    transformed = capture_parameters(root)
    path = root / "src" / "fabric_cicd" / "_common" / "_file.py"
    oracle.git(root, "diff", "--exit-code", oracle.CICD_COMMIT, "--", str(path))
    tree = ast.parse(path.read_text(encoding="utf-8-sig"))
    cls = next(
        node
        for node in tree.body
        if isinstance(node, ast.ClassDef) and node.name == "File"
    )
    method = next(
        node
        for node in cls.body
        if isinstance(node, ast.FunctionDef) and node.name == "base64_payload"
    )
    namespace = {"base64": base64}
    exec(
        compile(ast.Module(body=[method], type_ignores=[]), str(path), "exec"),
        namespace,
    )
    payload = namespace["base64_payload"].fget
    return {
        "source": transformed["source"],
        "fileSourceSHA256": hashlib.sha256(path.read_bytes()).hexdigest(),
        "boundary": "Actual pinned literal schema/transforms then original File.base64_payload, without package/auth/filesystem constructors.",
        "cases": [
            {
                "id": case["id"],
                "input": case["input"],
                "parameters": case["parameters"],
                "expectedPart": payload(
                    SimpleNamespace(
                        type="text",
                        contents=case["expected"],
                        relative_path="notebook-content.py",
                    )
                ),
            }
            for case in transformed["cases"]
            if case["valid"]
        ],
    }


def main() -> None:
    """Capture once before native expectation, otherwise check fixed observations."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = capture(args.reference_root)
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__)
        .with_name("literal_parameter_payload_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError(
            "Pinned literal wire source differs from immutable receipt"
        )
    print(
        json.dumps(
            {
                "source": result["source"],
                "cases": len(result["cases"]),
                "status": "passed",
            }
        )
    )


if __name__ == "__main__":
    main()

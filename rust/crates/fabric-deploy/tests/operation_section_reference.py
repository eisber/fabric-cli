# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture original operation-section validation before native boundary fixes."""

from __future__ import annotations

import argparse
import hashlib
import json
import logging
import re
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from config_reference import PIN, load


def capture(root: Path) -> dict[str, Any]:
    """Run actual section validation with no workspace/filesystem initialization."""
    head = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
        timeout=10,
    ).stdout.strip()
    if head != PIN:
        raise ValueError("Wrong pinned source")
    source = root / "src" / "fabric_cicd"
    paths = [source / "constants.py", source / "_common" / "_config_validator.py"]
    subprocess.run(
        ["git", "-C", str(root), "diff", "--exit-code", PIN, "--", *map(str, paths)],
        check=True,
        capture_output=True,
        timeout=10,
    )
    constants: dict[str, Any] = {}
    load(paths[0], {"CONFIG_VALIDATION_MSGS"}, constants)
    namespace: dict[str, Any] = {
        "InputError": ValueError,
        "constants": SimpleNamespace(**constants),
        "logger": logging.getLogger("operation-section-reference"),
        "re": re,
    }
    load(paths[1], {"ConfigValidator"}, namespace)
    cases = []
    for field, value in [
        ("folder_exclude_regex", "^/folder"),
        ("folder_path_to_include", ["/folder"]),
        ("shortcut_exclude_regex", "^shortcut"),
    ]:
        section = {field: value, "skip": True}
        validator = namespace["ConfigValidator"]()
        validator.environment = "dev"
        validator._validate_operation_section(section, "unpublish")
        cases.append(
            {
                "section": "unpublish",
                "input": section,
                "expectedErrors": validator.errors,
            }
        )
    return {
        "source": f"microsoft/fabric-cicd@{PIN}",
        "cases": cases,
        "hashes": {
            "\\".join(path.relative_to(source).parts): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in paths
        },
        "boundary": "Actual ConfigValidator._validate_operation_section; source-forbidden unpublish fields fail even when skip is true.",
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
        .with_name("operation_section_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError("Pinned operation section differs from immutable receipt")
    print(
        json.dumps({"source": PIN, "cases": len(result["cases"]), "status": "passed"})
    )


if __name__ == "__main__":
    main()

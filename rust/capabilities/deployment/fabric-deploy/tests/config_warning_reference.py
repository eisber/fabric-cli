# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual pinned optional-environment warning order and text."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import subprocess
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from config_reference import BASE, PIN, load


def capture(root: Path) -> dict[str, Any]:
    """Execute the unchanged environment validator with a recording logger."""
    head = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        check=True,
        capture_output=True,
        text=True,
        timeout=10,
    ).stdout.strip()
    if head != PIN:
        raise ValueError("Wrong pinned CICD source")
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
    warnings = []
    namespace: dict[str, Any] = {
        "InputError": ValueError,
        "constants": SimpleNamespace(**constants),
        "logger": SimpleNamespace(warning=warnings.append, debug=lambda *args: None),
    }
    load(paths[1], {"ConfigValidator", "_get_config_fields"}, namespace)
    cases = []
    for fields in [
        {"parameter": {"other": "parameter.yml"}},
        {"item_types_in_scope": {"other": ["Notebook"]}},
        {
            "item_types_in_scope": {"prod": ["Notebook"], "test": ["Notebook"]},
            "parameter": {"other": "parameter.yml"},
        },
        {"parameter": {"dev": "parameter.yml"}},
    ]:
        config = copy.deepcopy(BASE)
        config["core"].update(fields)
        validator = namespace["ConfigValidator"]()
        validator.config = config
        validator.environment = "dev"
        warnings.clear()
        validator._validate_environment_exists()
        cases.append(
            {
                "config": config,
                "environment": "dev",
                "expectedWarnings": warnings.copy(),
                "expectedErrors": validator.errors,
            }
        )
    return {
        "source": f"microsoft/fabric-cicd@{PIN}",
        "boundary": "Actual ConfigValidator._validate_environment_exists with a synthetic recording logger, before path resolution or package initialization.",
        "hashes": {
            "\\".join(path.relative_to(source).parts): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in paths
        },
        "cases": cases,
    }


def main() -> None:
    """Capture once before native expectation, otherwise compare fixed observations."""
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
        .with_name("config_warning_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError(
            "Pinned environment warning source differs from immutable receipt"
        )
    print(
        json.dumps({"source": PIN, "cases": len(result["cases"]), "status": "passed"})
    )


if __name__ == "__main__":
    main()

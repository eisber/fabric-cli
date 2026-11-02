# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual source flag protocols with injected globals/environment."""

from __future__ import annotations

import argparse
import contextlib
import copy
import hashlib
import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from literal_parameter_reference import oracle


def capture(root: Path) -> dict[str, Any]:
    """Execute unchanged source bodies without imports or ambient environment."""
    namespace, hashes = oracle.reference_namespace(root)
    if {
        name: oracle.version(name) for name in oracle.DEPENDENCIES
    } != oracle.DEPENDENCIES:
        raise ValueError("Pinned oracle dependencies required")
    source = root / "src" / "fabric_cicd"
    oracle.load_nodes(
        source / "constants.py", {"VALID_ENABLE_FLAGS"}, namespace["constants"].__dict__
    )
    path = source / "_common" / "_validate_env_vars.py"
    oracle.git(root, "diff", "--exit-code", oracle.CICD_COMMIT, "--", str(path))
    oracle.load_nodes(path, {"is_env_flag_enabled"}, namespace)
    hashes["_common/_validate_env_vars.py"] = hashlib.sha256(
        path.read_bytes()
    ).hexdigest()
    namespace["__builtins__"]["__import__"] = lambda name, *args, **kwargs: (
        namespace["constants"]
        if name == "fabric_cicd.constants"
        else (_ for _ in ()).throw(ImportError(name))
    )
    namespace["contextlib"] = contextlib
    oracle.load_nodes(
        source / "_common" / "_config_utils.py", {"config_overrides_scope"}, namespace
    )
    constants = namespace["constants"]
    cases = []
    for value, flags in [
        ("true", ["1", "true", "yes"]),
        ("TRUE", ["1", "true", "yes"]),
        ("yes", ["1", "true", "yes"]),
        ("YeS", ["1", "true", "yes"]),
        ("1", ["1", "true", "yes"]),
        ("false", ["1", "true", "yes"]),
        (" true", ["1", "true", "yes"]),
        ("true ", ["1", "true", "yes"]),
        ("on", ["1", "true", "yes"]),
        ("", ["1", "true", "yes"]),
        ("CUSTOM", ["custom"]),
        ("CUSTOM", ["CUSTOM"]),
        ("able", "enabled"),
        ("true", [True]),
        ("1", [1]),
        ("true", False),
    ]:
        constants.VALID_ENABLE_FLAGS = flags
        namespace["os"] = SimpleNamespace(environ={"FLAG": value})
        valid = True
        expected = None
        try:
            expected = namespace["is_env_flag_enabled"]("FLAG")
        except TypeError:
            valid = False
        cases.append(
            {
                "kind": "environment-flag",
                "input": value,
                "flags": flags,
                "valid": valid,
                "expected": expected,
            }
        )
    for config in [
        {"features": ["enable_bulk_publish", "Case_Sensitive"]},
        {"features": {"dev": ["enable_response_collection"], "prod": ["other"]}},
        {"features": {"prod": ["other"]}},
        {
            "features": ["base", "base"],
            "constants": {"VERSION": {"dev": 42, "prod": 0}},
        },
        {"constants": {"VERSION": None}},
        {"constants": {"VERSION": {"prod": "other"}}},
    ]:
        constants.FEATURE_FLAG = {"base"}
        constants.VERSION = "1.4.0"
        with namespace["config_overrides_scope"](copy.deepcopy(config), "dev"):
            expected = {
                "features": sorted(constants.FEATURE_FLAG),
                "version": constants.VERSION,
            }
        cases.append(
            {
                "kind": "scope",
                "config": config,
                "expected": expected,
                "restored": {
                    "features": sorted(constants.FEATURE_FLAG),
                    "version": constants.VERSION,
                },
            }
        )
    return {
        "source": oracle.SOURCE,
        "sourceHashes": hashes,
        "dependencies": oracle.DEPENDENCIES,
        "boundary": "Actual is_env_flag_enabled and config_overrides_scope with injected constants/import/environment; no package initialization, ambient reads or services.",
        "cases": cases,
    }


def main() -> None:
    """Capture once before Rust expectations; compare fixed data thereafter."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = json.loads(json.dumps(capture(args.reference_root)))
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__)
        .with_name("flag_protocol_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError(
            "Pinned source flag protocol differs from immutable receipt"
        )
    print(json.dumps({"cases": len(result["cases"]), "status": "passed"}))


if __name__ == "__main__":
    main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual source path filters using an owned synthetic offline tree."""

from __future__ import annotations

import argparse
import glob
import json
import os
import shutil
from pathlib import Path
from typing import Any

from literal_parameter_reference import oracle


def capture(root: Path) -> dict[str, Any]:
    """Exercise unmodified source path helpers; create scratch only in this project."""
    namespace, hashes = oracle.reference_namespace(root)
    if {
        name: oracle.version(name) for name in oracle.DEPENDENCIES
    } != oracle.DEPENDENCIES:
        raise ValueError("Pinned oracle dependencies required")
    source = root / "src" / "fabric_cicd"
    oracle.load_nodes(
        source / "constants.py",
        {"WILDCARD_PATH_VALIDATIONS"},
        namespace["constants"].__dict__,
    )
    oracle.load_nodes(
        source / "_parameter" / "_utils.py",
        {
            "_process_regular_path",
            "_process_wildcard_path",
            "_set_wildcard_path_pattern",
            "_resolve_file_path",
            "_validate_wildcard_syntax",
            "_validate_nested_brackets_braces",
        },
        namespace,
    )
    namespace.update({"os": os, "glob": glob})
    files = [
        "content.py",
        "REPORT.json",
        "sub/content.py",
        "sub/other.json",
        "sub/deep/last.py",
        "{a,b}.txt",
    ]
    repository = Path("source_capture_path_filters")
    repository.mkdir()
    try:
        for file in files:
            path = repository / file
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("synthetic", encoding="utf-8")
        repository = repository.resolve()
        cases = []
        for pattern in [
            "content.py",
            "/content.py",
            "\\content.py",
            "./content.py",
            "sub/../content.py",
            "missing",
            "*.py",
            "*.json",
            "**/*.py",
            "sub/*.py",
            "sub/**/*.py",
            "sub/**",
            "**",
            "sub/**/**",
            "sub//*.py",
            "[c]*.py",
            "[!x]*.py",
            "[]*.py",
            "[-x]*.py",
            "{a,b}.txt",
            "*{a,b}.txt",
            "*{a}.txt",
            "../*.py",
            "sub\\*.py",
            "sub/[co]*.py",
            "{ROOT}/sub/*.py",
        ]:
            actual = pattern.replace("{ROOT}", str(repository))
            paths = namespace["process_input_path"](repository, actual)
            expected = sorted(path.relative_to(repository).as_posix() for path in paths)
            cases.append({"pattern": pattern, "expected": expected})
    finally:
        shutil.rmtree(repository)
    return {
        "source": oracle.SOURCE,
        "sourceHashes": hashes,
        "dependencies": oracle.DEPENDENCIES,
        "windowsPaths": os.name == "nt",
        "caseSensitive": os.name != "nt",
        "boundary": "Actual process_input_path/glob/path validation over owned synthetic local files; no service, credentials, ambient environment, external repository reads or temporary-directory APIs.",
        "files": files,
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
        Path(__file__).with_name("path_filter_source.json").read_text(encoding="utf-8")
    ):
        raise AssertionError("Pinned path filters differ from immutable receipt")
    print(json.dumps({"cases": len(result["cases"]), "status": "passed"}))


if __name__ == "__main__":
    main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Reexecute released definition source captures without rewriting their receipts."""

from __future__ import annotations

import argparse
import base64
import copy
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

WORKSPACE = Path(__file__).resolve().parents[2]
TESTS = WORKSPACE / "crates" / "fabric-deploy" / "tests"
SOURCE = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
TIMEOUT_SECONDS = 120
COMPARISONS = (
    ("export", "definition_export_windows.json", True),
    ("set", "definition_set_source.json", False),
    ("import", "definition_import_source.json", True),
    ("bulk", "definition_bulk_source.json", False),
)


def compare(family: str, actual: dict[str, Any], expected: dict[str, Any]) -> None:
    """Require exact ordered observations, with only documented host text newlines.

    The export receipt was captured on Windows. On other hosts, remove exactly
    the Windows text-mode LF expansion from its expected text bytes, as the
    released Rust consumer already does. Binary bytes and observed data are never
    normalized. Committed receipts, request strings and array order stay intact.
    """
    if actual.get("source") != SOURCE or expected.get("source") != SOURCE:
        raise AssertionError(f"{family}: pinned Python source differs")
    if family == "export":
        if expected.get("platform") != "win32" or actual.get("platform") != sys.platform:
            raise AssertionError("export: unexpected source receipt platform")
        expected = copy.deepcopy(expected)
        expected["platform"] = sys.platform
        if os.name != "nt":
            for case in expected["cases"]:
                if case["name"] != "binary":
                    content = base64.b64decode(case["expected_base64"], validate=True)
                    case["expected_base64"] = base64.b64encode(
                        content.replace(b"\r\n", b"\n")
                    ).decode("ascii")
    if json.dumps(actual) != json.dumps(expected):
        raise AssertionError(f"{family}: fresh ordered source receipt differs")


def run(artifacts: Path) -> None:
    """Require all four actual pinned source captures and immutable comparisons."""
    for family, fixture, _ in COMPARISONS:
        for path in (TESTS / f"definition_{family}_reference.py", TESTS / fixture):
            if not path.is_file():
                raise FileNotFoundError(path)
    environment = {
        **os.environ,
        "PYTHONUTF8": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
    }
    artifacts.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="definition-source-", dir=artifacts) as directory:
        temporary = Path(directory)
        for family, fixture, local_files in COMPARISONS:
            output = temporary / f"{family}.json"
            command = [
                sys.executable,
                "-B",
                str(TESTS / f"definition_{family}_reference.py"),
                str(output),
            ]
            if local_files:
                command.append(str(temporary))
            subprocess.run(
                command,
                cwd=WORKSPACE,
                env=environment,
                check=True,
                timeout=TIMEOUT_SECONDS,
            )
            actual = json.loads(output.read_text(encoding="utf-8"))
            expected = json.loads((TESTS / fixture).read_text(encoding="utf-8"))
            compare(family, actual, expected)
            print(f"{family}: exact ordered pinned source receipt matched", flush=True)


def main() -> None:
    """Run source-only checks paired with the public native definition tests."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    run(parser.parse_args().artifacts)


if __name__ == "__main__":
    main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Reproduce additive export receipts from actual pinned Python source, offline."""

import argparse
import base64
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[3]
CASES = ROOT / "rust" / "crates" / "fabric-deploy" / "tests"


def main() -> None:
    """Compare new receipts without rewriting any committed expected data."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", required=True, type=Path)
    args = parser.parse_args()
    artifacts = args.artifacts.resolve()
    if not artifacts.is_relative_to(ROOT) or artifacts == ROOT:
        raise RuntimeError("Artifacts must be confined to this checkout")
    artifacts.mkdir(parents=True, exist_ok=False)
    for stem, filesystem in [
        ("definition_export_fallback", True),
        ("definition_export_native", True),
        ("definition_export_discovery", False),
        ("definition_export_unicode", False),
        ("definition_export_index", False),
        ("definition_export_precondition", False),
        ("definition_transfer_input", False),
        ("definition_export_destination", False),
        ("definition_export_base64", True),
    ]:
        observed = artifacts / f"{stem}.json"
        command = [sys.executable, "-B", str(CASES / f"{stem}_reference.py"), str(observed)]
        if filesystem:
            command.append(str(artifacts / f"{stem}-files"))
        subprocess.run(command, cwd=ROOT, timeout=30, check=True)
        actual = json.loads(observed.read_text(encoding="utf-8"))
        expected = json.loads((CASES / f"{stem}_source.json").read_text(encoding="utf-8"))
        if expected.get("platform") == "win32" and actual.get("platform") != "win32":
            for case in expected["cases"]:
                if case["name"] == "remote-binary":
                    continue
                key = "local_base64" if "local_base64" in case else "expected_base64"
                content = base64.b64decode(case[key]).replace(b"\r\n", b"\n")
                case[key] = base64.b64encode(content).decode("ascii")
        expected.pop("platform", None)
        actual.pop("platform", None)
        if json.dumps(actual, ensure_ascii=True) != json.dumps(expected, ensure_ascii=True):
            raise RuntimeError(f"Actual pinned source differs from {stem} receipt")
        print(f"{stem}: actual pinned source matches immutable receipt")


if __name__ == "__main__":
    main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Re-execute new pinned import captures without rewriting expected receipts."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def main() -> None:
    """Compare every captured request, event, effect and object insertion order."""
    directory = Path(__file__).resolve().parent
    scratch = Path.cwd() / f".import-reference-check-{os.getpid()}"
    scratch.mkdir()
    try:
        for name in ("lifecycle", "inputs"):
            observed = scratch / f"{name}.json"
            subprocess.run(
                [sys.executable, "-B", str(directory / f"definition_import_{name}_reference.py"),
                 str(observed), str(scratch / f"{name}-source")],
                check=True, capture_output=True, text=True, timeout=30,
            )
            text = observed.read_text(encoding="utf-8")
            if name == "inputs" and os.sep != "\\":
                # Only the Windows receipt's native input-path diagnostic differs.
                data = json.loads(text)
                for case in data["cases"]:
                    for event in case["events"]:
                        if "grey" in event:
                            event["grey"] = event["grey"].replace(
                                "<input-root>/file.bin", "<input-root>\\file.bin"
                            )
                text = json.dumps(data)
            actual = json.loads(text, object_pairs_hook=list)
            expected = json.loads(
                (directory / f"definition_import_{name}_source.json").read_text(encoding="utf-8"),
                object_pairs_hook=list,
            )
            if actual != expected:
                raise AssertionError(f"Actual pinned import {name} changed")
            print(f"Pinned import {name}: exact request/event/effect receipt matched")
    finally:
        shutil.rmtree(scratch)


if __name__ == "__main__":
    main()

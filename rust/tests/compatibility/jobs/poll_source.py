# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture actual non-string/HTTP-204 source job polling before native comparison."""

import argparse
import copy
import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from job_execution import compare as execution


def check(capture: Path | None) -> None:
    """Keep actual source stream/order evidence independent from Rust assertions."""
    rows = []
    for status in (None, False, 7, [], {}):
        for output in ("json", "text"):
            case = {
                "name": f"poll/nonstring/{len(rows)}", "leaf": "run",
                "responses": [
                    {"status": 202, "body": None, "headers": {
                        "Location": f"{execution.ORIGIN}/workspaces/w/items/n/jobs/instances/run",
                    }},
                    {"status": 204, "body": None},
                    {"status": 200, "body": {"status": status}},
                ],
                "waits": [10],
            }
            rows.append({"input": case, "format": output,
                         "observed": execution.source_observation(case, output)})
    document = {"source_commit": execution.PIN, "rows": copy.deepcopy(rows)}
    if capture:
        capture.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    else:
        assert json.loads((HERE / "poll_source.json").read_text(encoding="utf-8")) == document
    print(f"PASS {len(rows)} actual-source non-string/204 job polling observations")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--capture", type=Path)
    check(parser.parse_args().capture)

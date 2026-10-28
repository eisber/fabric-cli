# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Additive actual-source observations; never rewrites the 155/396 contracts."""

from __future__ import annotations

import argparse
import copy
import json
import sys
from pathlib import Path

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from job_execution import compare as execution
from job_mutations import compare as mutations
from job_reads import compare as reads

PIN = execution.PIN


def capture() -> list[dict]:
    """Run unchanged pinned parsers, handlers, payload builders and real UI."""
    rows = []
    for resource in execution.RESOURCES:
        for leaf in ("start", "run"):
            for status in (200, 201, 204):
                case = {
                    "name": f"{resource}/{leaf}/no-output-{status}",
                    "resource": resource, "leaf": leaf,
                    "responses": [{"status": status, "body": None}], "waits": [],
                }
                for output in ("json", "text"):
                    rows.append({"input": case, "format": output,
                                 "observed": execution.source_observation(case, output)})
        for leaf, schedule, body, expected in [
            ("run-status", False, {"id": "run"}, {"data": {"id": "run"}}),
            ("run-status", False, {"status": None}, {"data": {"status": None}}),
            ("run-status", False, False, {"data": False}),
            ("run-status", False, [1, None], {"data": [1, None]}),
            ("run-status", True, None, {"data": None}),
            ("run-status", True, {"enabled": "retained"}, {"data": {"enabled": "retained"}}),
            *[("run-list", False, {"value": value}, {"message": "No runs found"})
              for value in (None, False, 0, "", {}, [])],
            ("run-list", False, {"value": [{}, None, 1, "x"]},
             {"data": [{}, {}, {}, {}]}),
            ("run-list", False, {"value": [{"id": "a"}, "no matching column"]},
             {"data": [{"id": "a"}, {}]}),
        ]:
            case = {"name": f"{resource}/{leaf}/{schedule}/{len(rows)}", "leaf": leaf,
                    "response": {"status": 200, "body": body}, "expected_output": expected}
            for output in ("json",):
                observation, _ = reads.source_observation(case, resource, schedule, output)
                observation["requests"] = [
                    {"method": "GET", "url": url, "body": None, "skill": None}
                    for url in [
                        f"{reads.ORIGIN}/workspaces", f"{reads.ORIGIN}/workspaces/w/items",
                        *observation["requests"],
                    ]
                ]
                observation.pop("render_error")
                observation.update(scopes=[["https://api.fabric.microsoft.com/.default"]] * 3,
                                   file_reads=[], waits=[])
                rows.append({"input": dict(case, resource=resource, schedule=schedule),
                             "format": output, "observed": observation})
        for leaf, options, responses in [
            ("run-cancel", ["--id=run"], [{"status": 200, "body": None}]),
            ("run-cancel", ["--id=run", "--wait"], [{"status": 204, "body": None}]),
            ("run-sch", ["--input={}"], [{"status": 200, "body": None}]),
            ("run-sch", ["--input={}"], [{"status": 201, "body": {"id": 42}}]),
            ("run-update", ["--id=schedule", "--enable"],
             [{"status": 204, "body": None}]),
            ("run-update", ["--id=schedule", "--enable"],
             [{"status": 200, "body": {"configuration": None}},
              {"status": 200, "body": {"id": False}}]),
            ("run-rm", ["--id=schedule", "--force"], [{"status": 204, "body": None}]),
            ("run-rm", ["--id=schedule"], []),
        ]:
            case = {"name": f"{resource}/{leaf}/{len(rows)}", "resource": resource,
                    "leaf": leaf, "options": options, "responses": responses, "waits": []}
            case["methods"] = (
                (["GET", "PATCH"] if len(responses) == 2 else ["GET"])
                if leaf == "run-update" else
                (["DELETE"] if responses else []) if leaf == "run-rm" else ["POST"]
            )
            case["confirmations"] = int(leaf == "run-rm" and "--force" not in options)
            for output in ("json", "text"):
                rows.append({"input": case, "format": output,
                             "observed": mutations.source_observation(case, output)})
    return copy.deepcopy(rows)


def check(path: Path | None) -> None:
    """Capture before native execution; immutable source output is the oracle."""
    rows = capture()
    if path:
        document = {"copyright": "Copyright (c) Microsoft Corporation. Licensed under the MIT License.",
                    "source_commit": PIN, "rows": rows}
        path.write_text(json.dumps(document, indent=2, ensure_ascii=True) + "\n", encoding="utf-8")
    else:
        frozen = json.loads((HERE / "command_source.json").read_text(encoding="utf-8"))
        assert frozen["source_commit"] == PIN
        assert frozen["rows"] == rows, "unchanged-source observations differ from the frozen capture"
    print(f"PASS {len(rows)} additive unchanged-source job command observations at {PIN}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--capture", type=Path)
    check(parser.parse_args().capture)

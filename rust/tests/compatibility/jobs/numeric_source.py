# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Capture bounded Unicode numeric spellings from actual pinned Python job code."""

import argparse
import copy
import json
import sys
import unicodedata
from pathlib import Path

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
from job_execution import compare as execution
from job_mutations import compare as mutations


def wide(value: str) -> str:
    """Spell decimal digits in full-width form, preserving ASCII punctuation."""
    return value.translate(str.maketrans("0123456789", "０１２３４５６７８９"))


def check(capture: Path | None) -> None:
    """Freeze real parser/payload/polling behavior without widening JSON bounds."""
    accepted = {"status": 202, "body": None, "headers": {
        "Location": f"{execution.ORIGIN}/workspaces/w/items/n/jobs/instances/run",
    }}
    completed = {"status": 200, "body": {"status": "Completed"}}
    rows = []
    for resource in ("Notebook", "DataPipeline"):
        for kind, spelling, value in [
            ("int", wide("18446744073709551615"), 18446744073709551615),
            ("int", wide("-9_223_372_036_854_775_808"), -9223372036854775808),
            ("float", wide("+1_2.5e+2"), 1250.0),
        ]:
            parameter = {"type": kind, "value": value} if resource == "Notebook" else value
            case = {"name": f"{resource}/{kind}/{len(rows)}", "resource": resource,
                    "leaf": "start", "options": ["--params", f"n:{kind}={spelling}"],
                    "execution_data": {"parameters": {"n": parameter}},
                    "responses": [accepted], "waits": []}
            rows.append({"input": case, "format": "json",
                         "observed": execution.source_observation(case, "json")})
    for options, header, waits in [
        (["--timeout=６_０", "--polling_interval=２"], None, [2]),
        ([], "+１_０", [10]),
        ([], "٧", [7]),
    ]:
        initial = copy.deepcopy(accepted)
        if header is not None:
            initial["headers"]["Retry-After"] = header
        case = {"name": f"numeric-poll/{len(rows)}", "leaf": "run",
                "options": options, "responses": [initial, completed], "waits": waits}
        rows.append({"input": case, "format": "json",
                     "observed": execution.source_observation(case, "json")})
    for kind, interval in [("cron", wide("18446744073709551615")),
                           ("daily", "１２:３４suffix")]:
        case = {
            "name": f"numeric-schedule/{kind}", "leaf": "run-sch",
            "options": [f"--type={kind}", f"--interval={interval}",
                        "--start=２０２６-０１-０１T００:００:００",
                        "--end=２０２６-１２-０１T００:００:００"],
            "responses": [{"status": 201, "body": {"id": "sch"}}], "methods": ["POST"],
        }
        rows.append({"input": case, "format": "json",
                     "observed": mutations.source_observation(case, "json")})
    document = {
        "source_commit": execution.PIN,
        "unicode_version": unicodedata.unidata_version,
        "decimal_blocks": [number for number in range(0x110000)
                           if unicodedata.decimal(chr(number), None) == 0],
        "rows": copy.deepcopy(rows),
    }
    if capture:
        capture.write_text(json.dumps(document, indent=2, ensure_ascii=True) + "\n", encoding="utf-8")
    else:
        assert json.loads((HERE / "numeric_source.json").read_text(encoding="utf-8")) == document
    print(f"PASS {len(rows)} actual-source bounded numeric observations; "
          f"{len(document['decimal_blocks'])} decimal blocks at Unicode {document['unicode_version']}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--capture", type=Path)
    check(parser.parse_args().capture)

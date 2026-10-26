# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Check raw definition bytes against real Python json.dumps and pinned fab_util.dumps."""

import argparse
import json
from pathlib import Path
from typing import Any, Optional

from reference import DIRECTORY, Reference, fixtures


def check(observations: Optional[list[dict[str, Any]]] = None) -> int:
    """Compare exact raw bytes, not an envelope or parsed-value proxy."""
    cases = json.loads((DIRECTORY / "definition-cases.json").read_bytes())
    source = Reference(fixtures()["timestamp"]).namespace
    if observations is not None and [x["name"] for x in observations] != [
        x["name"] for x in cases
    ]:
        raise AssertionError("missing, reordered or additional observations")
    for index, case in enumerate(cases):
        expected = json.dumps(case["data"])
        if expected != source["dumps"](case["data"]) or expected != case["expected"]:
            raise AssertionError(f"{case['name']}: source/handwritten bytes differ")
        if observations is not None:
            if observations[index]["encoded"].encode("utf-8") != expected.encode("utf-8"):
                raise AssertionError(f"{case['name']}: Rust/source bytes differ")
    return len(cases)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--observed", type=Path)
    args = parser.parse_args()
    values = json.loads(args.observed.read_bytes()) if args.observed else None
    print(f"PASS {check(values)} Python-default definition JSON cases")

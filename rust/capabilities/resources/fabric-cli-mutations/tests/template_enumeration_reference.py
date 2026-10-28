# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Order-sensitive actual-source/native consumers for controlled walk inputs."""

from __future__ import annotations

import base64
from copy import deepcopy
import json
import subprocess
from typing import Any

from python_reference import PIN, ROOT, reference
from template_listing_inputs import LISTINGS


def compare(probe: str) -> None:
    """Prove independent file/directory permutations at the real request boundary."""
    cases: list[dict[str, Any]] = []
    orders: list[list[list[str]]] = []
    model_tail = ["definition/model.tmdl", "definition/database.tmdl"]
    report_definition = [
        "definition/version.json", "definition/report.json",
        "definition/pages/pages.json",
        "definition/pages/b8c5fb8d635f898326c6/page.json",
    ]
    theme = ["StaticResources/SharedResources/BaseThemes/CY24SU10.json"]
    for root_reverse in [False, True]:
        for nested_reverse in [False, True]:
            listings = deepcopy(LISTINGS)
            rows = listings["Blank.SemanticModel"]
            if root_reverse:
                rows[0]["files"].reverse()
            if nested_reverse:
                rows[1]["files"].reverse()
            expected = (
                ["definition.pbism", ".platform"] if root_reverse
                else [".platform", "definition.pbism"]
            ) + (list(reversed(model_tail)) if nested_reverse else model_tail) + [
                "definition/tables/Table.tmdl"
            ]
            cases.append(dict(
                kind="SemanticModel", command="create", params={"run": "true", "Description": "notes"},
                capture_creation=True, template_enumeration=listings,
                responses=[dict(status=201, data={"id": "root-id"})],
            ))
            orders.append([expected])
    for directory_reverse in [False, True]:
        for root_reverse in [False, True]:
            for nested_reverse in [False, True]:
                listings = deepcopy(LISTINGS)
                rows = listings["Blank.Report"]
                if root_reverse:
                    rows[0]["files"].reverse()
                if nested_reverse:
                    rows[4]["files"].reverse()
                if directory_reverse:
                    rows[0]["directories"].reverse()
                    listings["Blank.Report"] = [rows[0], *rows[4:], *rows[1:4]]
                definition = (
                    [report_definition[1], report_definition[0], *report_definition[2:]]
                    if nested_reverse else report_definition
                )
                expected = (
                    [".platform", "definition.pbir"] if root_reverse
                    else ["definition.pbir", ".platform"]
                ) + (definition + theme if directory_reverse else theme + definition)
                for dependent in [False, True]:
                    cases.append(dict(
                        kind="Report", command="create",
                        params={"run": "true"} if dependent else {"semanticModelId": "model-id"},
                        capture_creation=True, template_enumeration=listings,
                        responses=(
                            [dict(status=201, data={"id": "model-id"})] if dependent else []
                        ) + [dict(status=201, data={"id": "root-id"})],
                    ))
                    orders.append(
                        [[
                            ".platform", "definition.pbism", "definition/model.tmdl",
                            "definition/database.tmdl", "definition/tables/Table.tmdl",
                        ], expected] if dependent else [expected]
                    )
    for mirror in [None, "genericmirror", "azuresql", "azuresqlmi", "snowflake", "cosmosdb", "unknown"]:
        cases.append(dict(
            kind="MirroredDatabase", command="create",
            params={"run": "true"} if mirror is None else {"mirrorType": mirror, "connectionId": "connection"},
            capture_creation=True, template_enumeration=deepcopy(LISTINGS),
            responses=[dict(status=201, data={"id": "root-id"})],
        ))
        orders.append([["mirroring.json"]])
    expected_outputs = [reference(case) for case in cases]
    result = subprocess.run(
        [probe], input="".join(json.dumps(case) + "\n" for case in cases),
        text=True, encoding="utf-8", capture_output=True, check=True, timeout=120,
    )
    actual_outputs = [json.loads(line) for line in result.stdout.splitlines()]
    for case, expected, actual, expected_orders in zip(
        cases, expected_outputs, actual_outputs, orders, strict=True
    ):
        if actual != expected:
            raise AssertionError(json.dumps(dict(case=case, python=expected, rust=actual), indent=2))
        for output in [expected, actual]:
            observed = [
                [part["path"] for part in request["payload"]["definition"]["parts"]]
                for request in output["requests"]
            ]
            if observed != expected_orders:
                raise AssertionError(dict(expected=expected_orders, actual=observed))
        # Unmodified bytes are independently read from the immutable source pin.
        if case["kind"] == "SemanticModel":
            for part in actual["requests"][0]["payload"]["definition"]["parts"]:
                raw = subprocess.check_output([
                    "git", "show", f"{PIN}:src/fabric_cli/commands/fs/payloads/Blank.SemanticModel/{part['path']}"
                ], cwd=ROOT)
                if part["payloadType"] != "InlineBase64" or base64.b64decode(part["payload"]) != raw:
                    raise AssertionError("Pinned raw payload bytes changed")
    if len({json.dumps(output["requests"][0]["payload"]["definition"]["parts"]) for output in actual_outputs[:4]}) != 4:
        raise AssertionError("Changing file input order did not change both actual outputs")
    invalid = []
    for change in ["missing_template", "missing_file", "duplicate_file", "invalid_file",
                   "duplicate_row", "missing_row", "invalid_directory", "inconsistent_directories"]:
        case = deepcopy(cases[0])
        listing = case["template_enumeration"]["Blank.SemanticModel"]
        if change == "missing_template":
            del case["template_enumeration"]["Blank.SemanticModel"]
        elif change == "missing_file":
            listing[0]["files"].pop()
        elif change == "duplicate_file":
            listing[0]["files"].append(".platform")
            # Python really emits the duplicate. Native rejects this explicitly;
            # it must not silently deduplicate and report source parity.
            source_parts = reference(case)["requests"][0]["payload"]["definition"]["parts"]
            if [part["path"] for part in source_parts].count(".platform") != 2:
                raise AssertionError("Source duplicate classification evidence changed")
        elif change == "invalid_file":
            listing[0]["files"][0] = "../.platform"
        elif change == "duplicate_row":
            listing.append(deepcopy(listing[-1]))
        elif change == "missing_row":
            listing.pop()
        elif change == "invalid_directory":
            listing[1]["directory"] = "../definition"
        else:
            listing[0]["directories"] = []
        case["responses"] = []
        invalid.append(case)
    result = subprocess.run(
        [probe], input="".join(json.dumps(case) + "\n" for case in invalid),
        text=True, encoding="utf-8", capture_output=True, check=True, timeout=120,
    )
    rejected = [json.loads(line) for line in result.stdout.splitlines()]
    if len(rejected) != len(invalid):
        raise AssertionError("Missing invalid-enumeration observations")
    for output in rejected:
        if output["error"] != "InvalidTemplateEnumeration" or output["requests"] or output["outputs"] or output["cache"]:
            raise AssertionError(output)
    partial = deepcopy(cases[5])
    del partial["template_enumeration"]["Blank.Report"]
    partial["responses"] = [dict(status=201, data={"id": "model-id"})]
    result = subprocess.run(
        [probe], input=json.dumps(partial) + "\n", text=True, encoding="utf-8",
        capture_output=True, check=True, timeout=120,
    )
    output = json.loads(result.stdout)
    if (
        output["error"] != "InvalidTemplateEnumeration"
        or len(output["requests"]) != 1
        or output["requests"][0]["payload"]["type"] != "SemanticModel"
        or output["cache"] != [dict(action="upsert", name="Thing_auto.SemanticModel")]
        or output["outputs"]
    ):
        raise AssertionError(output)
    print(json.dumps(dict(template_order_cases=len(cases), invalid_listing_cases=len(invalid), source=PIN, result="passed")))

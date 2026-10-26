# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Freeze actual pinned Python job/UI observations before invoking native code."""

from __future__ import annotations

import argparse
import copy
import importlib.util
import json
import os
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Optional

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
ORIGIN = "https://api.fabric.microsoft.com/v1"
sys.path.insert(0, str(HERE.parent / "output"))
from text_reference import TextReference  # noqa: E402


def load_module(path: Path, name: str) -> Any:
    """Load an unchanged source runner, not product initialization."""
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


JOBS = load_module(HERE.parent / "jobs" / "reference.py", "job_source")


def source_observation(
    case: dict[str, Any], resource: str, schedule: bool, output_format: str
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Execute unchanged job API, list/status handler, projection and UI functions."""
    manifest = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
    existing = json.loads((HERE.parent / "jobs" / "cases.json").read_text(encoding="utf-8"))
    assert manifest["resources"] == existing["resources"]
    assert manifest["timestamp"] == "2026-01-02T03:04:05.000006Z"
    assert manifest["source_commit"] == "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
    reference = TextReference({"name": case["name"], "input": {}})
    ns = reference.reference.namespace
    args = argparse.Namespace(
        command="job", command_path=f"job {case['leaf']}", output_format=output_format,
        schedule=schedule, id="run", ws_id="w", item_id="n",
        jobType=manifest["resources"][resource],
    )
    requests: list[str] = []
    outputs: list[dict[str, Any]] = []

    def request(values: argparse.Namespace, **kwargs: Any) -> Any:
        assert not kwargs and values.method == "get"
        requests.append(f"{ORIGIN}/{values.uri}")
        response = case["response"]
        return SimpleNamespace(
            status_code=response["status"], headers=response.get("headers", {}),
            text=json.dumps(response["body"]),
        )

    def output(values: argparse.Namespace, **kwargs: Any) -> None:
        outputs.append({k: v for k, v in kwargs.items() if k in ("data", "message")})
        ns["print_output_format"](values, **kwargs)

    common: dict[str, Any] = {
        "Namespace": argparse.Namespace, "Any": Any, "Optional": Optional, "Item": Any,
        "ApiResponse": Any, "json": json,
    }
    api = dict(common, fabric_api=SimpleNamespace(do_request=request))
    JOBS.load_functions(ROOT / "src/fabric_cli/client/fab_api_jobs.py", manifest["source_commit"], api)
    listing = dict(common, utils_ui=SimpleNamespace(print_output_format=output))
    JOBS.load_functions(
        ROOT / "src/fabric_cli/utils/fab_cmd_ls_utils.py",
        manifest["source_commit"], listing, {"format_and_print_output"},
    )
    handler = dict(
        common, jobs_api=SimpleNamespace(**api),
        utils_ls=SimpleNamespace(**listing),
        fab_ui=SimpleNamespace(print_output_format=output),
    )
    filename = "fab_jobs_run_list.py" if case["leaf"] == "run-list" else "fab_jobs_run_status.py"
    JOBS.load_functions(
        ROOT / "src/fabric_cli/commands/jobs" / filename,
        manifest["source_commit"], handler,
    )
    handler["exec_command"](args, SimpleNamespace())
    expected = case.get("expected_schedule_output", case.get("expected_output", case.get("expected_source"))) if schedule else case.get("expected_output", case.get("expected_source"))
    assert outputs == ([] if expected == {} else [expected]), (case["name"], outputs, expected)
    return {
        "exit": 0, "stdout": reference.stdout.getvalue(), "stderr": reference.stderr.getvalue(),
        "requests": requests, "render_error": None,
    }, args.__dict__


def discovery(resource: str) -> list[dict[str, Any]]:
    """Independent metadata fixtures, never inferred from a native response."""
    return [
        {"status": 200, "body": {"value": [{"id": "w", "displayName": "Demo", "type": "Workspace"}]}},
        {"status": 200, "body": {"value": [{"id": "n", "displayName": "Book", "type": resource}]}},
    ]


def check(probe: Path | None) -> None:
    """Compare bytes, requests, scopes and exits; gaps cannot pass as exact cases."""
    manifest = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
    inputs: list[dict[str, Any]] = []
    expectations: list[tuple[str, dict[str, Any], str | None]] = []
    for case in manifest["exact"]:
        for resource in manifest["resources"]:
            for schedule in (False, True):
                for output_format in ("json", "text"):
                    observed, _ = source_observation(case, resource, schedule, output_format)
                    observed["requests"] = [
                        f"{ORIGIN}/workspaces", f"{ORIGIN}/workspaces/w/items", *observed["requests"],
                    ]
                    observed["scopes"] = [["https://api.fabric.microsoft.com/.default"]] * 3
                    command = ["job", case["leaf"], f"Demo.Workspace/Book.{resource}"]
                    if case["leaf"] == "run-status":
                        command += ["--id=run"]
                    if schedule:
                        command += ["--schedule"]
                    command += ["--output_format", output_format]
                    inputs.append({
                        "command": command, "responses": [*discovery(resource), case["response"]],
                        "timestamp": manifest["timestamp"],
                    })
                    expectations.append((f"{case['name']}/{resource}/{schedule}/{output_format}", observed, None))
    for case in manifest["gaps"]:
        observed, _ = source_observation(case, "Notebook", case.get("schedule", False), "json")
        command = ["job", case["leaf"], "Demo.Workspace/Book.Notebook"]
        if case["leaf"] == "run-status":
            command += ["--id=run"]
        if case.get("schedule"):
            command += ["--schedule"]
        inputs.append({"command": command, "responses": [*discovery("Notebook"), case["response"]]})
        expectations.append((case["name"], observed, case["native_error"]))
    for leaf in ("run-list", "run-status"):
        case = next(case for case in manifest["exact"] if case["leaf"] == leaf)
        observed, _ = source_observation(case, "Notebook", False, "json")
        observed["requests"] = [
            f"{ORIGIN}/workspaces", f"{ORIGIN}/workspaces/w/items", *observed["requests"],
        ]
        observed["scopes"] = [["https://api.fabric.microsoft.com/.default"]] * 3
        for skill in ("", "selected"):
            command = ["job", leaf, "Demo.Workspace/Book.Notebook", f"--skill={skill}"]
            if leaf == "run-status":
                command += ["--id=run"]
            inputs.append({
                "command": command, "responses": [*discovery("Notebook"), case["response"]],
                "timestamp": manifest["timestamp"],
            })
            expectations.append((f"{leaf}/skill={skill}", copy.deepcopy(observed), None))
    frozen = copy.deepcopy(expectations)
    assert len(frozen) == 68
    if probe is None:
        print("PASS 64 independently pinned source job read observations; 4 declared API gaps")
        return
    environment = {k: v for k, v in os.environ.items() if k.upper() in ("SYSTEMROOT", "WINDIR", "TEMP", "TMP")}
    result = subprocess.run(
        [str(probe.resolve(strict=True))], input="".join(json.dumps(case) + "\n" for case in inputs),
        text=True, encoding="utf-8", capture_output=True, check=True, timeout=60, env=environment,
    )
    assert result.stderr == "", result.stderr
    rows = [json.loads(line) for line in result.stdout.splitlines()]
    assert len(rows) == len(frozen)
    for row, (name, expected, gap) in zip(rows, frozen):
        if gap is None:
            assert row == expected, (name, row, expected)
        else:
            assert row["exit"] == 1 and row["stderr"] == "" and row["render_error"] is None, (name, row)
            assert json.loads(row["stdout"])["result"]["error_code"] == gap, (name, row)
            assert row["requests"][:2] == [f"{ORIGIN}/workspaces", f"{ORIGIN}/workspaces/w/items"]
            assert len(row["requests"]) == len(row["scopes"]) == 3
    assert expectations == frozen, "source expectations were mutated during native execution"
    print("PASS 64 exact native job read comparisons; 4 explicit API gaps fail nonzero")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe", type=Path)
    check(parser.parse_args().probe)

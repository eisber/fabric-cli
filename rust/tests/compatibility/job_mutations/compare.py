# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Compare mounted cancellation/schedule commands with unchanged pinned Python."""

from __future__ import annotations

import argparse
import copy
import io
import json
import os
import re
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Optional

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
sys.path.insert(0, str(HERE.parent))
from job_execution.compare import (  # noqa: E402
    JOBS, ORIGIN, PIN, RESOURCES, SourceItem, SourceParser, TextReference,
    discovery, positive_int,
)

START = "2026-01-01T00:00:00"
END = "2026-12-01T00:00:00"
CRON = {
    "type": "Cron", "interval": 10, "startDateTime": START,
    "endDateTime": END, "localTimeZoneId": "UTC",
}


def source_observation(case: dict[str, Any], output_format: str) -> dict[str, Any]:
    """Execute real parser, wrapper, API, input conversion, polling and UI functions."""
    reference = TextReference({"name": case["name"], "input": {}})
    ui = SimpleNamespace(**reference.reference.namespace)
    waits: list[int] = []
    requests: list[dict[str, Any]] = []
    file_reads: list[str] = []
    confirmations: list[bool] = []
    resource = case.get("resource", "Notebook")
    response_rows = iter(copy.deepcopy(case["responses"]))
    constants = SimpleNamespace(
        COMMAND_JOBS_DESCRIPTION="Jobs", ERROR_NOT_RUNNABLE="NotRunnable",
        ERROR_INVALID_INPUT="InvalidInput", ERROR_JOB_FAILED="JobFailed",
        ERROR_NOT_SUPPORTED="NotSupported",
    )
    common = {
        "Namespace": argparse.Namespace, "Any": Any, "Optional": Optional,
        "Item": SourceItem, "ApiResponse": Any, "json": json,
        "FabricCLIError": JOBS.SourceError, "FabricJobType": JOBS.FabricJobType,
        "fab_constant": constants,
    }

    def request(args: argparse.Namespace, **kwargs: Any) -> Any:
        requests.append({
            "method": args.method.upper(), "url": f"{ORIGIN}/{args.uri}",
            "body": json.loads(kwargs["data"]) if "data" in kwargs else None, "skill": None,
        })
        response = next(response_rows)
        headers = response.get("headers", {})
        return SimpleNamespace(
            status_code=response["status"], text=json.dumps(response["body"]),
            headers={**headers, **{key.lower(): value for key, value in headers.items()}},
        )

    def open_file(path: str, mode: str) -> io.StringIO:
        assert mode == "r"
        file_reads.append(path)
        return io.StringIO(json.dumps(case["files"][path]))

    def confirm() -> bool:
        confirmations.append(True)
        return case.get("confirmed", False)

    ui.prompt_confirm = confirm
    logger = SimpleNamespace(log_warning=ui.print_warning, log_debug=lambda _: None)
    api = dict(common, fabric_api=SimpleNamespace(do_request=request))
    JOBS.load_functions(ROOT / "src/fabric_cli/client/fab_api_jobs.py", PIN, api)
    polling = dict(
        common, CaseInsensitiveDict=dict, DEFAULT_POLLING_INTERVAL=10, fab_logger=logger,
    )
    JOBS.load_functions(ROOT / "src/fabric_cli/utils/fab_http_polling_utils.py", PIN, polling)
    utils = dict(
        common, re=re, CaseInsensitiveDict=dict, fab_ui=ui, fab_logger=logger,
        time=SimpleNamespace(sleep=waits.append, time=lambda: 0), open=open_file,
        jobs_api=SimpleNamespace(**api), get_polling_interval=polling["get_polling_interval"],
        ErrorMessages=SimpleNamespace(Common=SimpleNamespace(
            job_instance_failed=lambda job_id: f"Job instance '{job_id}' Failed",
        )),
    )
    JOBS.load_functions(ROOT / "src/fabric_cli/utils/fab_cmd_job_utils.py", PIN, utils)
    filenames = {
        "run-cancel": ("fab_jobs_run_cancel.py", "run_cancel_command", "jobs_run_cancel"),
        "run-sch": ("fab_jobs_run_sch.py", "run_sch_command", "jobs_run_sch"),
        "run-update": ("fab_jobs_run_update.py", "run_update_command", "jobs_run_update"),
        "run-rm": ("fab_jobs_run_rm.py", "run_rm_command", "jobs_run_rm"),
    }
    filename, wrapper_name, module_name = filenames[case["leaf"]]
    handler = dict(
        common, jobs_api=SimpleNamespace(**api), utils_job=SimpleNamespace(**utils),
        fab_ui=ui, utils_ui=ui,
    )
    JOBS.load_functions(ROOT / "src/fabric_cli/commands/jobs" / filename, PIN, handler)
    item = SourceItem(
        name=f"Book.{resource}", full_name=f"Book.{resource}",
        path=f"/Demo.Workspace/Book.{resource}", id="n", type=resource,
        workspace=SimpleNamespace(id="w"), job_type=JOBS.FabricJobType(RESOURCES[resource]),
    )
    wrappers = dict(
        common, handle_exceptions=lambda: lambda function: function,
        set_command_context=lambda: lambda function: function,
        handle_context=SimpleNamespace(get_command_context=lambda _: item),
        Command=SimpleNamespace(
            JOB_RUN_CANCEL="job run-cancel", JOB_RUN_SCH="job run-sch",
            JOB_RUN_UPDATE="job run-update", JOB_RUN_RM="job run-rm",
        ),
        utils_job=SimpleNamespace(**utils), fab_ui=ui,
        **{module_name: SimpleNamespace(**handler)},
    )
    JOBS.load_functions(
        ROOT / "src/fabric_cli/commands/jobs/fab_jobs.py", PIN, wrappers, {wrapper_name},
    )
    parser_namespace = dict(
        common, _SubParsersAction=Any, show_help=lambda _: None,
        utils_error_parser=SimpleNamespace(get_usage_prog=lambda _: "job"),
        lazy_command=lambda *_: None, _jobs_module_path="unused", validate_positive_int=positive_int,
    )
    JOBS.load_functions(
        ROOT / "src/fabric_cli/parsers/fab_jobs_parser.py", PIN, parser_namespace, {"register_parser"},
    )
    parser = SourceParser(prog="fab")
    parser_namespace["register_parser"](parser.add_subparsers(dest="command"))
    command = ["job", case["leaf"], f"Demo.Workspace/Book.{resource}", *case["options"], "--output_format", output_format]
    args = parser.parse_args(command)
    args.command_path = f"job {case['leaf']}"
    wrappers[wrapper_name](args)
    assert next(response_rows, None) is None
    assert waits == case.get("waits", []), (case["name"], waits)
    assert len(confirmations) == case.get("confirmations", 0), case["name"]
    if "payload" in case:
        assert requests[-1]["body"] == case["payload"], (case["name"], requests[-1])
    assert [row["method"] for row in requests] == case["methods"], (case["name"], requests)
    discovery_requests = [
        {"method": "GET", "url": f"{ORIGIN}/workspaces", "body": None, "skill": None},
        {"method": "GET", "url": f"{ORIGIN}/workspaces/w/items", "body": None, "skill": None},
    ]
    return {
        "exit": 0, "stdout": reference.stdout.getvalue(), "stderr": reference.stderr.getvalue(),
        "requests": [*discovery_requests, *requests],
        "scopes": [["https://api.fabric.microsoft.com/.default"]] * (2 + len(requests)),
        "file_reads": file_reads, "waits": waits, "confirmations": len(confirmations),
    }


def cases() -> list[dict[str, Any]]:
    """Source-valid fixtures for every leaf/resource plus schedule/confirmation branches."""
    options = ["--type=cron", "--interval=10", f"--start={START}", f"--end={END}"]
    existing = {"status": 200, "body": {"id": "sch", "configuration": CRON, "enabled": True}}
    created = {"status": 201, "body": {"id": "sch"}}
    updated = {"status": 200, "body": {"id": "sch"}}
    templates = [
        {"leaf": "run-cancel", "options": ["--id=run"],
         "responses": [{"status": 202, "body": None}], "methods": ["POST"]},
        {"leaf": "run-sch", "options": options, "responses": [created],
         "payload": {"configuration": CRON, "enabled": False}, "methods": ["POST"]},
        {"leaf": "run-update", "options": ["--id=sch", "--disable"], "responses": [existing, updated],
         "payload": {"configuration": CRON, "enabled": False}, "methods": ["GET", "PATCH"]},
        {"leaf": "run-rm", "options": ["--id=sch", "--force"],
         "responses": [{"status": 200, "body": None}], "methods": ["DELETE"]},
    ]
    rows = [
        dict(copy.deepcopy(case), name=f"{resource}/{case['leaf']}", resource=resource)
        for resource in RESOURCES for case in templates
    ]
    for terminal in ("Completed", "Cancelled"):
        rows.append({
            "name": f"cancel-wait/{terminal}", "leaf": "run-cancel", "options": ["--id=run", "-w"],
            "responses": [
                {"status": 202, "body": None, "headers": {"Retry-After": "2"}},
                {"status": 200, "body": {"id": "run", "status": "InProgress"}},
                {"status": 200, "body": {"id": "run", "status": terminal}},
            ], "waits": [2, 10], "methods": ["POST", "GET", "GET"],
        })
    for kind in ("daily", "weekly"):
        config = {
            "type": kind.title(), "times": ["10:00", "16:00"],
            "startDateTime": START, "endDateTime": END, "localTimeZoneId": "UTC",
        }
        selected = [f"--type={kind}", "--interval=10:00,16:00", f"--start={START}", f"--end={END}", "--enable"]
        if kind == "weekly":
            selected.append("--days=Monday,Friday")
            config["weekdays"] = ["Monday", "Friday"]
        rows.append({
            "name": f"create/{kind}", "leaf": "run-sch", "options": selected,
            "responses": [created], "payload": {"configuration": config, "enabled": True},
            "methods": ["POST"],
        })
    full = {"configuration": CRON, "enabled": "source value"}
    for file in (False, True):
        rows.append({
            "name": f"create/full/file={file}", "leaf": "run-sch",
            "options": ["--input", "schedule.json" if file else json.dumps(full)],
            "files": {"schedule.json": full}, "responses": [created],
            "payload": full, "methods": ["POST"],
        })
    for name, selected, payload in [
        ("enable", ["--enable"], {"configuration": CRON, "enabled": True}),
        ("input-preserves-state", ["--input", '{"type":"Daily","times":["10:00"]}'],
         {"configuration": {"type": "Daily", "times": ["10:00"]}, "enabled": True}),
        ("full-input", ["--input", json.dumps(full)], full),
        ("fields-preserve-state", options, {"configuration": CRON, "enabled": True}),
    ]:
        rows.append({
            "name": f"update/{name}", "leaf": "run-update", "options": ["--id=sch", *selected],
            "responses": [existing, updated], "payload": payload, "methods": ["GET", "PATCH"],
        })
    for confirmed in (False, True):
        rows.append({
            "name": f"remove/confirmed={confirmed}", "leaf": "run-rm", "options": ["--id=sch"],
            "confirmed": confirmed, "confirmations": 1,
            "responses": [{"status": 200, "body": None}] if confirmed else [],
            "methods": ["DELETE"] if confirmed else [],
        })
    return rows


def check(probe: Path | None) -> None:
    """Freeze source observations before native execution and compare every effect."""
    expectations = []
    inputs = []
    for case in cases():
        for output_format in ("json", "text"):
            expectations.append((f"{case['name']}/{output_format}", source_observation(case, output_format)))
            resource = case.get("resource", "Notebook")
            inputs.append({
                "command": ["job", case["leaf"], f"Demo.Workspace/Book.{resource}", *case["options"], "--output_format", output_format],
                "responses": [*discovery(resource), *case["responses"]],
                "files": case.get("files", {}), "confirmed": case.get("confirmed", False),
                "observe_confirmation": True,
            })
    frozen = copy.deepcopy(expectations)
    assert len(frozen) == 64
    if probe:
        environment = {key: value for key, value in os.environ.items() if key.upper() in ("SYSTEMROOT", "WINDIR", "TEMP", "TMP")}
        result = subprocess.run(
            [str(probe.resolve(strict=True))], input="".join(json.dumps(case) + "\n" for case in inputs),
            text=True, encoding="utf-8", capture_output=True, check=True, timeout=60, env=environment,
        )
        assert result.stderr == "", result.stderr
        rows = [json.loads(line) for line in result.stdout.splitlines()]
        assert len(rows) == len(frozen)
        for row, (name, expected) in zip(rows, frozen):
            assert row == expected, (name, row, expected)
    assert expectations == frozen
    print(f"PASS {len(frozen)} exact {'native ' if probe else 'pinned Python '}job cancellation/schedule compositions")


if __name__ == "__main__":
    arguments = argparse.ArgumentParser(description=__doc__)
    arguments.add_argument("--probe", type=Path)
    check(arguments.parse_args().probe)

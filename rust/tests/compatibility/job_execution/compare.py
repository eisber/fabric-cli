# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Freeze pinned Python submission/run observations before the production native probe."""

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
sys.path.insert(0, str(HERE.parent / "job_reads"))
from compare import JOBS, TextReference, discovery  # noqa: E402

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
ORIGIN = "https://api.fabric.microsoft.com/v1"
RESOURCES = {
    "SparkJobDefinition": "sparkjob", "Notebook": "RunNotebook",
    "DataPipeline": "Pipeline", "Lakehouse": "TableMaintenance", "GraphModel": "refreshGraph",
}


class SourceParser(argparse.ArgumentParser):
    """Source parser registration with only its documentation metadata removed."""

    def __init__(self, *args: Any, **kwargs: Any) -> None:
        kwargs.pop("fab_examples", None)
        kwargs.pop("fab_learnmore", None)
        super().__init__(*args, **kwargs)
        self.add_argument("--output_format", choices=["json", "text"])
        self.add_argument("--skill")


class SourceItem(SimpleNamespace):
    """Explicit synthetic resolver result, not a live/cache lookup."""

    def check_command_support(self, _: Any) -> None:
        """Fixtures select only the five independently pinned supported job types."""

def positive_int(value: str) -> int:
    """Source registration's injected positive-integer validator."""
    number = int(value)
    if number <= 0:
        raise ValueError("positive integer required")
    return number


def source_observation(case: dict[str, Any], output_format: str) -> dict[str, Any]:
    """Execute the actual parser, wrapper, payload, API, polling and UI source."""
    reference = TextReference({"name": case["name"], "input": {}})
    ui = SimpleNamespace(**reference.reference.namespace)
    waits: list[int] = []
    requests: list[dict[str, Any]] = []
    file_reads: list[str] = []
    resource = case.get("resource", "Notebook")
    response_rows = iter(copy.deepcopy(case["responses"]))
    constants = SimpleNamespace(
        COMMAND_JOBS_DESCRIPTION="Jobs", ERROR_NOT_RUNNABLE="NotRunnable",
        ERROR_INVALID_INPUT="InvalidInput", ERROR_JOB_FAILED="JobFailed",
        FAB_JOB_CANCEL_ONTIMEOUT="job_cancel_ontimeout",
    )
    common = {
        "Namespace": argparse.Namespace, "Any": Any, "Optional": Optional,
        "Item": SourceItem, "ApiResponse": Any, "json": json,
        "FabricCLIError": JOBS.SourceError, "FabricJobType": JOBS.FabricJobType,
        "fab_constant": constants,
    }

    def request(args: argparse.Namespace, **kwargs: Any) -> Any:
        body = json.loads(kwargs["data"]) if "data" in kwargs else None
        requests.append({
            "method": args.method.upper(), "url": f"{ORIGIN}/{args.uri}", "body": body, "skill": None,
        })
        response = next(response_rows)
        return SimpleNamespace(
            status_code=response["status"], text=json.dumps(response["body"]),
            headers={
                **response.get("headers", {}),
                **{key.lower(): value for key, value in response.get("headers", {}).items()},
            },
        )

    def open_file(path: str, mode: str) -> io.StringIO:
        assert mode == "r"
        file_reads.append(path)
        return io.StringIO(json.dumps(case["files"][path]))

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
    handler = dict(
        common, jobs_api=SimpleNamespace(**api), utils_job=SimpleNamespace(**utils),
        fab_ui=ui, con=constants,
        config=SimpleNamespace(get_config=lambda _: "false" if case.get("cancel_on_timeout") is False else "true"),
    )
    JOBS.load_functions(ROOT / "src/fabric_cli/commands/jobs/fab_jobs_run.py", PIN, handler)
    item = SourceItem(
        name=f"Book.{resource}", path=f"/Demo.Workspace/Book.{resource}",
        id="n", type=resource, workspace=SimpleNamespace(id="w"),
        job_type=JOBS.FabricJobType(RESOURCES[resource]),
    )
    wrappers = dict(
        common, handle_exceptions=lambda: lambda function: function,
        set_command_context=lambda: lambda function: function,
        handle_context=SimpleNamespace(get_command_context=lambda _: item),
        Command=SimpleNamespace(JOB_RUN="job run"), utils_job=SimpleNamespace(**utils),
        fab_ui=ui, jobs_run=SimpleNamespace(**handler),
    )
    JOBS.load_functions(
        ROOT / "src/fabric_cli/commands/jobs/fab_jobs.py", PIN, wrappers, {"run_command"},
    )
    parser_namespace = dict(
        common, _SubParsersAction=Any, show_help=lambda _: None,
        utils_error_parser=SimpleNamespace(get_usage_prog=lambda _: "job"),
        lazy_command=lambda *_: None, _jobs_module_path="unused",
        validate_positive_int=positive_int,
    )
    JOBS.load_functions(
        ROOT / "src/fabric_cli/parsers/fab_jobs_parser.py", PIN, parser_namespace, {"register_parser"},
    )
    parser = SourceParser(prog="fab")
    parser_namespace["register_parser"](parser.add_subparsers(dest="command"))
    command = ["job", case["leaf"], f"Demo.Workspace/Book.{resource}", *case.get("options", []), "--output_format", output_format]
    args = parser.parse_args(command)
    args.command_path = f"job {case['leaf']}"
    wrappers["run_command"](args)
    assert next(response_rows, None) is None
    expected_body = case.get("execution_data")
    assert requests[0]["body"] == ({"executionData": expected_body} if "execution_data" in case else None), case["name"]
    assert waits == case["waits"], (case["name"], waits)
    discovery_requests = [
        {"method": "GET", "url": f"{ORIGIN}/workspaces", "body": None, "skill": None},
        {"method": "GET", "url": f"{ORIGIN}/workspaces/w/items", "body": None, "skill": None},
    ]
    return {
        "exit": 0, "stdout": reference.stdout.getvalue(), "stderr": reference.stderr.getvalue(),
        "requests": [*discovery_requests, *requests],
        "scopes": [["https://api.fabric.microsoft.com/.default"]] * (2 + len(requests)),
        "file_reads": file_reads, "waits": waits,
    }


def cases() -> list[dict[str, Any]]:
    """Hand-authored source-valid fixture inputs; expectations never come from Rust."""
    accepted = {
        "status": 202, "body": None,
        "headers": {"Location": f"{ORIGIN}/workspaces/w/items/n/jobs/instances/run", "Retry-After": "10"},
    }
    completed = {"status": 200, "body": {"id": "run", "status": "Completed"}}
    rows = []
    for resource in RESOURCES:
        for leaf in ("start", "run"):
            rows.append({
                "name": f"{resource}/{leaf}", "resource": resource, "leaf": leaf,
                "responses": [accepted] if leaf == "start" else [accepted, completed],
                "waits": [] if leaf == "start" else [10],
            })
    for leaf in ("start", "run"):
        for name, options, execution_data, files in [
            ("params-config", ["-P", "n:int=2,", "flag:bool=false", "-C", "{'driver':true}"],
             {"parameters": {"n": {"type": "int", "value": 2}, "flag": {"type": "bool", "value": False}}, "configuration": {"driver": True}}, {}),
            ("inline", ["-i", "{'null':null,'false':false,'values':[1,2]}"],
             {"null": None, "false": False, "values": [1, 2]}, {}),
            ("file", ["-i", "execution.json"], {"value": "selected", "empty": []},
             {"execution.json": {"value": "selected", "empty": []}}),
        ]:
            rows.append({
                "name": f"{leaf}/{name}", "leaf": leaf, "options": options,
                "execution_data": execution_data, "files": files,
                "responses": [accepted] if leaf == "start" else [accepted, completed],
                "waits": [] if leaf == "start" else [10],
            })
    rows.append({
        "name": "pipeline-params", "resource": "DataPipeline", "leaf": "run",
        "options": ["--params", "n:int=2,", "flag:bool=false", "--polling_interval=2"],
        "execution_data": {"parameters": {"n": 2, "flag": False}},
        "responses": [accepted, completed], "waits": [2],
    })
    for cancel in (False, True):
        rows.append({
            "name": f"timeout/cancel={cancel}", "leaf": "run",
            "options": ["--timeout=1"], "cancel_on_timeout": cancel,
            "responses": [accepted, {"status": 202, "body": None}] if cancel else [accepted],
            "waits": [1],
        })
    return rows


def check(probe: Path | None) -> None:
    """Compare exact source streams, requests, credential scopes, files and timers."""
    expectations = []
    inputs = []
    for case in cases():
        for output_format in ("json", "text"):
            expectations.append((f"{case['name']}/{output_format}", source_observation(case, output_format)))
            resource = case.get("resource", "Notebook")
            inputs.append({
                "command": ["job", case["leaf"], f"Demo.Workspace/Book.{resource}", *case.get("options", []), "--output_format", output_format],
                "responses": [*discovery(resource), *case["responses"]],
                "files": case.get("files", {}), "cancel_on_timeout": case.get("cancel_on_timeout", True),
            })
    frozen = copy.deepcopy(expectations)
    assert len(frozen) == 38
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
    print(f"PASS {len(frozen)} exact {'native ' if probe else 'pinned Python '}job start/run compositions")


if __name__ == "__main__":
    arguments = argparse.ArgumentParser(description=__doc__)
    arguments.add_argument("--probe", type=Path)
    check(arguments.parse_args().probe)

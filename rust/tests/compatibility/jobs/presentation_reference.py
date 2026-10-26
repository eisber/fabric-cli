# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Run pinned job/UI bodies and compare complete streams with the native consumer."""

import argparse
import importlib.util
import json
import subprocess
import sys
from collections import deque
from functools import wraps
from pathlib import Path
from types import SimpleNamespace
from typing import Any


ROOT = Path(__file__).resolve().parents[4]
DIRECTORY = Path(__file__).resolve().parent


def module(name: str, path: Path) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec and spec.loader
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


sys.path.insert(0, str(ROOT / "rust/tests/compatibility/output"))
text_source = module("job_text_source", ROOT / "rust/tests/compatibility/output/text_reference.py")
sys.path.insert(0, str(ROOT / "rust/crates/fabric-cli-mutations/tests"))
error_source = module("job_error_source", ROOT / "rust/crates/fabric-cli-mutations/tests/error_reference.py")


def observe(case: dict[str, Any]) -> dict[str, Any]:
    """Execute actual wrapper/helper/API/output source with synthetic collaborators."""
    reference = text_source.TextReference({
        **case, "name": case["name"], "input": {},
        "config": case.get("format", "text"),
    })
    ns = reference.reference.namespace
    const_names = {
        "ERROR_NOT_RUNNABLE", "ERROR_JOB_FAILED", "ERROR_INVALID_INPUT",
        "ERROR_UNSUPPORTED_COMMAND", "ERROR_NOT_SUPPORTED", "ERROR_UNAUTHORIZED",
        "EXIT_CODE_AUTHORIZATION_REQUIRED", "EXIT_CODE_ERROR", "FAB_JOB_CANCEL_ONTIMEOUT",
    }
    constants = error_source.constants(const_names)
    con = SimpleNamespace(**constants)
    ui = SimpleNamespace(**ns)
    ui.prompt_confirm = lambda: case.get("confirmed", False)
    logger = SimpleNamespace(log_warning=ui.print_warning, log_debug=lambda *a: None)
    polling = error_source.selected("utils/fab_http_polling_utils.py", {"get_polling_interval"},
        {"fab_logger": logger, "DEFAULT_POLLING_INTERVAL": 10})
    # Parse argument cardinality and defaults through the original registration.
    parser = argparse.ArgumentParser()
    register = error_source.selected("parsers/fab_jobs_parser.py", {"register_parser"}, {
        "fab_constant": SimpleNamespace(COMMAND_JOBS_DESCRIPTION="jobs"),
        "utils_error_parser": SimpleNamespace(get_usage_prog=lambda parser: parser.prog),
        "validate_positive_int": int, "lazy_command": lambda *a: None, "show_help":lambda *a:None,
        "_jobs_module_path":"fabric_cli.commands.jobs.fab_jobs",
    })["register_parser"]
    class SourceParsers(argparse._SubParsersAction):
        def add_parser(self, name: str, **kwargs: Any) -> Any:
            for key in ("fab_examples", "fab_learnmore"):
                kwargs.pop(key, None)
            return super().add_parser(name, **kwargs)
    class SourceParser(argparse.ArgumentParser):
        def __init__(self, **kwargs: Any) -> None:
            super().__init__(**kwargs)
            self.register("action", "parsers", SourceParsers)
        def add_subparsers(self, **kwargs: Any) -> Any:
            kwargs["parser_class"] = SourceParser
            return super().add_subparsers(**kwargs)
    parser = SourceParser()
    group = parser.add_subparsers()
    register(group)
    tokens = case["args"]
    # Global output format is supplied by the front door, not each source job parser.
    explicit = None
    if "--output_format" in tokens:
        at = tokens.index("--output_format")
        explicit = tokens[at + 1]
        tokens = tokens[:at] + tokens[at + 2:]
    args = parser.parse_args(["job", *tokens])
    args.command = "job"
    args.command_path = f"job {args.jobs_command}"
    args.output_format = explicit or case.get("format", "text")
    args.ws_id = "ws"
    args.item_id = "item"
    args.item = "item.DataPipeline"
    args.jobType = "Pipeline"
    replies = deque(case["replies"])
    calls = []
    exceptions = error_source.load("core/fab_exceptions.py", dict(json=json, html=error_source.html, re=error_source.re))
    common = error_source.load("errors/common.py", {})
    real_error = ns["FabricCLIError"]
    # Both separately loaded classes use the same exact source; namespace shares
    # the UI's class so the real decorator sees every actual job exception.
    exceptions.FabricCLIError = real_error
    api_types = error_source.load("client/fab_api_types.py", {
        "json": json, "CaseInsensitiveDict": error_source.Headers, "fab_constant": con,
        "FabricCLIError": real_error, "ErrorMessages": SimpleNamespace(Common=common.CommonErrors),
    })
    def request(arguments: argparse.Namespace, **kwargs: Any) -> Any:
        calls.append({"method": arguments.method, "uri": arguments.uri})
        reply = replies.popleft()
        if reply.get("accepted"):
            return api_types.ApiResponse(202, "", b"", error_source.Headers({
                "Location": "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances/run"}))
        status = reply["status"]
        if status in (401, 403, 404):
            raise real_error(
                {401: "Access is unauthorized",403: "Access is forbidden. You do not have permission to access this resource",404: "The requested resource could not be found"}[status],
                {401:"Unauthorized",403:"Forbidden",404:"NotFound"}[status])
        if status >= 400:
            raise ns["FabricAPIError"](json.dumps(reply.get("data")))
        text = json.dumps(reply["data"]) if "data" in reply else ""
        return api_types.ApiResponse(status, text, text.encode(), error_source.Headers(reply.get("headers", {})))
    api = error_source.selected("client/fab_api_jobs.py", {
        "run_on_demand_item_job","get_item_job_instance","cancel_item_job_instance",
        "list_item_schedules","list_item_runs","get_item_schedule","create_item_schedule",
        "update_item_schedule","remove_item_schedule",
    }, {"fabric_api": SimpleNamespace(do_request=request)})
    utils = error_source.selected("utils/fab_cmd_job_utils.py", {
        "build_config_from_args","_process_input","_process_params","_process_config",
        "_process_notebook_param","_process_pipeline_param","_process_schedule_args",
        "_extract_times","_extract_days","validate_timeout_polling_interval","wait_for_job_completion",
    }, {"json":json,"re":error_source.re,"fab_constant":con,"FabricCLIError":real_error,
        "FabricJobType":SimpleNamespace(PIPELINE="Pipeline",RUN_NOTEBOOK="RunNotebook"),
        "jobs_api":SimpleNamespace(**api),"fab_ui":ui,"fab_logger":logger,
        "ErrorMessages":SimpleNamespace(Common=common.CommonErrors),
        "Namespace":argparse.Namespace,"get_polling_interval":polling["get_polling_interval"],
        "time":SimpleNamespace(time=lambda:0,sleep=lambda _:None)})
    listing = error_source.selected("utils/fab_cmd_ls_utils.py",{"format_and_print_output"},
        {"utils_ui":ui})
    names = {
        "start":"fab_jobs_run.py","run":"fab_jobs_run.py","run-list":"fab_jobs_run_list.py",
        "run-status":"fab_jobs_run_status.py","run-cancel":"fab_jobs_run_cancel.py",
        "run-sch":"fab_jobs_run_sch.py","run-update":"fab_jobs_run_update.py","run-rm":"fab_jobs_run_rm.py",
    }
    command_ns = error_source.selected("commands/jobs/" + names[args.jobs_command], {"exec_command"}, {
        "json":json,"jobs_api":SimpleNamespace(**api),"utils_job":SimpleNamespace(**utils),
        "fab_ui":ui,"utils_ui":ui,"utils_ls":SimpleNamespace(**listing),"fab_constant":con,
        "FabricCLIError":real_error,"con":con,
        "config":SimpleNamespace(get_config=lambda _:"true" if case.get("cancel_on_timeout",True) else "false"),
    })
    context = SimpleNamespace(path="/demo.Workspace/item.DataPipeline",full_name="item.DataPipeline",job_type="Pipeline")
    def execute(arguments: argparse.Namespace) -> None:
        if args.jobs_command in ("start","run"):
            utils["validate_timeout_polling_interval"](args)
            utils["build_config_from_args"](args,context)
            args.wait = args.jobs_command=="run"
            ui.print_grey(("Running job (sync)" if args.wait else "Starting job (async)") + " for 'item.DataPipeline'...")
        elif args.jobs_command in ("run-sch","run-update"):
            utils["build_config_from_args"](args,context,schedule=True)
        command_ns["exec_command"](args,context)
    decorators = error_source.selected("core/fab_decorators.py",{"handle_exceptions"},{
        "wraps":wraps,"FabricCLIError":real_error,"fab_ui":ui,
        "ERROR_UNAUTHORIZED":"Unauthorized","EXIT_CODE_AUTHORIZATION_REQUIRED":4,"EXIT_CODE_ERROR":1,
    })
    result = decorators["handle_exceptions"]()(execute)(args)
    assert not replies, case["name"]
    return {"name":case["name"],"stdout":reference.stdout.getvalue(),"stderr":reference.stderr.getvalue(),
        "exit":result or 0,"requests":calls}


def check(probe: str | None) -> None:
    """Compare every complete native stream/request observation with unchanged source."""
    fixture = json.loads((DIRECTORY / "presentation_cases.json").read_text(encoding="utf-8"))
    expected = [observe(case) for case in fixture["cases"]]
    if probe:
        result = subprocess.run([probe], input="".join(json.dumps(case)+"\n" for case in fixture["cases"]),
            check=True,capture_output=True,text=True,encoding="utf-8")
        actual = [json.loads(line) for line in result.stdout.splitlines()]
        assert len(actual)==len(expected)
        for old,new in zip(expected,actual,strict=True):
            assert old==new, json.dumps({"python":old,"rust":new},ensure_ascii=True,indent=2)
    print(f"PASS {len(expected)} unchanged-source job presentation stream/request contracts")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--probe")
    check(parser.parse_args().probe)

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute unchanged pinned Python job functions against hand-authored expectations."""

import ast
import json
import re
import subprocess
import sys
from argparse import Namespace
from enum import Enum
from pathlib import Path
from types import ModuleType, SimpleNamespace
from typing import Any, Optional
from urllib.parse import urlencode


class SourceError(Exception):
    """Minimal source exception boundary, without importing product initialization."""

    def __init__(self, message: str, code: str) -> None:
        self.message = message
        self.status_code = code
        super().__init__(message)


class FabricJobType(Enum):
    """Pinned enum values used by the unchanged source functions."""

    SPARK_JOB = "sparkjob"
    RUN_NOTEBOOK = "RunNotebook"
    PIPELINE = "Pipeline"
    TABLE_MAINTENANCE = "TableMaintenance"
    REFRESH_GRAPH = "refreshGraph"


def load_functions(
    path: Path, pin: str, namespace: dict[str, Any], names: Optional[set[str]] = None,
    classes: Optional[set[str]] = None,
) -> None:
    """Require AST equality to the exact source pin, then execute only its functions."""
    root = Path(__file__).resolve().parents[4]
    relative = path.relative_to(root).as_posix()
    pinned = subprocess.run(
        ["git", "show", f"{pin}:{relative}"],
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    ).stdout
    tree = ast.parse(path.read_text(encoding="utf-8"))
    if ast.dump(tree, include_attributes=False) != ast.dump(
        ast.parse(pinned), include_attributes=False
    ):
        raise AssertionError(f"Source drift: {relative}")
    functions = [
        node for node in tree.body
        if (isinstance(node, ast.FunctionDef) and (names is None or node.name in names))
        or (isinstance(node, ast.ClassDef) and classes and node.name in classes)
    ]
    exec(compile(ast.Module(body=functions, type_ignores=[]), str(path), "exec"), namespace)


def check_reference() -> None:
    """Run source parameter, schedule, REST and execution-lifecycle contracts offline."""
    directory = Path(__file__).resolve().parent
    root = directory.parents[3]
    cases = json.loads((directory / "cases.json").read_text(encoding="utf-8"))
    pinned_types = subprocess.run(
        ["git", "show", f"{cases['python_source_commit']}:src/fabric_cli/core/fab_types.py"],
        cwd=root, check=True, capture_output=True, text=True, encoding="utf-8",
    ).stdout
    types_tree = ast.parse(pinned_types)
    enum_values = {
        node.name: {
            entry.targets[0].id: ast.literal_eval(entry.value)
            for entry in node.body if isinstance(entry, ast.Assign)
            and isinstance(entry.targets[0], ast.Name) and isinstance(entry.value, ast.Constant)
        }
        for node in types_tree.body if isinstance(node, ast.ClassDef)
        and node.name in ("ItemType", "FabricJobType")
    }
    job_map = next(
        node.value for node in types_tree.body
        if isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name)
        and node.target.id == "ITJobMap"
    )
    resources = {
        enum_values["ItemType"][key.attr]: enum_values["FabricJobType"][value.attr]
        for key, value in zip(job_map.keys, job_map.values)
    }
    assert resources == cases["resources"], "The pinned source resource support map changed"
    pinned_commands = subprocess.run(
        ["git", "show", f"{cases['python_source_commit']}:src/fabric_cli/core/fab_commands.py"],
        cwd=root, check=True, capture_output=True, text=True, encoding="utf-8",
    ).stdout
    command_class = next(
        node for node in ast.parse(pinned_commands).body
        if isinstance(node, ast.ClassDef) and node.name == "Command"
    )
    command_path = next(
        node for node in command_class.body
        if isinstance(node, ast.FunctionDef) and node.name == "get_command_path"
    )
    command_path.decorator_list = []
    command_namespace: dict[str, Any] = {}
    exec(compile(ast.Module(body=[command_path], type_ignores=[]), "pinned-command-path", "exec"), command_namespace)
    constants = SimpleNamespace(
        ERROR_NOT_RUNNABLE="NotRunnable",
        ERROR_JOB_FAILED="JobFailed",
        ERROR_INVALID_INPUT="InvalidInput",
    )
    events: list[Any] = []
    def capture_output(args: Namespace, **kwargs: Any) -> None:
        events.append(("output", dict(kwargs, _command=getattr(args, "command", None))))
    ui = SimpleNamespace(
        print_progress=lambda *a, **kw: events.append(("progress", a)),
        print_grey=lambda *a, **kw: events.append(("grey", a)),
        print_output_format=capture_output,
        print_entries_unix_style=lambda *a, **kw: events.append(("table", a)),
        print_warning=lambda *a, **kw: events.append(("warning", a)),
        prompt_confirm=lambda: False,
    )
    namespace: dict[str, Any] = {
        "Namespace": Namespace, "Optional": Optional, "Any": Any,
        "Item": Any, "ApiResponse": Any, "CaseInsensitiveDict": dict,
        "FabricCLIError": SourceError, "FabricJobType": FabricJobType,
        "fab_constant": constants, "json": json, "re": re, "fab_ui": ui,
        "fab_logger": SimpleNamespace(log_warning=lambda *a: events.append(("log", a))),
        "ErrorMessages": SimpleNamespace(Common=SimpleNamespace(
            job_instance_failed=lambda job_id: f"Job instance '{job_id}' Failed"
        )),
    }
    polling = {
        "Optional": Optional, "CaseInsensitiveDict": dict,
        "fab_logger": SimpleNamespace(log_debug=lambda *a: None),
        "DEFAULT_POLLING_INTERVAL": 10,
    }
    load_functions(
        root / "src/fabric_cli/utils/fab_http_polling_utils.py",
        cases["python_source_commit"], polling,
    )
    namespace["get_polling_interval"] = polling["get_polling_interval"]
    load_functions(
        root / "src/fabric_cli/utils/fab_cmd_job_utils.py",
        cases["python_source_commit"], namespace,
    )
    count = len(resources)
    for case in cases["execution"]:
        args = Namespace(**case["args"])
        item = SimpleNamespace(job_type=FabricJobType(case["job_type"]))
        try:
            namespace["build_config_from_args"](args, item)
            actual = json.loads(args.configuration) if hasattr(args, "configuration") else None
            assert "error" not in case and actual == case["expected"], case["name"]
        except SourceError as error:
            assert error.status_code == case.get("error"), case["name"]
        count += 1
    for case in cases["schedules"]:
        defaults = dict(enable=False, type=None, interval=None, start=None, end=None, days=None)
        if case.get("update"):
            defaults["disable"] = True
        defaults.update(case["args"])
        args = Namespace(**defaults)
        try:
            namespace["build_config_from_args"](args, SimpleNamespace(), schedule=True)
            actual = json.loads(args.configuration) if args.configuration else None
            assert "error" not in case and actual == case["expected"], case["name"]
        except SourceError as error:
            assert error.status_code == case.get("error"), case["name"]
        count += 1
    api: dict[str, Any] = {"Namespace": Namespace, "Optional": Optional, "Any": Any, "ApiResponse": Any}
    load_functions(
        root / "src/fabric_cli/client/fab_api_jobs.py",
        cases["python_source_commit"], api,
    )
    for case in cases["wire"]:
        captured: list[Any] = []
        response = SimpleNamespace(
            status_code=case["status"],
            headers={"Location": "https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances/run"},
        )
        def request(args: Namespace, **kwargs: Any) -> Any:
            captured.append((args.method, args.uri, kwargs.get("data")))
            return response
        api["fabric_api"] = SimpleNamespace(do_request=request)
        args = Namespace(ws_id="ws", item_id="item", jobType="Pipeline", instance_id="run", schedule_id="sch")
        arguments = [args, case["payload"]] if "payload" in case else [args]
        result = api[case["function"]](*arguments)
        assert captured == [(case["method"], case["uri"], case.get("payload"))], case["function"]
        if case["function"] == "run_on_demand_item_job":
            assert result[1] == "run"
        count += 1
    for case in cases["wait"]:
        events.clear()
        waits: list[int] = []
        requests: list[str] = []
        statuses = iter(case["statuses"])
        namespace["time"] = SimpleNamespace(sleep=waits.append, time=lambda: 0)
        def get_status(args: Namespace) -> Any:
            requests.append(args.instance_id)
            return SimpleNamespace(status_code=200, text=json.dumps({"id": "run", "status": next(statuses)}),
                headers=case.get("response_headers", {}))
        namespace["jobs_api"] = SimpleNamespace(get_item_job_instance=get_status)
        try:
            namespace["wait_for_job_completion"](
                Namespace(command="job", output_format="json"), "run",
                SimpleNamespace(headers=case["headers"]), case.get("timeout"), case.get("custom"),
            )
            assert "error" not in case, case["name"]
        except SourceError as error:
            assert error.status_code == case.get("error"), case["name"]
        except TimeoutError:
            assert case.get("error") == "TimeoutError", case["name"]
        assert waits == case["waits"] and len(requests) == case["requests"], case["name"]
        output = [value["data"] for kind, value in events if kind == "output"]
        assert output == ([case["output"]] if "output" in case else []), case["name"]
        if "Failed" in case["statuses"] or "FutureStatus" in case["statuses"]:
            assert any(kind == "table" for kind, _ in events), case["name"]
        count += 1
    constants.ERROR_NOT_SUPPORTED = "NotSupported"
    constants.FAB_JOB_CANCEL_ONTIMEOUT = "job_cancel_ontimeout"
    listing: dict[str, Any] = {"utils_ui": ui}
    load_functions(
        root / "src/fabric_cli/utils/fab_cmd_ls_utils.py",
        cases["python_source_commit"], listing, {"format_and_print_output"},
    )
    modules = {
        "start": "fab_jobs_run.py", "run": "fab_jobs_run.py",
        "run-list": "fab_jobs_run_list.py", "run-cancel": "fab_jobs_run_cancel.py",
        "run-status": "fab_jobs_run_status.py", "run-sch": "fab_jobs_run_sch.py",
        "run-update": "fab_jobs_run_update.py", "run-rm": "fab_jobs_run_rm.py",
    }
    for case in [
        dict(case, _resource=resource, _job_type=job_type)
        for resource, job_type in resources.items() for case in cases["cli"]
    ]:
        events.clear()
        replies = iter(case["responses"])
        used: list[str] = []
        def request(args: Namespace, **kwargs: Any) -> Any:
            used.append(args.uri)
            reply = next(replies)
            return SimpleNamespace(
                status_code=reply["status"],
                text=json.dumps(reply["body"]) if "body" in reply else "",
                headers=reply.get("headers", {}),
            )
        api["fabric_api"] = SimpleNamespace(do_request=request)
        namespace["jobs_api"] = SimpleNamespace(**api)
        namespace["time"] = SimpleNamespace(sleep=lambda _: None, time=lambda: 0)
        defaults = dict(
            ws_id="ws", item_id="item", item="item", jobType=case["_job_type"],
            command="job", jobs_command=case["leaf"], output_format="json", wait=case["leaf"] == "run",
            schedule=False, force=False, id=None, schedule_id=None,
            enable=False, disable=True, type=None, interval=None, start=None,
            end=None, days=None, timeout=None, polling_interval=None,
        )
        defaults.update(case.get("args", {}))
        args = Namespace(**defaults)
        args.command_path = command_namespace["get_command_path"](args)
        ui.prompt_confirm = lambda: case.get("confirmed", False)
        module = {
            "Namespace": Namespace, "Item": Any, "json": json,
            "jobs_api": namespace["jobs_api"], "utils_job": SimpleNamespace(**namespace),
            "fab_ui": ui, "utils_ui": ui, "utils_ls": SimpleNamespace(**listing),
            "FabricCLIError": SourceError, "fab_constant": constants,
            "con": constants, "config": SimpleNamespace(
                get_config=lambda _: "true" if case.get("cancel_on_timeout", True) else "false"
            ),
        }
        load_functions(
            root / "src/fabric_cli/commands/jobs" / modules[case["leaf"]],
            cases["python_source_commit"], module,
        )
        context = SimpleNamespace(
            path=f"/demo.Workspace/item.{case['_resource']}",
            full_name=f"item.{case['_resource']}",
            job_type=FabricJobType(case["_job_type"]),
        )
        try:
            if case["leaf"] in ("start", "run"):
                namespace["build_config_from_args"](args, context)
            elif case["leaf"] in ("run-sch", "run-update"):
                namespace["build_config_from_args"](args, context, schedule=True)
            module["exec_command"](args, context)
            assert "error" not in case, case["name"]
        except SourceError as error:
            assert error.status_code == case.get("error"), case["name"]
        output = [
            {key: val for key, val in value.items() if key in ("message", "data")}
            for event, value in events if event == "output"
        ]
        assert output == ([case["output"]] if "output" in case else []), case["name"]
        commands = [value["_command"] for event, value in events if event == "output"]
        assert commands == ([case.get("output_command", "job")] if "output" in case else []), case["name"]
        assert any(event == "table" for event, _ in events) == case.get("table", False), case["name"]
        assert len(used) == len(case["responses"]), case["name"]
        count += 1
    paging: dict[str, Any] = {
        "json": json, "CaseInsensitiveDict": dict, "FabricCLIError": SourceError,
    }
    load_functions(
        root / "src/fabric_cli/client/fab_api_types.py",
        cases["python_source_commit"], paging, set(), {"ApiResponse"},
    )
    paging["Namespace"] = Namespace
    paging["fab_constant"] = SimpleNamespace(DEBUG=False)
    load_functions(
        root / "src/fabric_cli/client/fab_api_client.py",
        cases["python_source_commit"], paging, {"_handle_successful_response"},
    )
    for case in cases["paging"]:
        pages = iter(case["pages"])
        queries: list[str] = []
        def request(args: Namespace, continuation_token: Optional[str] = None) -> Any:
            page = next(pages)
            queries.append(urlencode({"continuationToken": continuation_token}) if continuation_token else "")
            body = json.dumps(page["body"])
            response = paging["ApiResponse"](200, body, body.encode(), page.get("headers", {}))
            return paging["_handle_successful_response"](args, response)
        paging["do_request"] = request
        result = request(Namespace()).json()
        assert result["value"] == case["expected"], case["name"]
        assert queries == case["queries"], case["name"]
        assert len(queries) == case["pages_received"], case["name"]
        assert result.get("total_pages") == case.get("source_total_pages"), case["name"]
        count += 1
    fake_modules = {
        name: ModuleType(name)
        for name in ("fabric_cli", "fabric_cli.core", "fabric_cli.core.fab_auth", "fabric_cli.core.fab_context")
    }
    fake_modules["fabric_cli.core.fab_auth"].FabAuth = lambda: SimpleNamespace(get_access_token=lambda _: "synthetic")
    fake_modules["fabric_cli.core.fab_context"].Context = lambda: SimpleNamespace(command="job", fabric_skill=None)
    old_modules = {name: sys.modules.get(name) for name in fake_modules}
    sys.modules.update(fake_modules)
    try:
        for case in cases["lro_routing"]:
            reconciled: list[Any] = []
            class RequestsError(Exception):
                pass
            raw = SimpleNamespace(status_code=202, headers=case["headers"], text="", content=b"")
            client: dict[str, Any] = {
                "ApiResponse": paging["ApiResponse"], "Namespace": Namespace,
                "requests": SimpleNamespace(compat=SimpleNamespace(urlencode=urlencode), RequestException=RequestsError),
                "time": SimpleNamespace(time=lambda: 0, sleep=lambda _: None),
                "fab_logger": SimpleNamespace(log_debug_http_request=lambda *a: None, log_debug_http_response=lambda *a: None, log_debug=lambda *a: None),
                "fab_constant": SimpleNamespace(
                    API_ENDPOINT_FABRIC="api.fabric.microsoft.com", API_VERSION_FABRIC="v1",
                    SCOPE_FABRIC_DEFAULT="fabric", SCOPE_AZURE_DEFAULT="azure",
                    SCOPE_ONELAKE_DEFAULT="storage",
                ),
                "_transform_workspace_url_for_private_link_if_needed": lambda url, uri, **kw: url,
                "_build_user_agent": lambda _: "offline",
                "_get_session": lambda: SimpleNamespace(request=lambda **kw: raw),
                "_handle_successful_response": lambda args, response: response,
                "FabricCLIError": SourceError, "urlparse": __import__("urllib.parse", fromlist=["urlparse"]).urlparse,
            }
            load_functions(
                root / "src/fabric_cli/client/fab_api_client.py",
                cases["python_source_commit"], client, {"do_request", "_handle_fab_long_running_op"},
            )
            def poll_operation(**kwargs: Any) -> Any:
                reconciled.append(kwargs)
                result = kwargs["original_response"]
                result.status_code = 200
                return result
            client["_poll_operation"] = poll_operation
            result = client["do_request"](Namespace(
                uri="workspaces/ws/items/item/jobs/instances/run/cancel", method="post", wait=case["wait"],
            ))
            assert bool(reconciled) == case["generic"], case["name"]
            assert result.status_code == (200 if case["generic"] else 202), case["name"]
            count += 1
    finally:
        for name, previous in old_modules.items():
            if previous is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = previous
    print(f"PASS {count} unchanged Python job contracts at {cases['python_source_commit']}")


if __name__ == "__main__":
    check_reference()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Additional unchanged-source branch expectations; never rewrites released cases."""

import json
import re
import ast
import subprocess
from argparse import Namespace
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Optional

from reference import FabricJobType, SourceError, load_functions


def check_branches() -> None:
    """Execute each additive expectation against unchanged pinned Python bodies."""
    directory = Path(__file__).resolve().parent
    root = directory.parents[3]
    cases = json.loads((directory / "branches.json").read_text(encoding="utf-8"))
    namespace = {
        "json": json, "re": re, "Namespace": Namespace, "Any": Any,
        "Optional": Optional, "Item": Any, "ApiResponse": Any,
        "CaseInsensitiveDict": dict, "FabricCLIError": SourceError,
        "FabricJobType": FabricJobType,
        "fab_constant": SimpleNamespace(ERROR_NOT_RUNNABLE="NotRunnable", ERROR_INVALID_INPUT="InvalidInput"),
    }
    load_functions(root / "src/fabric_cli/utils/fab_cmd_job_utils.py", cases["python_source_commit"], namespace)
    resources = json.loads((directory / "cases.json").read_text(encoding="utf-8"))["resources"]
    pinned_types=subprocess.run(["git","show",f"{cases['python_source_commit']}:src/fabric_cli/core/fab_types.py"],cwd=root,check=True,capture_output=True,text=True,encoding="utf-8").stdout
    item_enum=next(node for node in ast.parse(pinned_types).body if isinstance(node,ast.ClassDef) and node.name=="ItemType")
    all_types={ast.literal_eval(node.value) for node in item_enum.body if isinstance(node,ast.Assign) and isinstance(node.value,ast.Constant)}
    assert all_types-set(resources)==set(cases["rejected_resources"])
    assert len(all_types)==44
    checked = 0
    for section in ("execution", "schedules"):
        for case in cases[section]:
            defaults = dict(enable=False, type=None, interval=None, start=None, end=None, days=None)
            if case.get("update"):
                defaults["disable"] = True
            defaults.update(case["args"])
            args = Namespace(**defaults)
            item = SimpleNamespace(job_type=FabricJobType(resources[case.get("resource", "DataPipeline")]))
            try:
                namespace["build_config_from_args"](args, item, schedule=section == "schedules")
                actual = json.loads(args.configuration) if getattr(args, "configuration", None) else None
                assert "error" not in case and actual == case["expected"], (case["name"], actual)
            except SourceError as error:
                assert case["error"] == {"code": error.status_code, "message": error.message}, (case["name"], error.message)
            checked += 1
    yaml_source=subprocess.run(["git","show",f"{cases['python_source_commit']}:src/fabric_cli/core/fab_config/command_support.yaml"],cwd=root,check=True,capture_output=True,text=True,encoding="utf-8").stdout
    job_section=yaml_source.split("\n  job:",1)[1].split("\n  label:",1)[0]
    supported_names=[line.strip()[2:] for line in job_section.splitlines() if line.startswith("      - ")]
    supported={next(value for value in all_types if value.lower()==name.replace("_","").lower()) for name in supported_names}
    assert supported==set(resources)
    hierarchy_namespace:dict[str,Any]={
        "Command":Any,"FabricCLIError":SourceError,
        "fab_constant":SimpleNamespace(ERROR_UNSUPPORTED_COMMAND="UnsupportedCommand"),
        "ErrorMessages":SimpleNamespace(Hierarchy=SimpleNamespace(command_not_supported=lambda command:f"not supported for command '{command}'")),
        "cmd":SimpleNamespace(get_unsupported_items=lambda _:[],get_supported_items=lambda _:list(supported),get_supported_elements=lambda _:[]),
    }
    for class_name,relative,base in [
        ("FabricElement","src/fabric_cli/core/hiearchy/fab_element.py",None),
        ("_BaseItem","src/fabric_cli/core/hiearchy/fab_base_item.py","FabricElement"),
    ]:
        pinned=subprocess.run(["git","show",f"{cases['python_source_commit']}:{relative}"],cwd=root,check=True,capture_output=True,text=True,encoding="utf-8").stdout
        source_class=next(node for node in ast.parse(pinned).body if isinstance(node,ast.ClassDef) and node.name==class_name)
        method=next(node for node in source_class.body if isinstance(node,ast.FunctionDef) and node.name=="check_command_support")
        reduced=ast.ClassDef(name=class_name,bases=[ast.Name(id=base,ctx=ast.Load())] if base else [],keywords=[],body=[method],decorator_list=[])
        module=ast.fix_missing_locations(ast.Module(body=[reduced],type_ignores=[]))
        exec(compile(module,str(root/relative),"exec"),hierarchy_namespace)
    for item_type in all_types:
        for leaf in ("start","run","run-list","run-cancel","run-status","run-sch","run-update","run-rm"):
            item=hierarchy_namespace["_BaseItem"]()
            item.item_type=item_type
            item.type="Item"
            command=f"job {'run' if leaf=='start' else leaf}"
            try:
                assert item.check_command_support(SimpleNamespace(value=command)) is True
                assert item_type in supported
            except SourceError as error:
                assert item_type not in supported
                assert error.status_code=="UnsupportedCommand"
                assert error.message==f"not supported for command '{command}'"
            checked+=1
    polling = {
        "Optional": Optional, "CaseInsensitiveDict": dict,
        "fab_logger": SimpleNamespace(log_debug=lambda *a: None), "DEFAULT_POLLING_INTERVAL": 10,
    }
    load_functions(root / "src/fabric_cli/utils/fab_http_polling_utils.py", cases["python_source_commit"], polling)
    for case in cases["polling"]:
        assert polling["get_polling_interval"]({"retry-after": case["header"]}) == case["expected"]
        checked += 1
    class FakeItem:
        workspace = SimpleNamespace(id="ws")
        id = "item"
        type = "Item"
        name = "item.Notebook"
        job_type = FabricJobType.RUN_NOTEBOOK
        def check_command_support(self, command: str) -> None:
            assert command == "job run"
    wrapper = {
        "Namespace": Namespace, "Item": FakeItem,
        "handle_exceptions": lambda: (lambda fn: fn),
        "set_command_context": lambda: (lambda fn: fn),
        "handle_context": SimpleNamespace(get_command_context=lambda _: FakeItem()),
        "Command": SimpleNamespace(JOB_RUN="job run"), "utils_job": SimpleNamespace(**namespace),
    }
    load_functions(root / "src/fabric_cli/commands/jobs/fab_jobs.py", cases["python_source_commit"], wrapper, {"run_command"})
    for case in cases["wrapper_events"]:
        events = []
        captures = []
        wrapper["fab_ui"] = SimpleNamespace(print_grey=events.append)
        wrapper["jobs_run"] = SimpleNamespace(exec_command=lambda args, item: captures.append((args.item, args.wait)))
        wrapper["run_command"](Namespace(path=["item.Notebook"], jobs_command=case["leaf"]))
        assert events == [case["expected"]]
        assert captures == [(case["item"], case["wait"])]
        checked += 1
    update={"Namespace":Namespace,"Item":Any,"json":json,"FabricCLIError":SourceError,
        "fab_constant":namespace["fab_constant"],"fab_ui":SimpleNamespace(print_output_format=lambda *a,**kw:None)}
    load_functions(root/"src/fabric_cli/commands/jobs/fab_jobs_run_update.py",cases["python_source_commit"],update)
    for case in cases["schedule_updates"]:
        captured=[]
        def update_schedule(args:Namespace,payload:str)->Any:
            captured.append(json.loads(payload))
            return SimpleNamespace(status_code=200,text='{"id":"sch"}')
        update["jobs_api"]=SimpleNamespace(
            get_item_schedule=lambda args:SimpleNamespace(status_code=200,text=json.dumps(case["existing"])),
            update_item_schedule=update_schedule)
        defaults=dict(enable=False,disable=True,type=None,interval=None,start=None,end=None,days=None)
        defaults.update(case["args"])
        args=Namespace(**defaults)
        namespace["build_config_from_args"](args,SimpleNamespace(),schedule=True)
        update["exec_command"](args,SimpleNamespace())
        assert captured==[case["expected"]],case["name"]
        checked+=1
    print(f"PASS {checked} additional unchanged Python job branches")


if __name__ == "__main__":
    check_branches()

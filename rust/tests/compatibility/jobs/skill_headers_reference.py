# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute pinned request/header and job API bodies with explicit synthetic context."""

import importlib.util
import ast
import json
import sys
from argparse import Namespace
from pathlib import Path
from types import SimpleNamespace
from typing import Any


def check_reference() -> None:
    """Check source header truthiness, audience binding, request order and job routes."""
    root = Path(__file__).resolve().parents[4]
    released = root / "rust/crates/fabric-cli-mutations/tests"
    sys.path.insert(0, str(released))
    spec = importlib.util.spec_from_file_location("released_skill_source", released / "error_reference.py")
    assert spec and spec.loader
    source = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(source)
    cases = json.loads(Path(__file__).with_name("skill_headers.json").read_text(encoding="utf-8"))
    wire = json.loads(Path(__file__).with_name("cases.json").read_text(encoding="utf-8"))["wire"]
    assert cases["python_source_commit"] == source.PIN
    constants = source.constants({
        "SCOPE_FABRIC_DEFAULT", "SCOPE_AZURE_DEFAULT", "SCOPE_ONELAKE_DEFAULT",
        "API_ENDPOINT_FABRIC", "API_ENDPOINT_ONELAKE", "API_ENDPOINT_AZURE",
        "API_VERSION_FABRIC", "FABRIC_SKILL_HEADER",
    })
    powerbi=next(node for node in ast.parse(source.source("core/fab_constant.py")).body
        if isinstance(node,ast.Assign) and any(isinstance(target,ast.Name) and target.id=="API_ENDPOINT_POWER_BI" for target in node.targets))
    defaults={"validate_and_get_env_variable":lambda name,default:default}
    exec(compile(ast.Module(body=[powerbi],type_ignores=[]),"pinned-powerbi-default","exec"),defaults)
    constants["API_ENDPOINT_POWER_BI"]=defaults["API_ENDPOINT_POWER_BI"]
    assert constants["FABRIC_SKILL_HEADER"] == cases["header"]
    captured: list[dict[str, Any]] = []
    acquired: list[Any] = []
    context = SimpleNamespace(command="job start", fabric_skill=None)
    expected_status = 200

    def request(method: str, url: str, **kwargs: Any) -> Any:
        captured.append({"method": method, "url": url, "headers": kwargs["headers"], "data": kwargs.get("data")})
        return SimpleNamespace(status_code=expected_status, text='{"id":"synthetic"}', content=b'{}',
            headers=source.Headers({"Location":"https://api.fabric.microsoft.com/v1/workspaces/ws/items/item/jobs/instances/run"}))

    logger = SimpleNamespace(**{name: lambda *a: None for name in (
        "log_debug", "log_debug_http_request", "log_debug_http_response",
        "log_debug_http_request_exception",
    )})
    namespace = source.selected("client/fab_api_client.py", {"do_request"}, {
        "Namespace": Namespace, "fab_constant": SimpleNamespace(**constants),
        "requests": SimpleNamespace(compat=SimpleNamespace(urlencode=source.urlencode),RequestException=RuntimeError),
        "time": SimpleNamespace(time=lambda: 0),
        "fab_logger": logger,
        "_transform_workspace_url_for_private_link_if_needed": lambda url, uri, **kw: url,
        "_build_user_agent": lambda command: "source-test-agent",
        "_get_session": lambda: SimpleNamespace(request=request),
        "FabAuth": lambda: SimpleNamespace(get_access_token=lambda scope: acquired.append(scope) or "synthetic-only"),
        "FabContext": lambda: context,
        "ApiResponse": lambda **kwargs: SimpleNamespace(**kwargs),
        "_handle_successful_response": lambda args, response: response,
        "FabricCLIError": source.load("core/fab_exceptions.py",dict(json=json,html=source.html,re=source.re)).FabricCLIError,
    })
    api = source.selected("client/fab_api_jobs.py", {case["function"] for case in wire},
        {"fabric_api":SimpleNamespace(do_request=namespace["do_request"])})
    checked = 0
    for value in cases["values"]:
        context.fabric_skill = value["skill"]
        for case in wire:
            expected_status = case["status"]
            args = Namespace(ws_id="ws",item_id="item",jobType="Pipeline",instance_id="run",schedule_id="sch",wait=False)
            result = api[case["function"]](args, case["payload"]) if "payload" in case else api[case["function"]](args)
            actual = captured[-1]
            assert actual["headers"].get(cases["header"]) == value["expected"]
            assert actual["url"] == f"https://api.fabric.microsoft.com/v1/{case['uri']}"
            assert actual["method"] == case["method"]
            assert actual["data"] == case.get("payload")
            checked += 1
        expected_status = 200
        for uri in cases["followup_uris"]:
            namespace["do_request"](Namespace(uri=uri,method="get",wait=False))
            assert captured[-1]["headers"].get(cases["header"]) == value["expected"]
            checked += 1
    for case in cases["audiences"]:
        context.fabric_skill = "selected"
        namespace["do_request"](Namespace(uri="workspaces/ws",audience=case["audience"],method="get",wait=False))
        assert (cases["header"] in captured[-1]["headers"]) == case["expected"]
        checked += 1
    context.fabric_skill = "selected"
    namespace["do_request"](Namespace(uri="workspaces/ws",headers={cases["header"]:"caller-override"},wait=False))
    assert captured[-1]["headers"][cases["header"]] == "selected"
    checked += 1
    for actual in captured:
        assert actual["headers"]["Authorization"] == "Bearer synthetic-only"
        assert actual["headers"]["Content-Type"] == "application/json"
    assert len(acquired) == len(captured)
    print(f"PASS {checked} unchanged Python job skill-header contracts at {source.PIN}")


if __name__ == "__main__":
    check_reference()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Run actual pinned job/HTTP bodies with explicitly injected endpoint snapshots."""

import ast
import importlib.util
import json
import sys
from argparse import Namespace
from pathlib import Path
from types import SimpleNamespace
from typing import Any

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]


def check() -> None:
    """Prove selected hosts retain fixed Fabric scopes and explicit skill truthiness."""
    location = ROOT / "rust/crates/fabric-cli-mutations/tests"
    sys.path.insert(0, str(location))
    spec = importlib.util.spec_from_file_location("job_endpoint_source", location / "error_reference.py")
    assert spec and spec.loader
    source = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(source)
    manifest = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
    assert manifest["python_source_commit"] == source.PIN
    constants = source.constants({
        "SCOPE_FABRIC_DEFAULT", "SCOPE_AZURE_DEFAULT", "SCOPE_ONELAKE_DEFAULT",
        "API_ENDPOINT_FABRIC", "API_ENDPOINT_ONELAKE", "API_ENDPOINT_AZURE",
        "API_VERSION_FABRIC", "FABRIC_SKILL_HEADER",
    })
    powerbi = next(node for node in ast.parse(source.source("core/fab_constant.py")).body
        if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name)
        and target.id == "API_ENDPOINT_POWER_BI" for target in node.targets))
    defaults = {"validate_and_get_env_variable": lambda name, default: default}
    exec(compile(ast.Module(body=[powerbi], type_ignores=[]), "pinned-default", "exec"), defaults)
    constants["API_ENDPOINT_POWER_BI"] = defaults["API_ENDPOINT_POWER_BI"]
    constants = SimpleNamespace(**constants)
    context = SimpleNamespace(command="job", fabric_skill=None)
    requests, scopes = [], []
    status = 200
    host = "api.fabric.microsoft.com"

    def request(method: str, url: str, **kwargs: Any) -> Any:
        requests.append({
            "method": method, "url": url, "body": kwargs.get("data"),
            "skill": kwargs["headers"].get("x-ms-fabric-skill"),
        })
        return SimpleNamespace(status_code=status, text='{"id":"sch"}', content=b"{}",
            headers=source.Headers({
                "Location": f"https://{host}/v1/workspaces/ws/items/item/jobs/instances/run",
            }))

    logger = SimpleNamespace(**{name: lambda *args: None for name in (
        "log_debug", "log_debug_http_request", "log_debug_http_response",
        "log_debug_http_request_exception",
    )})
    client = source.selected("client/fab_api_client.py", {"do_request"}, {
        "Namespace": Namespace, "fab_constant": constants,
        "requests": SimpleNamespace(compat=SimpleNamespace(urlencode=source.urlencode), RequestException=RuntimeError),
        "time": SimpleNamespace(time=lambda: 0), "fab_logger": logger,
        "_transform_workspace_url_for_private_link_if_needed": lambda url, uri, **kw: url,
        "_build_user_agent": lambda command: "offline-source-agent",
        "_get_session": lambda: SimpleNamespace(request=request),
        "FabAuth": lambda: SimpleNamespace(get_access_token=lambda scope: scopes.append(scope) or "offline-only"),
        "FabContext": lambda: context, "ApiResponse": lambda **kwargs: SimpleNamespace(**kwargs),
        "_handle_successful_response": lambda args, response: response,
    })
    api = source.selected("client/fab_api_jobs.py", {row["function"] for row in manifest["wire"]},
                          {"fabric_api": SimpleNamespace(do_request=client["do_request"])})
    count = 0
    for host in ("api.fabric.microsoft.com", "dailyapi.fabric.microsoft.com", "msitapi.fabric.microsoft.com"):
        constants.API_ENDPOINT_FABRIC = host
        for job_type in manifest["resources"].values():
            for skill in (None, "", "caller-skill"):
                context.fabric_skill = skill
                for row in manifest["wire"]:
                    status = row["status"]
                    args = Namespace(ws_id="ws", item_id="item", jobType=job_type,
                                     instance_id="run", schedule_id="sch", wait=False)
                    if "payload" in row:
                        api[row["function"]](args, row["payload"])
                    else:
                        api[row["function"]](args)
                    expected = row["uri"].replace("jobType=Pipeline", f"jobType={job_type}")
                    expected = expected.replace("/Pipeline/", f"/{job_type}/")
                    assert requests[-1] == {
                        "method": row["method"], "url": f"https://{host}/v1/{expected}",
                        "body": row.get("payload"), "skill": skill or None,
                    }
                    assert scopes[-1] == ["https://api.fabric.microsoft.com/.default"]
                    count += 1
                status = 200
                for uri in (
                    "workspaces/ws/items/item/jobs/instances?continuationToken=next",
                    "operations/11111111-1111-1111-1111-111111111111",
                    "operations/11111111-1111-1111-1111-111111111111/result",
                ):
                    client["do_request"](Namespace(uri=uri, method="get", wait=False))
                    assert requests[-1]["url"] == f"https://{host}/v1/{uri}"
                    assert requests[-1]["skill"] == (skill or None)
                    assert scopes[-1] == ["https://api.fabric.microsoft.com/.default"]
                    count += 1
    assert len(requests) == len(scopes) == count
    print(f"PASS {count} actual-source job endpoint/scope/skill observations at {source.PIN}")


if __name__ == "__main__":
    check()

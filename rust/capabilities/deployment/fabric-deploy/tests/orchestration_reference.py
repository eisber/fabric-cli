# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Actual pinned deploy_with_config orchestration using synthetic capabilities."""

from __future__ import annotations

import argparse
import contextlib
import copy
import hashlib
import json
import logging
import subprocess
from enum import Enum
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from config_reference import BASE, PIN, load


def capture(root: Path) -> dict[str, Any]:
    """Run unchanged source orchestration; injected functions never perform I/O."""

    def git(*args: str) -> str:
        return subprocess.run(
            ["git", "-C", str(root), *args],
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout.strip()

    if git("rev-parse", "HEAD") != PIN:
        raise ValueError("Wrong pinned CICD source")
    source = root / "src" / "fabric_cicd"
    paths = [
        source / "constants.py",
        source / "_common" / "_config_utils.py",
        source / "publish.py",
    ]
    git("diff", "--exit-code", PIN, "--", *map(str, paths))
    values: dict[str, Any] = {"Enum": Enum}
    load(paths[0], {"FeatureFlag"}, values)
    namespace: dict[str, Any] = {
        "contextlib": contextlib,
        "logger": logging.getLogger("orchestration-reference"),
        "FeatureFlag": values["FeatureFlag"],
        "DeploymentStatus": SimpleNamespace(COMPLETED="completed", FAILED="failed"),
        "DeploymentResult": lambda **kwargs: SimpleNamespace(**kwargs),
        "constants": SimpleNamespace(FEATURE_FLAG={"original"}),
        "log_header": lambda *args: None,
    }
    load(
        paths[1],
        {
            "get_config_value",
            "update_setting",
            "extract_workspace_settings",
            "extract_publish_settings",
            "extract_unpublish_settings",
            "config_overrides_scope",
        },
        namespace,
    )
    load(paths[2], {"deploy_with_config", "_collect_responses"}, namespace)
    cases = []
    for name, config, fail in [
        ("both-skipped", {**BASE, "publish": {"skip": True}}, False),
        ("publish-only-no-parameter-autodiscovery", BASE, False),
        ("default-both-operations", {"core": BASE["core"]}, False),
        (
            "explicit-parameter",
            {
                "core": {**BASE["core"], "parameter": "explicit.yml"},
                "unpublish": {"skip": True},
            },
            False,
        ),
        (
            "publish-failure-restores-flags",
            {**BASE, "features": ["enable_response_collection"]},
            True,
        ),
    ]:
        calls = []

        def validate_environment(environment: str) -> str:
            calls.append(["validate_environment", environment])
            return environment

        def configuration(path: str, environment: str, overrides: Any) -> Any:
            calls.append(["load_config", path, environment, overrides])
            return copy.deepcopy(config)

        def workspace(**kwargs: Any) -> SimpleNamespace:
            kwargs.pop("token_credential")
            calls.append(["workspace", kwargs])
            return SimpleNamespace(responses=None, unpublish_responses=None)

        def publish(workspace: Any, **kwargs: Any) -> None:
            calls.append(["publish", kwargs])
            if fail:
                workspace.responses = {"Notebook": {"Book": {"status_code": 201}}}
                raise ValueError("synthetic publish failure")

        namespace.update(
            {
                "validate_environment": validate_environment,
                "load_config_file": configuration,
                "FabricWorkspace": workspace,
                "publish_all_items": publish,
                "unpublish_all_orphan_items": lambda workspace, **kwargs: calls.append(
                    ["unpublish", kwargs]
                ),
            }
        )
        try:
            result = namespace["deploy_with_config"](
                "config.yml",
                token_credential=object(),
                environment="dev",
                host_app="ms-fabric-cli/0.1.0",
            )
        except ValueError as error:
            result = error.deployment_result
        cases.append(
            {
                "id": name,
                "config": config,
                "failure": fail,
                "calls": calls,
                "result": vars(result),
                "finalFlags": sorted(namespace["constants"].FEATURE_FLAG),
            }
        )
    return {
        "source": f"microsoft/fabric-cicd@{PIN}",
        "hashes": {
            "\\".join(path.relative_to(source).parts): hashlib.sha256(
                path.read_bytes()
            ).hexdigest()
            for path in paths
        },
        "boundary": "Actual deploy_with_config/config extraction/scope cleanup with recording synthetic config loader, workspace constructor and publish/unpublish callbacks. No package/auth/network/local-repository initialization.",
        "cases": cases,
    }


def main() -> None:
    """Capture once before native expectations; otherwise check the immutable file."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--reference-root", type=Path, required=True)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    result = capture(args.reference_root)
    if args.capture:
        with args.capture.open("x", encoding="utf-8", newline="\n") as file:
            json.dump(result, file, ensure_ascii=True, indent=2)
            file.write("\n")
    elif result != json.loads(
        Path(__file__)
        .with_name("orchestration_source.json")
        .read_text(encoding="utf-8")
    ):
        raise AssertionError("Pinned orchestration differs from immutable receipt")
    print(
        json.dumps({"source": PIN, "cases": len(result["cases"]), "status": "passed"})
    )


if __name__ == "__main__":
    main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Check ignored configuration through the pinned deployment execution wrapper."""

from __future__ import annotations

import argparse
import ast
import contextlib
import copy
import hashlib
import json
import logging
from pathlib import Path
import subprocess
from types import SimpleNamespace
from typing import Any

PIN = "5ca096c00a9a878c646c59fe537b883eba951329"


def load(path: Path, names: set[str], namespace: dict[str, Any]) -> None:
    """Execute selected unchanged source definitions without package initialization."""
    nodes = [
        node
        for node in ast.parse(path.read_text(encoding="utf-8-sig")).body
        if isinstance(node, ast.FunctionDef) and node.name in names
    ]
    if {node.name for node in nodes} != names:
        raise AssertionError(f"Missing pinned definitions in {path.name}")
    future = ast.ImportFrom(
        module="__future__", names=[ast.alias(name="annotations")], level=0
    )
    module = ast.fix_missing_locations(ast.Module(body=[future, *nodes], type_ignores=[]))
    exec(compile(module, str(path), "exec"), namespace)


def capture(root: Path) -> dict[str, Any]:
    """Observe actual extractors, override scope and wrapper with injected boundaries."""
    head = subprocess.run(
        ["git", "-C", str(root), "rev-parse", "HEAD"],
        check=True, capture_output=True, text=True, timeout=10,
    ).stdout.strip()
    if head != PIN:
        raise AssertionError("Wrong immutable CICD source")
    source = root / "src" / "fabric_cicd"
    files = [source / "_common" / "_config_utils.py", source / "publish.py"]
    subprocess.run(
        ["git", "-C", str(root), "diff", "--exit-code", PIN, "--", *map(str, files)],
        check=True, capture_output=True, text=True, timeout=10,
    )
    logger = logging.getLogger("config-extensions-execution")
    logger.addHandler(logging.NullHandler())
    namespace: dict[str, Any] = {
        "logger": logger,
        "contextlib": contextlib,
        "constants": SimpleNamespace(FEATURE_FLAG=set()),
        "FeatureFlag": SimpleNamespace(
            ENABLE_RESPONSE_COLLECTION=SimpleNamespace(value="enable_response_collection")
        ),
        "DeploymentStatus": SimpleNamespace(COMPLETED="completed", FAILED="failed"),
        "DeploymentResult": lambda **values: SimpleNamespace(**values),
        "validate_environment": lambda value: value.strip(),
        "log_header": lambda *_args: None,
    }
    load(files[0], {
        "get_config_value", "update_setting", "extract_workspace_settings",
        "extract_publish_settings", "extract_unpublish_settings", "config_overrides_scope",
    }, namespace)
    load(files[1], {"deploy_with_config", "_collect_responses"}, namespace)
    base = {
        "core": {
            "workspace_id": "11111111-1111-1111-1111-111111111111",
            "repository_directory": "caller-repository",
            "item_types_in_scope": ["CopyJob"],
        },
        "publish": {},
        "unpublish": {},
    }
    cases = []
    for location in ("baseline", "top", "core", "publish", "unpublish", "all"):
        document = copy.deepcopy(base)
        if location in ("top", "all"):
            document["caller_metadata"] = {"retained": [False, 0, "source-ignored"]}
        for section in ("core", "publish", "unpublish"):
            if location in (section, "all"):
                document[section]["caller_metadata"] = {"retained": True}
        calls: list[dict[str, Any]] = []

        def loaded(*_args: Any) -> dict[str, Any]:
            return copy.deepcopy(document)

        def workspace(**values: Any) -> SimpleNamespace:
            values["token_credential"] = "caller-injected"
            calls.append({"stage": "workspace", "arguments": values})
            return SimpleNamespace()

        def publish(_workspace: Any, **values: Any) -> None:
            calls.append({"stage": "publish", "arguments": values})

        def unpublish(_workspace: Any, **values: Any) -> None:
            calls.append({"stage": "unpublish", "arguments": values})

        namespace.update(
            load_config_file=loaded, FabricWorkspace=workspace,
            publish_all_items=publish, unpublish_all_orphan_items=unpublish,
        )
        result = namespace["deploy_with_config"](
            "caller-config.yml", token_credential=object(), environment="test",
            host_app="source-oracle",
        )
        cases.append({
            "name": location, "document": document, "status": result.status,
            "message": result.message, "calls": calls,
        })
    for case in cases:
        if case["status"] != "completed" or case["calls"] != cases[0]["calls"]:
            raise AssertionError(f"Unknown configuration changed execution: {case['name']}")
    return {
        "source_commit": PIN,
        "source_hashes": {
            path.relative_to(source).as_posix(): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in files
        },
        "boundary": (
            "Actual deployment wrapper, setting extractors and override scope. "
            "Already-validated documents, environment label validation, constructor "
            "and publication are injected; no product initialization, credentials "
            "or service calls. Parser behavior is not used as execution proof."
        ),
        "cases": cases,
    }


def main() -> None:
    """Compare immutable receipts; capture only when explicitly creating new evidence."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference-root", required=True, type=Path)
    parser.add_argument("--capture", type=Path)
    args = parser.parse_args()
    observed = capture(args.reference_root)
    if args.capture:
        args.capture.write_text(
            json.dumps(observed, indent=2, ensure_ascii=True) + "\n", encoding="utf-8"
        )
    else:
        expected = json.loads(
            Path(__file__).with_name("config_extensions_execution_source.json").read_text(
                encoding="utf-8"
            )
        )
        if observed != expected:
            raise AssertionError("Pinned configuration execution changed")
    print(f"PASS {len(observed['cases'])} actual configuration execution cases at {PIN}")


if __name__ == "__main__":
    main()

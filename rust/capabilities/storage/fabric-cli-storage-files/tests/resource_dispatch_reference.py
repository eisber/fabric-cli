# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Check new negative-route fixtures against unchanged, git-pinned dispatchers.

This development-only oracle never imports product modules or performs local
transfers. Unexpected handler selection fails, including any local move route.
The same independently checked JSON is consumed by resource_command.rs.
"""

from __future__ import annotations

import argparse
import html
import json
from pathlib import Path
import re
import sys
from types import SimpleNamespace
from typing import Any

sys.path.insert(
    0,
    str(Path(__file__).resolve().parents[3] / "resources" / "fabric-cli-mutations" / "tests"),
)
from error_reference import constants
from python_reference import PIN, load


class Local:
    type = "LocalPath"
    path = "synthetic-local"


class OneLake:
    type = "OneLake"
    path = "synthetic-onelake"


class Item:
    type = "Item"
    path = "synthetic-item"


def unexpected(*_: Any) -> None:
    raise AssertionError("Rejected route must not select a transfer or delete handler")


def main() -> None:
    """Compare every synthetic rejection's exact source code and message."""
    exceptions = load("core/fab_exceptions.py", dict(json=json, html=html, re=re))
    values = SimpleNamespace(
        **constants({
            "ERROR_INVALID_INPUT",
            "ERROR_NOT_SUPPORTED",
            "WARNING_INVALID_PATHS",
            "WARNING_NOT_SUPPORTED_PATHS",
        })
    )
    collaborators = dict(
        Namespace=argparse.Namespace,
        fab_constant=values,
        FabricCLIError=exceptions.FabricCLIError,
        LocalPath=Local,
        OneLakeItem=OneLake,
        Item=Item,
        Workspace=type("Workspace", (), {}),
        Folder=type("Folder", (), {}),
        cp_onelake=SimpleNamespace(
            copy_local_2_onelake=unexpected,
            copy_onelake_2_local=unexpected,
            copy_onelake_2_onelake=unexpected,
        ),
        mv_onelake=SimpleNamespace(move_onelake_file=unexpected),
        fab_item_util=SimpleNamespace(item_sensitivity_label_warnings=unexpected),
    )
    handlers = {
        moving: load(
            f"commands/fs/fab_fs_{'mv' if moving else 'cp'}.py", collaborators
        ).exec_command
        for moving in (False, True)
    }
    fixtures = json.loads(Path(__file__).with_name("resource-routes.json").read_text())
    classes = {"local": Local, "onelake": OneLake, "item": Item}
    for case in fixtures:
        for flags in (False, True):
            args = argparse.Namespace(
                force=flags, recursive=flags, block_on_path_collision=flags
            )
            try:
                handlers[case["move"]](args, classes[case["from"]](), classes[case["to"]]())
            except exceptions.FabricCLIError as error:
                assert error.status_code == case["code"], case
                assert error.message == case["message"], case
            else:
                raise AssertionError(f"Original dispatcher accepted rejected fixture: {case}")
    print(f"{len(fixtures) * 2} pinned resource-route cases passed at {PIN}")


if __name__ == "__main__":
    main()

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Actual pinned custom parser classes, without product imports or ambient state."""

from __future__ import annotations

import argparse
import ast
import re
import sys
from types import SimpleNamespace
from typing import Any

from python_reference import PIN, load, source


def source_root_parser() -> argparse.ArgumentParser:
    """Execute the original custom parser/formatter and global flag definitions."""
    namespace: dict[str, Any] = dict(
        argparse=argparse,
        re=re,
        sys=sys,
        fab_global_params=load("parsers/fab_global_params.py", {"argparse": argparse}),
        fab_constant=SimpleNamespace(FAB_MODE_COMMANDLINE="commandline"),
    )
    tree = ast.parse(source("core/fab_parser_setup.py"))
    tree.body = [
        node
        for node in tree.body
        if isinstance(node, ast.ClassDef)
        and node.name in ("CustomHelpFormatter", "CustomArgumentParser")
    ]
    tree.body.insert(
        0,
        ast.ImportFrom(
            module="__future__", names=[ast.alias(name="annotations")], level=0
        ),
    )
    ast.fix_missing_locations(tree)
    exec(
        compile(tree, f"{PIN}:core/fab_parser_setup.py", "exec"),
        namespace,
    )
    return namespace["CustomArgumentParser"]()

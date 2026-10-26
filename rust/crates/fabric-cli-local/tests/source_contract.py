# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Run five local leaf expectations against pinned source, never ambient Fabric."""

from __future__ import annotations

import ast
from contextlib import redirect_stdout
from io import StringIO
import json
import os.path
from pathlib import Path
import subprocess
import sys
from tempfile import TemporaryDirectory
from types import ModuleType, SimpleNamespace
from unittest.mock import patch

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
ROOT = Path(__file__).resolve().parents[4]


def source(path: str) -> str:
    """Read an immutable Git object without importing installed product code."""
    return subprocess.check_output(
        ["git", "show", f"{PIN}:src/fabric_cli/{path}"], cwd=ROOT, text=True,
        encoding="utf-8",
    )


def module(name: str) -> ModuleType:
    """Install an isolated, explicit fake import boundary."""
    result = ModuleType(name)
    result.__path__ = []
    sys.modules[name] = result
    if "." in name:
        parent, leaf = name.rsplit(".", 1)
        setattr(sys.modules[parent], leaf, result)
    return result


def function(path: str, name: str, globals_: dict) -> object:
    """Compile the actual source function, not a rewritten approximation."""
    tree = ast.parse(source(path))
    node = next(node for node in ast.walk(tree) if isinstance(node, ast.FunctionDef) and node.name == name)
    exec(compile(ast.Module(body=[node], type_ignores=[]), path, "exec"), globals_)
    return globals_[name]


def main() -> None:
    """Verify source handlers and frozen expectations before native execution."""
    for package in (
        "fabric_cli", "fabric_cli.core", "fabric_cli.commands",
        "fabric_cli.commands.config", "fabric_cli.client", "fabric_cli.utils",
        "fabric_cli.errors",
    ):
        module(package)

    constants = module("fabric_cli.core.fab_constant")
    tree = ast.parse(source("core/fab_constant.py"))
    assignments = {
        node.targets[0].id: node for node in tree.body
        if isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Name)
    }
    config_names = ("FAB_CONFIG_KEYS_TO_VALID_VALUES", "CONFIG_DEFAULT_VALUES")
    names = {
        node.id for name in config_names for node in ast.walk(assignments[name])
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load)
    } | {"FAB_MODE", "FAB_MODE_INTERACTIVE", "FAB_MODE_COMMANDLINE", "ERROR_INVALID_INPUT", "ERROR_INVALID_PATH"}
    for name in names:
        setattr(constants, name, ast.literal_eval(assignments[name].value))
    for name in config_names:
        exec(compile(ast.Module(body=[assignments[name]], type_ignores=[]), "constants", "exec"), constants.__dict__)

    secure = module("fabric_cli.utils.fab_secure_io")
    secure.create_restricted_dir = lambda path: Path(path).mkdir(parents=True, exist_ok=True)
    secure.write_restricted_file = lambda path, content: Path(path).write_text(content, encoding="utf-8")
    errors = module("fabric_cli.core.fab_exceptions")
    errors.FabricCLIError = RuntimeError
    config_errors = {}
    exec(compile(source("errors/config.py"), "config_errors", "exec"), config_errors)
    sys.modules["fabric_cli.errors"].ErrorMessages = SimpleNamespace(Config=config_errors["ConfigErrors"])
    module("fabric_cli.client.fab_api_capacity")
    module("fabric_cli.utils.fab_util")

    events: list[tuple[str, object]] = []
    ui = module("fabric_cli.utils.fab_ui")
    ui.print_grey = lambda text: events.append(("stderr", text))
    ui.print_output_format = lambda args, **fields: events.append(("output", fields))
    ui.print_warning = lambda text: events.append(("warning", text))
    ls = module("fabric_cli.utils.fab_cmd_ls_utils")
    ls.format_and_print_output = lambda **fields: events.append(("rows", fields["data"]))

    with TemporaryDirectory(prefix="fabric-local-source-") as temporary:
        state = module("fabric_cli.core.fab_state_config")
        with patch.object(os.path, "expanduser", return_value=temporary):
            exec(compile(source("core/fab_state_config.py"), "state_config", "exec"), state.__dict__)
        state.init_defaults()
        expected = Path(__file__).with_name("fixtures").joinpath("defaults.json").read_text(encoding="utf-8").rstrip("\n")
        assert Path(state.config_file).read_text(encoding="utf-8") == expected

        for action in ("set", "get", "ls"):
            target = module(f"fabric_cli.commands.config.fab_config_{action}")
            exec(compile(source(f"commands/config/fab_config_{action}.py"), action, "exec"), target.__dict__)
        sys.modules["fabric_cli.commands.config.fab_config_set"].exec_command(SimpleNamespace(key="DEBUG_ENABLED", value=" 'true' "))
        assert events[-2:] == [
            ("stderr", "Updating 'debug_enabled' value..."),
            ("output", {"message": "Configuration 'debug_enabled' set to 'true'"}),
        ]
        sys.modules["fabric_cli.commands.config.fab_config_get"].exec_command(SimpleNamespace(key="DEBUG_ENABLED"))
        assert events[-1] == ("output", {"data": "true"})
        sys.modules["fabric_cli.commands.config.fab_config_ls"].exec_command(SimpleNamespace())
        rows = events[-1][1]
        assert len(rows) == 18
        assert rows[0] == {"setting": "cache_enabled", "value": "true"}
        assert rows[-1] == {"setting": "check_cli_version_updates", "value": "true"}
        assert rows[5] == {"setting": "local_definition_labels", "value": ""}

        cache_names = (
            "_get_workspaces_from_cache", "_get_workspace_items_from_cache",
            "_get_workspace_folders_from_cache", "_get_spark_pools_from_cache",
            "_get_managed_identities_from_cache", "_get_managed_private_endpoints_from_cache",
            "_get_external_data_shares_for_item_from_cache",
        )
        caches = {name: SimpleNamespace(cache={"synthetic": 1}) for name in cache_names}
        caches["fab_logger"] = SimpleNamespace(log_debug=lambda text: None)
        memory = module("fabric_cli.utils.fab_mem_store")
        memory.clear_caches = function("utils/fab_mem_store.py", "clear_caches", caches)
        clear = module("fabric_cli.commands.config.fab_config_clear_cache")
        exec(compile(source("commands/config/fab_config_clear_cache.py"), "clear_cache", "exec"), clear.__dict__)
        clear.exec_command(SimpleNamespace())
        assert all(not caches[name].cache for name in cache_names)
        assert events[-1] == ("output", {"message": "Cleared the cache"})

        version = function("utils/fab_ui.py", "print_version", {"__version__": "1.8.0"})
        text = StringIO()
        with redirect_stdout(text):
            version()
        assert text.getvalue() == "fab version 1.8.0\nhttps://aka.ms/fabric-cli/release-notes\n"

    print(json.dumps({"source_commit": PIN, "successful_handler_leaves": 5, "ambient_fabric_imports": False}))


if __name__ == "__main__":
    main()

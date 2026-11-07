# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Six more source-handler checks with synthetic context, browser and terminal."""

from __future__ import annotations

import ast
from argparse import Namespace
from enum import Enum
from pathlib import Path
import json
from tempfile import TemporaryDirectory
from types import SimpleNamespace

from build_support_reference import snapshot
from source_contract import PIN, function, main as config_contract, source


def main() -> None:
    """Execute pinned source functions before Rust outputs are available."""
    config_contract()
    data = snapshot()
    here = Path(__file__).resolve().parent
    assert json.loads((here.parent / "src" / "support.json").read_text()) == data
    events = []
    ui = SimpleNamespace(
        print_output_format=lambda args, **fields: events.append(("output", fields)),
        print_grey=lambda text: events.append(("grey", text)),
        print=lambda text: events.append(("print", text + "\n")),
    )
    class Tenant:
        pass

    with TemporaryDirectory(prefix="fabric-local-navigation-source-") as directory:
        workspace = SimpleNamespace(id="workspace-id", name="W.Workspace", path="/W.Workspace")
        note = SimpleNamespace(id="note-id", name="Note.Notebook", path="/W.Workspace/Note.Notebook", workspace=workspace, item_type="Notebook")
        state = SimpleNamespace(context=None)
        globals_ = {
            "Namespace": Namespace, "FabricElement": object, "Tenant": Tenant,
            "Context": lambda: state, "utils_ui": ui,
        }
        change = function("commands/fs/fab_fs_cd.py", "_change_context", globals_)
        cd = function("commands/fs/fab_fs_cd.py", "exec_command", globals_)
        cd(Namespace(), workspace)
        assert state.context is workspace
        assert events[-1] == ("output", {"message": "Switched to 'W.Workspace'"})

        # The real source persistence function imports only the explicit secure-I/O
        # fake established by the first five-leaf contract.
        context_file = Path(directory) / "context-4242.json"
        save = function("core/fab_context.py", "_save_context_to_file", {"json": json, "utils_ui": ui})
        save(SimpleNamespace(_context=workspace, _context_file=str(context_file)))
        assert context_file.read_text() == '{"path": "/W.Workspace"}'
        change(note, Namespace())
        pwd = function("commands/fs/fab_fs.py", "_print_context", {
            "Context": lambda: state, "json": json, "os": SimpleNamespace(getcwd=lambda: directory), "utils_ui": ui,
        })
        pwd()
        assert events[-1] == ("grey", json.dumps({
            "name": "Note", "type": ".Notebook", "id": "note-id", "local_path": directory,
        }, indent=4))

        noop = lambda: (lambda func: func)
        clear = function("parsers/fab_fs_parser.py", "do_clear", {
            "Namespace": Namespace, "handle_exceptions": noop, "set_command_context": noop,
            "os": SimpleNamespace(name="posix", system=lambda command: events.append(("terminal", command))),
        })
        clear(Namespace())
        assert events[-1] == ("terminal", "clear")
        extension = function("parsers/fab_extension_parser.py", "show_help", {
            "Namespace": Namespace,
            "fab_logger": SimpleNamespace(log_warning=lambda text, command: events.append(("warning", text, command))),
            "fab_constant": SimpleNamespace(INFO_FEATURE_NOT_SUPPORTED="Feature is not supported"),
        })
        extension(Namespace(command_path="extension"))
        assert events[-1] == ("warning", "Feature is not supported", "extension")

        identity = SimpleNamespace(
            _get_auth_property=lambda key: "user",
            get_token_claims=lambda *args: {"tid": "tenant-id", "upn": "synthetic@example.invalid"},
        )
        globals_ = {
            "Namespace": Namespace, "Optional": __import__("typing").Optional,
            "Workspace": object, "Item": object, "fab_ui": ui, "COMMAND": "open",
            "FabAuth": lambda: identity,
            "webbrowser": SimpleNamespace(open_new=lambda url: events.append(("browser", url))),
            "fab_state_config": SimpleNamespace(get_config=lambda key: "fabric"),
            "fab_constant": SimpleNamespace(
                WEB_URI="https://app.fabric.microsoft.com/groups",
                FAB_DEFAULT_OPEN_EXPERIENCE="default_open_experience",
                FAB_DEFAULT_OPEN_EXPERIENCE_POWERBI="power-bi",
                FAB_DEFAULT_OPEN_EXPERIENCE_FABRIC="fabric-developer",
                IDENTITY_TYPE="identity_type", SCOPE_FABRIC_DEFAULT="offline",
            ),
            "uri_mapping": data["uris"],
        }
        for name in ("_get_ux_experience", "_add_claims_as_query_param", "_open_in_browser", "_open_item", "_open_workspace"):
            function("commands/fs/fab_fs_open.py", name, globals_)
        globals_["_open_item"](Namespace(output_format="json"), note)
        expected = "https://app.fabric.microsoft.com/groups/workspace-id/synapsenotebooks/note-id/?experience=fabric-developer"
        assert events[-3:] == [
            ("grey", "Opening 'Note.Notebook' in the web browser..."),
            ("output", {"message": expected}),
            ("browser", expected + "&upn=synthetic@example.invalid&ctid=tenant-id"),
        ]
        globals_["_open_workspace"](Namespace(output_format="json"), workspace)
        assert events[-1] == ("browser", "https://app.fabric.microsoft.com/groups/workspace-id/list?experience=fabric-developer&upn=synthetic@example.invalid&ctid=tenant-id")

        command_node = next(node for node in ast.parse(source("core/fab_commands.py")).body if isinstance(node, ast.ClassDef) and node.name == "Command")
        command_node.body = [node for node in command_node.body if isinstance(node, ast.Assign)]
        namespace = {"Enum": Enum, "platform": SimpleNamespace(system=lambda: "Linux")}
        exec(compile(ast.Module(body=[command_node], type_ignores=[]), "source_commands", "exec"), namespace)
        commands = namespace["Command"]
        policies = {entry["command"]: entry for entry in data["policies"]}
        cmd = SimpleNamespace(**{
            f"get_{field}": (lambda command, field=field: policies[command.value][field])
            for field in ("supported_elements", "supported_items", "unsupported_items")
        })
        namespace.update({
            "cmd": cmd, "FabricCLIError": RuntimeError, "Command": commands,
            "ErrorMessages": SimpleNamespace(Hierarchy=SimpleNamespace(command_not_supported=lambda command: f"not supported for command '{command}'")),
            "fab_constant": SimpleNamespace(ERROR_UNSUPPORTED_COMMAND="UnsupportedCommand"),
        })
        for filename, classname, base in (
            ("fab_element.py", "FabricElement", None), ("fab_base_item.py", "_BaseItem", "FabricElement"),
        ):
            tree = ast.parse(source(f"core/hiearchy/{filename}"))
            node = next(node for node in tree.body if isinstance(node, ast.ClassDef) and node.name == classname)
            node.body = [next(member for member in node.body if isinstance(member, ast.FunctionDef) and member.name == "check_command_support")]
            node.bases = [ast.Name(id=base, ctx=ast.Load())] if base else []
            node.decorator_list = []
            exec(compile(ast.fix_missing_locations(ast.Module(body=[node], type_ignores=[])), filename, "exec"), namespace)
        model = namespace["_BaseItem"]()
        model.type = "Item"
        model.item_type = "Notebook"
        globals_ = {"FabricElement": object, "Any": object, "utils_ui": ui}
        available = function("commands/desc/fab_describe.py", "_get_available_commands", globals_)
        report = function("commands/desc/fab_describe.py", "_print_available_commands", globals_)
        report(".Notebook", available(model, [command for command in commands if command.value in policies]))
        assert events[-1] == ("print", json.loads((here / "fixtures" / "notebook-desc.json").read_text()))

    print(json.dumps({"source_commit": PIN, "successful_handler_leaves": 11, "real_browser_or_terminal_actions": False}))


if __name__ == "__main__":
    main()

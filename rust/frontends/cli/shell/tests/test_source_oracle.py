# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Check shell fixtures against extracted pinned Python methods, entirely offline."""

import ast
import json
from pathlib import Path
import shlex
import subprocess
from types import MethodType, SimpleNamespace
import unittest


ROOT = Path(__file__).resolve().parents[5]
CONTRACT = json.loads(Path(__file__).with_name("contract.json").read_text(encoding="utf-8"))
SOURCE_COMMIT = CONTRACT["source"]["commit"]


def pinned_source(path: str) -> str:
    """Read the exact locally available baseline without importing product code."""
    return subprocess.check_output(
        ["git", "-C", str(ROOT), "show", f"{SOURCE_COMMIT}:{path}"],
        text=True,
        encoding="utf-8",
    )


class SourceProbe:
    """Supply offline parser/UI/context fakes around the real source methods."""

    def __init__(self) -> None:
        self.action = "empty"
        self.tokens: list[str] = []
        self.messages: list[str] = []
        self.parse_error: BaseException | None = None
        self.unknown_commands: set[str] = set()
        self._is_running = False
        self.custom_style = None
        self.modes: list[str] = []
        self.session_options: dict[str, object] = {}
        constants_tree = ast.parse(
            pinned_source("src/fabric_cli/core/fab_constant.py")
        )
        values = {}
        for node in constants_tree.body:
            if isinstance(node, ast.Assign):
                for target in node.targets:
                    if isinstance(target, ast.Name) and target.id.startswith(
                        ("INTERACTIVE_", "FAB_MODE_")
                    ):
                        values[target.id] = ast.literal_eval(node.value)
        constants = SimpleNamespace(**values)
        probe = self

        class Choices(dict):
            def __contains__(self, _command: str) -> bool:
                return _command not in probe.unknown_commands

            def __getitem__(self, command: str) -> SimpleNamespace:
                def parse_args(rest: list[str]) -> SimpleNamespace:
                    if probe.parse_error:
                        raise probe.parse_error
                    probe.action = "command"
                    probe.tokens = [command, *rest]
                    return SimpleNamespace(func=lambda _args: None)

                return SimpleNamespace(parse_args=parse_args)

        self.subparsers = SimpleNamespace(choices=Choices())
        def root_parser_error(_message: str) -> None:
            raise SystemExit(2)

        self.parser = SimpleNamespace(
            set_mode=lambda _mode: None, error=root_parser_error
        )
        context = SimpleNamespace(
            context=SimpleNamespace(path="/"), set_runtime_mode=self.modes.append
        )

        def display(text: str) -> None:
            self.messages.append(text)
            self.action = (
                "exit" if text == constants.INTERACTIVE_EXIT_MESSAGE else "prefix-hint"
            )

        def help_display(*_args: object) -> None:
            self.action = "help"

        def version_display() -> None:
            self.action = "version"

        def prompt_session(**kwargs: object) -> object:
            self.session_options = kwargs
            return object()

        namespace = {
            "shlex": shlex,
            "fab_logger": SimpleNamespace(print_log_file_path=lambda: None),
            "fab_constant": constants,
            "utils_ui": SimpleNamespace(
                print=display, display_help=help_display, print_version=version_display
            ),
            "fab_commands": SimpleNamespace(COMMANDS={}),
            "Command": SimpleNamespace(get_command_path=lambda _args: "synthetic"),
            "Context": lambda: context,
            "HTML": lambda value: value,
            "html": __import__("html"),
            "CursorShape": SimpleNamespace(BLINKING_BEAM=None),
            "PromptSession": prompt_session,
            "InMemoryHistory": object,
        }
        tree = ast.parse(pinned_source("src/fabric_cli/core/fab_interactive.py"))
        methods = [
            method
            for node in tree.body
            if isinstance(node, ast.ClassDef) and node.name == "InteractiveCLI"
            for method in node.body
            if isinstance(method, ast.FunctionDef)
            and method.name in {"handle_command", "start_interactive", "init_session"}
        ]
        module = ast.Module(body=methods, type_ignores=[])
        exec(compile(module, "<pinned interactive methods>", "exec"), namespace)
        self.handle_command = MethodType(namespace["handle_command"], self)
        self.start_interactive = MethodType(namespace["start_interactive"], self)
        self.init_session = MethodType(namespace["init_session"], self)


class SourceOracleTests(unittest.TestCase):
    """Protect expectations using source execution, not a second Rust lexer."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.probe = SourceProbe()

    def test_lexical_and_raw_alias_cases(self) -> None:
        for case in CONTRACT["cases"]:
            with self.subTest(line=case["line"]):
                probe = self.probe
                probe.action = "empty"
                probe.tokens = []
                if "error" in case:
                    with self.assertRaisesRegex(ValueError, case["error"]):
                        probe.handle_command(case["line"])
                    continue
                self.assertEqual(
                    shlex.split(case["line"].strip()), case["tokens"]
                )
                probe.handle_command(case["line"])
                self.assertEqual(probe.action, case["action"])
                if probe.action == "command":
                    self.assertEqual(probe.tokens, case["tokens"])

    def test_source_catches_subparser_systemexit_but_not_runtime_errors(self) -> None:
        probe = self.probe
        try:
            probe.parse_error = SystemExit(2)
            self.assertIsNone(probe.handle_command("ls --synthetic-invalid"))
            probe.parse_error = RuntimeError("synthetic command failure")
            with self.assertRaisesRegex(RuntimeError, "synthetic command failure"):
                probe.handle_command("ls")
        finally:
            probe.parse_error = None

    def test_source_eof_inner_exception_quirk_terminates_without_farewell(self) -> None:
        probe = self.probe
        probe.messages.clear()
        calls = 0

        def prompt(*_args: object, **_kwargs: object) -> str:
            nonlocal calls
            calls += 1
            if calls > 1:
                raise AssertionError("EOF must not be retried")
            raise EOFError()

        probe.session = SimpleNamespace(prompt=prompt)
        probe.start_interactive()
        self.assertEqual(calls, 1)
        self.assertEqual(probe.messages[-1], "Error in interactive session: ")
        self.assertNotIn("Exiting interactive mode. Goodbye!", probe.messages)
        self.assertFalse(probe._is_running)

    def test_source_lexical_failure_ends_session_normally_after_diagnostic(self) -> None:
        for line, message in [
            ('get "unfinished', "No closing quotation"),
            ("get trailing\\", "No escaped character"),
        ]:
            with self.subTest(line=line):
                probe = self.probe
                probe.messages.clear()
                calls = 0

                def prompt(*_args: object, **_kwargs: object) -> str:
                    nonlocal calls
                    calls += 1
                    if calls > 1:
                        raise AssertionError("Lexical failure must not retry input")
                    return line

                probe.session = SimpleNamespace(prompt=prompt)
                self.assertIsNone(probe.start_interactive())
                self.assertEqual(calls, 1)
                self.assertEqual(
                    probe.messages[-1], f"Error in interactive session: {message}"
                )
                self.assertNotIn(
                    "Exiting interactive mode. Goodbye!", probe.messages
                )
                self.assertFalse(probe._is_running)

    def test_source_root_unknown_command_systemexit_is_not_caught(self) -> None:
        probe = self.probe
        try:
            probe.unknown_commands.add("synthetic-unknown")
            with self.assertRaises(SystemExit) as caught:
                probe.handle_command("synthetic-unknown")
            self.assertEqual(caught.exception.code, 2)
        finally:
            probe.unknown_commands.clear()

    def test_source_session_has_history_and_no_completer(self) -> None:
        history = object()
        self.probe.init_session(history)
        self.assertEqual(self.probe.session_options, {"history": history})

    def test_source_interrupt_prints_hint_and_continues_to_quit(self) -> None:
        probe = self.probe
        probe.messages.clear()
        calls = 0

        def prompt(*_args: object, **_kwargs: object) -> str:
            nonlocal calls
            calls += 1
            if calls == 1:
                raise KeyboardInterrupt()
            if calls == 2:
                return "quit"
            raise AssertionError("quit must not be retried")

        probe.session = SimpleNamespace(prompt=prompt)
        probe.start_interactive()
        self.assertEqual(calls, 2)
        self.assertEqual(
            probe.messages[-2:],
            [
                "\nUse 'quit' or 'exit' to leave interactive mode.",
                "Exiting interactive mode. Goodbye!",
            ],
        )
        self.assertFalse(probe._is_running)


if __name__ == "__main__":
    unittest.main()

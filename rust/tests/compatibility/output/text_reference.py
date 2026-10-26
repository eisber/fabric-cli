# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Execute actual text/table/UI source with private terminal/config/print collaborators."""

import argparse
import ast
import hashlib
import io
import json
import unicodedata
from html.parser import HTMLParser
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Optional

from reference import DIRECTORY, ROOT, Reference, definitions, fixtures


def error_definitions(namespace: dict[str, Any]) -> None:
    """Execute real, separately pinned error factories instead of hardcoded stubs."""
    sources = {
        "common.py": ("6fcfc00098d71517d8bb345015b48d7527544640", "CommonErrors"),
        "labels.py": ("3cb5b9c92122136d45378c314b74a55f8ea75fbb", "LabelsErrors"),
    }
    for name, (blob, symbol) in sources.items():
        raw = (ROOT / "src" / "fabric_cli" / "errors" / name).read_bytes()
        observed = hashlib.sha1(
            b"blob " + str(len(raw)).encode("ascii") + b"\0" + raw
        ).hexdigest()
        if observed != blob:
            raise AssertionError(f"{name}: pinned error source changed")
        nodes: list[ast.stmt] = [
            node
            for node in ast.parse(raw).body
            if isinstance(node, ast.ClassDef) and node.name == symbol
        ]
        if len(nodes) != 1:
            raise AssertionError(f"missing source class: {symbol}")
        exec(compile(ast.Module(body=nodes, type_ignores=[]), name, "exec"), namespace)
    namespace["ErrorMessages"] = SimpleNamespace(
        Common=namespace["CommonErrors"], Labels=namespace["LabelsErrors"]
    )


class Markup(HTMLParser):
    """Private headless toolkit collaborator, not a copy of source render logic."""

    def __init__(self, tty: bool) -> None:
        super().__init__(convert_charrefs=True)
        self.text = ""
        self.tty = tty
        self.names = {
            "ansigreen": "green",
            "ansired": "red",
            "ansiyellow": "yellow",
            "ansiblue": "blue",
            "grey": "grey",
        }

    def handle_starttag(self, tag: str, attrs: Any) -> None:
        if self.tty:
            self.text += f"<{self.names[tag]}>"

    def handle_endtag(self, tag: str) -> None:
        if self.tty:
            self.text += f"</{self.names[tag]}>"

    def handle_data(self, data: str) -> None:
        self.text += data


class TextReference:
    """One isolated source namespace per fixture, no real terminal or global patch."""

    def __init__(self, case: dict[str, Any]) -> None:
        self.case = case
        self.reference = Reference(fixtures()["timestamp"])
        ns = self.reference.namespace
        self.stdout = io.StringIO(newline="")
        self.stderr = io.StringIO(newline="")
        newline = "\r\n" if case.get("crlf") else "\n"
        tty = case.get("tty", False)

        def print_text(text: str, style: Any = None, file: Any = None) -> None:
            if case.get("fallback"):
                raise RuntimeError("controlled fallback")
            if tty and style == "fg:grey":
                text = f"<grey>{text}</grey>"
            if tty and style == "fg:#49C5B1":
                text = f"<fabric>{text}</fabric>"
            file.write(text + newline)

        def formatted(text: str, file: Any) -> None:
            if case.get("fallback"):
                raise RuntimeError("controlled formatted fallback")
            parser = Markup(tty)
            parser.feed(text)
            file.write(parser.text + newline)

        def builtin_print(text: str, file: Any = None) -> None:
            file.write(text + newline)

        original_import = ns["__builtins__"]["__import__"]

        def controlled_import(name: str, *args: Any, **kwargs: Any) -> Any:
            if name == "prompt_toolkit":
                return SimpleNamespace(
                    HTML=lambda text: text, print_formatted_text=formatted
                )
            return original_import(name, *args, **kwargs)

        ns["__builtins__"] = {**ns["__builtins__"], "__import__": controlled_import}
        ns["builtins"] = SimpleNamespace(print=builtin_print)
        ns["sys"] = SimpleNamespace(stdout=self.stdout, stderr=self.stderr)
        ns["unicodedata"] = unicodedata
        ns["shutil"] = SimpleNamespace(
            get_terminal_size=lambda fallback: SimpleNamespace(
                columns=case.get("width", fallback[0])
            )
        )
        ns["fab_lazy_load"] = SimpleNamespace(
            questionary=lambda: SimpleNamespace(print=print_text)
        )
        ns["fab_state_config"] = SimpleNamespace(
            get_config=lambda key: case.get("config", "text")
        )
        error_definitions(ns)
        definitions("core/fab_exceptions.py", {"OnelakeAPIError", "AzureAPIError"}, ns)
        definitions("utils/fab_util.py", {"truncate_columns"}, ns)
        ns["fab_util"] = SimpleNamespace(truncate_columns=ns["truncate_columns"])
        definitions(
            "utils/fab_ui.py",
            {
                "print_done",
                "print_warning",
                "print_info",
                "print_progress",
                "print_grey",
                "_safe_print_formatted_text",
                "_print_fallback",
                "_print_output_format_result_text",
                "_print_raw_data",
                "_print_dict",
                "_print_simple_items",
                "_print_error_format_text",
                "get_visual_length",
                "print_entries_unix_style",
                "_char_visual_width",
                "_get_visual_length",
                "_wrap_text",
                "_format_unix_style_field",
                "_format_unix_style_entry",
                "_print_entries_key_value_list_style",
                "_format_key_to_convert_to_title_case",
                "_check_special_cases",
                "print_version",
                "display_help",
                "print_fabric",
                "print",
            },
            ns,
        )

    def observe(self) -> dict[str, Any]:
        """Run a complete selected UI branch and return actual stream/error bytes."""
        case = self.case
        ns = self.reference.namespace
        values = case["input"]
        kind = case.get("kind", "render")
        exit_code = 0
        try:
            if kind == "render":
                command = values.get("command")
                args = argparse.Namespace(
                    command=command,
                    output_format=case.get("format", "text"),
                    **{f"{command}_subcommand": values.get("subcommand")},
                )
                ns["print_output_format"](
                    args,
                    data=values.get("data"),
                    message=values.get("message"),
                    hidden_data=values.get("hidden_data"),
                    show_headers=values.get("show_headers", False),
                    show_key_value_list=values.get("key_value", False),
                    truncate_columns=values.get("truncate_columns"),
                )
            elif kind == "table":
                ns["print_entries_unix_style"](
                    values.get("data"),
                    values["fields"],
                    values.get("show_headers", False),
                )
            elif kind in ("error", "fabric-error", "onelake-error", "azure-error"):
                source_type = {
                    "error": "FabricCLIError",
                    "fabric-error": "FabricAPIError",
                    "onelake-error": "OnelakeAPIError",
                    "azure-error": "AzureAPIError",
                }[kind]
                error = ns[source_type].__new__(ns[source_type])
                ns["FabricCLIError"].__init__(
                    error, values.get("message"), values.get("error_code")
                )
                error.request_id = values.get("request_id")
                error.timestamp = values.get("timestamp")
                error.more_details = values.get("details", [])
                if values.get("verbose"):
                    ns["_print_error_format_text"](
                        error.formatted_message(verbose=True), values.get("command")
                    )
                else:
                    ns["print_output_error"](error, values.get("command"), "text")
                exit_code = 4 if error.status_code == "Unauthorized" else 1
            elif kind == "key-values":
                ns["_print_entries_key_value_list_style"](values["data"])
            elif kind == "version":
                ns["__version__"] = values["version"]
                ns["print_version"]()
            elif kind == "help":
                ns["display_help"](values["categories"], values.get("header"))
            elif kind in ("plain", "grey", "fabric"):
                if kind == "grey":
                    ns["print_grey"](values["message"], case.get("to_stderr", False))
                else:
                    ns["print" if kind == "plain" else "print_fabric"](
                        values["message"]
                    )
            elif kind == "progress":
                ns["print_progress"](values["message"], values.get("progress"))
            elif kind in ("done", "warning", "info"):
                notice_args = [values["message"]]
                if kind != "done":
                    notice_args.append(values.get("command"))
                ns[f"print_{kind}"](*notice_args)
            else:
                raise AssertionError("unknown kind")
        except ns["FabricCLIError"] as error:
            return self.failure(error.message, error.status_code)
        except (ValueError, AttributeError) as error:
            return self.failure(str(error), "UnexpectedError")
        return {
            "name": case["name"],
            "stdout": self.stdout.getvalue(),
            "stderr": self.stderr.getvalue(),
            "exit": exit_code,
        }

    def failure(self, message: str, code: str) -> dict[str, Any]:
        """Retain actual source partial writes instead of hiding them on error."""
        observed = {"name": self.case["name"], "error": message, "code": code}
        if self.stdout.getvalue() or self.stderr.getvalue():
            observed["stdout"] = self.stdout.getvalue()
            observed["stderr"] = self.stderr.getvalue()
        return observed


def check(observations: Optional[list[dict[str, Any]]] = None) -> int:
    """Compare exact full stream/error/exit observations, not source algorithms."""
    cases = json.loads((DIRECTORY / "text-cases.json").read_bytes())
    if observations is not None and [x["name"] for x in observations] != [
        x["name"] for x in cases
    ]:
        raise AssertionError("missing, reordered or extra cases")
    for index, case in enumerate(cases):
        expected = TextReference(case).observe()
        if observations is not None and observations[index] != expected:
            raise AssertionError(
                f"{case['name']}: Rust/source mismatch\n{observations[index]!r}\n{expected!r}"
            )
        for key in ("stdout", "stderr", "exit", "error", "code"):
            if key in case and expected.get(key) != case[key]:
                raise AssertionError(
                    f"{case['name']}: handwritten {key} mismatch\n{expected!r}"
                )
    return len(cases)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--observed", type=Path)
    args = parser.parse_args()
    observed = json.loads(args.observed.read_bytes()) if args.observed else None
    print(f"PASS {check(observed)} real-source text/table cases")

# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Check new central test expectations against unchanged pinned output functions."""

from __future__ import annotations

import ast
import hashlib
import subprocess
from datetime import datetime, timezone
from enum import Enum
from types import SimpleNamespace
from typing import Any, Dict, List, Optional

from reference import ROOT, SOURCE_COMMIT, symbols


def main() -> None:
    """Prove status location, scalar wrapping and literal header spelling."""
    sources: dict[str, ast.Module] = {}
    for path, blob in {
        "core/fab_output.py": "4a6ea38ad912a2cce486568badf6b013329d7cb0",
        "utils/fab_ui.py": "1ff741b5838ca0c7d8c003fd60233804bd6df15f",
    }.items():
        raw = subprocess.check_output(
            ["git", "show", f"{SOURCE_COMMIT}:src/fabric_cli/{path}"], cwd=ROOT
        )
        actual = hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()
        assert actual == blob, path
        sources[path] = ast.parse(raw.decode("utf-8"), filename=path)
    env: dict[str, Any] = {
        "Any": Any,
        "Dict": Dict,
        "List": List,
        "Optional": Optional,
        "Enum": Enum,
        "datetime": datetime,
        "timezone": timezone,
        "fab_constant": SimpleNamespace(ERROR_UNEXPECTED_ERROR="UnexpectedError"),
    }
    output = symbols(
        sources, "core/fab_output.py", ["OutputStatus", "OutputResult", "FabricCLIOutput"], env
    )
    maintenance = output.FabricCLIOutput(
        command="table vacuum", data={"id": "run"}, message="Job instance 'run' completed"
    )._to_dict()
    assert maintenance["result"]["data"] == [{"id": "run"}]
    failure = output.FabricCLIOutput(
        command="table load", status=output.OutputStatus.Failure, error_code="Denied"
    )._to_dict()
    assert failure["status"] == "Failure"
    assert "status" not in failure["result"]
    lines: list[str] = []
    env.update(
        print_grey=lambda line, **kwargs: lines.append(line),
        shutil=SimpleNamespace(get_terminal_size=lambda fallback: SimpleNamespace(columns=80)),
        unicodedata=SimpleNamespace(east_asian_width=lambda char: "Na"),
    )
    ui = symbols(
        sources,
        "utils/fab_ui.py",
        [
            "print_entries_unix_style",
            "get_visual_length",
            "_char_visual_width",
            "_get_visual_length",
            "_wrap_text",
            "_format_unix_style_field",
            "_format_unix_style_entry",
        ],
        env,
    )
    ui.print_entries_unix_style(
        [{"name": "id", "type": "long", "nullable": False}],
        ["name", "type", "nullable"],
        header=True,
    )
    assert lines[0].split() == ["name", "type", "nullable"]
    assert lines[1] == "-" * len(lines[0])
    assert lines[2].split() == ["id", "long", "False"]
    print("TABLE central presentation: pinned status, list wrapping and literal headers passed")


if __name__ == "__main__":
    main()

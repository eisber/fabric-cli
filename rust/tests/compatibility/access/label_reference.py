# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Independent local-label and JSON-stream contracts using pinned Python source."""

import __future__
import argparse
import ast
import builtins
import json
import subprocess
from datetime import datetime, timezone
from enum import Enum
from io import StringIO
from types import SimpleNamespace
from unittest.mock import patch
from typing import Any

from reference import HERE, ROOT, SOURCE, SourceError, source_module


def output_source() -> dict:
    """Load actual output classes; inject time and all external boundaries."""
    raw = subprocess.run(
        ["git", "show", f"{SOURCE}:src/fabric_cli/core/fab_output.py"],
        cwd=ROOT, check=True, capture_output=True,
    ).stdout
    tree = ast.parse(raw.decode("utf-8"))
    tree.body = [node for node in tree.body if isinstance(node, ast.ClassDef)]
    namespace = {"Enum":Enum,"datetime":datetime,"timezone":timezone,"fab_constant":SimpleNamespace(ERROR_UNEXPECTED_ERROR="UnexpectedError")}
    exec(compile(tree,"fab_output.py","exec",flags=__future__.annotations.compiler_flag),namespace)
    return namespace


def main() -> None:
    """Validate seven local file contracts and initial independent output capture."""
    output = output_source()
    document = json.loads((HERE / "label_local.json").read_text(encoding="utf-8"))
    assert document["source_commit"] == SOURCE
    for case in document["cases"]:
        actual: list = []
        common = {
            "json":json, "FabricCLIError":SourceError,
            "fab_constant":SimpleNamespace(ERROR_INVALID_JSON="InvalidJson",ERROR_INVALID_PATH="InvalidPath",ERROR_INVALID_ENTRIES_FORMAT="InvalidEntriesFormat"),
            "ErrorMessages":SimpleNamespace(
                Common=SimpleNamespace(invalid_json_format=lambda:"Invalid JSON format",file_or_directory_not_exists=lambda:"The specified file or directory does not exist"),
                Labels=SimpleNamespace(invalid_entries_format=lambda:"The entries format in the labels definition file is invalid. Please check the file structure and ensure it follows the expected format")),
            "utils_ui":SimpleNamespace(print_output_format=lambda args,**kw:actual.append(kw["data"])),
        }
        handler = source_module("commands/labels/fab_labels_list_local.py",common)["exec_command"]
        error = None

        def fake_open(*args: Any, **kwargs: Any) -> StringIO:
            if case["input"] is None:
                raise FileNotFoundError("synthetic")
            return StringIO(case["input"])

        with patch.object(builtins,"open",fake_open):
            try:
                handler(argparse.Namespace(input="synthetic",output_format="json",command="label"))
            except SourceError as exception:
                error = {"code":exception.status_code,"message":exception.message}
        assert error == case["error"],case["name"]
        assert (actual[0] if actual else None) == case["data"],case["name"]
    streams = []
    access = json.loads((HERE / "cases.json").read_text(encoding="utf-8"))
    for case in access["cases"]:
        data = case["expected"]["data"]
        if data is None:
            stdout = ""
        else:
            envelope = output["FabricCLIOutput"](command="acl",data=data)
            envelope._timestamp = "2000-01-01T00:00:00Z"
            stdout = json.dumps(envelope._to_dict(),indent=4,ensure_ascii=False)+"\n"
        streams.append({"name":case["name"],"stdout":stdout,"stderr":"","exit":0})
    parser = argparse.ArgumentParser()
    parser.add_argument("--initial-capture",action="store_true")
    args = parser.parse_args()
    path = HERE / "streams.json"
    if args.initial_capture:
        if path.exists():
            raise SystemExit("Refusing to overwrite existing stream oracle")
        path.write_text(json.dumps(streams,indent=2)+"\n",encoding="utf-8")
    else:
        assert streams == json.loads(path.read_text(encoding="utf-8")),"Stream source expectations changed"
    print(f"local label source: {len(document['cases'])} cases; JSON streams: {len(streams)} cases passed")


if __name__ == "__main__":
    main()

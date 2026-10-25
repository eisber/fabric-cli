# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Run the pinned real CLI search wrapper with installed Python JMESPath.

The reference executes complete selected source definitions, never imports the
product package, and never generates expectations. All fixtures are synthetic.
"""

import argparse
import ast
import builtins
import copy
import hashlib
import importlib.util
import json
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Optional

import jmespath
import guards

DIRECTORY = Path(__file__).resolve().parent
ROOT = DIRECTORY.parents[3]
SOURCE_COMMIT = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"
BLOBS = {
    "utils/fab_jmespath.py": "9ba5ee2b2c59bb945606bd54f01554e399751d89",
    "utils/fab_util.py": "ce2c976d31b8d56996c2f0a7db9ab9abe7fcd145",
    "errors/common.py": "6fcfc00098d71517d8bb345015b48d7527544640",
    "utils/fab_cmd_ls_utils.py": "f843a06e03ecf6cf1892f79793d0b3132e5bdd22",
    "utils/fab_cmd_get_utils.py": "4d69ad98696e1e788c56ec52d9355d0308e53830",
    "commands/api/fab_api_request.py": "ee8b687e9c82caafa858a8d3f6f52e510f402d10",
    "commands/find/fab_find.py": "0f9bedccd64f09019f0ea8cdb40cad8c977afa54",
    "commands/acls/fab_acls_get.py": "12debf55c6549de0abbc71f48d303d7e975c3f06",
}


def source_tree(relative: str, raw: Optional[bytes] = None) -> ast.Module:
    """Verify the source blob before parsing any executable definitions."""
    if raw is None:
        raw = (ROOT / "src" / "fabric_cli" / relative).read_bytes()
    digest = hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw).hexdigest()
    if digest != BLOBS[relative]:
        raise AssertionError(f"source drift: {relative} != {SOURCE_COMMIT}")
    return ast.parse(raw, filename=relative)


def deny_import(name: str, *args: Any, **kwargs: Any) -> Any:
    """Source definitions have no permission to import collaborators."""
    raise AssertionError(f"unapproved source import: {name}")


def definitions(relative: str, names: set[str], namespace: dict[str, Any]) -> None:
    """Execute full original named definitions in a controlled namespace."""
    selected = [
        node for node in source_tree(relative).body
        if isinstance(node, (ast.FunctionDef, ast.ClassDef)) and node.name in names
    ]
    if {node.name for node in selected} != names:
        raise AssertionError(f"missing definitions: {relative}: {names}")
    exec(compile(ast.Module(body=selected, type_ignores=[]), relative, "exec"), namespace)


def load_output_reference() -> Any:
    """Reuse the unchanged source-executing output oracle, not a new envelope."""
    spec = importlib.util.spec_from_file_location(
        "query_output_reference", DIRECTORY.parent / "output" / "reference.py"
    )
    if spec is None or spec.loader is None:
        raise AssertionError("output reference unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


OUTPUT = load_output_reference()


class Reference:
    """Isolated real search, nargs, ls and get definitions with frozen output."""

    def __init__(self, timestamp: str) -> None:
        if jmespath.__version__ != "1.0.1":
            raise AssertionError(f"expected jmespath 1.0.1, got {jmespath.__version__}")
        for relative in BLOBS:
            source_tree(relative)
        self.output = OUTPUT.Reference(timestamp)
        ns: dict[str, Any] = {
            "__name__": "isolated_query_reference",
            "__builtins__": {**vars(builtins), "__import__": deny_import},
            "Any": Any, "Optional": Optional, "json": json, "jmespath": jmespath,
            "FabricCLIError": self.output.namespace["FabricCLIError"],
            "fab_constant": self.output.namespace["fab_constant"],
        }
        definitions("errors/common.py", {"CommonErrors"}, ns)
        ns["ErrorMessages"] = SimpleNamespace(Common=ns["CommonErrors"])
        definitions("utils/fab_jmespath.py", {"search", "_get_json_paths"}, ns)
        definitions("utils/fab_util.py", {"process_nargs"}, ns)
        self.namespace = ns

    def observe(self, case: dict[str, Any]) -> dict[str, Any]:
        """Observe exact trees/scalar strings and existing output bytes."""
        data = copy.deepcopy(case["data"])
        before = json.dumps(data, ensure_ascii=True)
        expression = case.get("expression")
        if "tokens" in case:
            expression = self.namespace["process_nargs"](case["tokens"])
        values: dict[str, Any] = {"command": "query"}
        try:
            result = (
                data if expression is None
                else self.namespace["search"](data, expression, case.get("deep", False))
            )
            observed = {"value": result}
            values["data"] = result
        except self.namespace["FabricCLIError"] as error:
            observed = {"error": error.status_code, "message": error.message}
            values.update(
                status="Failure", error_code=error.status_code, message=error.message
            )
        if json.dumps(data, ensure_ascii=True) != before:
            raise AssertionError(f"source mutated input: {case['name']}")
        output = self.output.observe({"name": case["name"], "input": values})
        return {
            "name": case["name"], "result": observed,
            "exit": output["exit"], "stdout": output["stdout"], "stderr": output["stderr"],
        }


def fixtures() -> dict[str, Any]:
    """Read frozen expectations without writing or regenerating anything."""
    result = json.loads((DIRECTORY / "cases.json").read_bytes())
    if result["source_commit"] != SOURCE_COMMIT:
        raise AssertionError("fixture source pin differs")
    return result


def exact(value: Any) -> str:
    """Include dictionary order, numeric type, strings and Unicode in equality."""
    return json.dumps(value, ensure_ascii=True, separators=(",", ":"))

def unsupported_observation(case: dict[str, Any], timestamp: str) -> dict[str, Any]:
    """Require explicit failure bytes for a declared gap; never count it as parity."""
    message = "Invalid jmespath query (https://jmespath.org)"
    failure = OUTPUT.Reference(timestamp).observe({
        "name": case["name"],
        "input": {"command": "query", "status": "Failure",
                  "error_code": "InvalidInput", "message": message},
    })
    return {
        "name": case["name"],
        "result": {"error": "InvalidInput", "message": message, "unsupported": True},
        "exit": failure["exit"], "stdout": failure["stdout"], "stderr": failure["stderr"],
    }


def compare(observations: list[dict[str, Any]]) -> list[str]:
    """Compare native observations against both real Python and frozen fixtures."""
    cases = fixtures()
    names = [case["name"] for case in cases["cases"]]
    if len(names) != len(set(names)):
        raise AssertionError("duplicate fixture names")
    if [item["name"] for item in observations] != names:
        raise AssertionError("native case inventory/order differs")
    failures = []
    for case, native in zip(cases["cases"], observations):
        python = Reference(cases["timestamp"]).observe(case)
        expected = (
            {"error": case["error"], "message": "Invalid jmespath query (https://jmespath.org)"}
            if "error" in case else {"value": case["expected"]}
        )
        if exact(python["result"]) != exact(expected):
            failures.append(f"{case['name']}: frozen expectation != real Python: {python['result']!r}")
        if case.get("support") == "unsupported":
            python = unsupported_observation(case, cases["timestamp"])
        if exact(native) != exact(python):
            failures.append(f"{case['name']}: native {native['result']!r} != Python {python['result']!r}")
    return failures


def main() -> None:
    """Compare a caller-provided native observation file; no subprocess launch."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--observations", type=Path, required=True)
    args = parser.parse_args()
    observations = json.loads(args.observations.read_bytes())
    with guards.offline():
        failures = compare(observations)
    if failures:
        raise AssertionError("\n".join(failures))
    unsupported = sum(case.get("support") == "unsupported" for case in fixtures()["cases"])
    print(json.dumps({
        "source_commit": SOURCE_COMMIT,
        "jmespath_version": jmespath.__version__,
        "native_exact": len(observations) - unsupported, "denominator": len(observations),
        "unsupported": unsupported,
    }))


if __name__ == "__main__":
    main()

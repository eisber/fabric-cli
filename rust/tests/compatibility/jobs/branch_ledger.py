# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Record exact pinned decisions and observed source lines, not a parity percentage."""

import argparse
import ast
import json
import subprocess
import sys
from pathlib import Path
from typing import Any

import branches_reference
import reference

PIN = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4"


def create_ledger(output: Path) -> None:
    """Write source decisions and measured execution without inferring full parity."""
    root = Path(__file__).resolve().parents[4]
    sources = [
        "src/fabric_cli/commands/jobs/fab_jobs.py",
        "src/fabric_cli/commands/jobs/fab_jobs_run.py",
        "src/fabric_cli/commands/jobs/fab_jobs_run_list.py",
        "src/fabric_cli/commands/jobs/fab_jobs_run_cancel.py",
        "src/fabric_cli/commands/jobs/fab_jobs_run_status.py",
        "src/fabric_cli/commands/jobs/fab_jobs_run_sch.py",
        "src/fabric_cli/commands/jobs/fab_jobs_run_update.py",
        "src/fabric_cli/commands/jobs/fab_jobs_run_rm.py",
        "src/fabric_cli/client/fab_api_jobs.py",
        "src/fabric_cli/utils/fab_cmd_job_utils.py",
        "src/fabric_cli/utils/fab_http_polling_utils.py",
    ]
    executed: dict[str, set[int]] = {}
    transitions: dict[str, set[tuple[int, int]]] = {}
    previous: dict[int, int] = {}

    def trace(frame: Any, event: str, arg: Any) -> Any:
        path = Path(frame.f_code.co_filename)
        try:
            relative = path.relative_to(root).as_posix()
        except ValueError:
            return None
        if relative not in sources:
            return None
        identity = id(frame)
        if event == "line":
            line = frame.f_lineno
            executed.setdefault(relative, set()).add(line)
            if identity in previous:
                transitions.setdefault(relative, set()).add((previous[identity], line))
            previous[identity] = line
        elif event in ("return", "exception"):
            previous.pop(identity, None)
        return trace

    sys.settrace(trace)
    try:
        reference.check_reference()
        branches_reference.check_branches()
    finally:
        sys.settrace(None)
    functions = []
    for relative in sources:
        source = subprocess.run(
            ["git", "show", f"{PIN}:{relative}"], cwd=root,
            check=True, capture_output=True, text=True, encoding="utf-8",
        ).stdout
        for function in ast.parse(source).body:
            if not isinstance(function, ast.FunctionDef):
                continue
            decisions = []
            for node in ast.walk(function):
                if isinstance(node, (ast.If, ast.While, ast.For, ast.Try, ast.Match)):
                    condition = getattr(node, "test", None) or getattr(node, "subject", None) or getattr(node, "iter", None)
                    body = getattr(node, "body", [])
                    otherwise = getattr(node, "orelse", [])
                    decisions.append({
                        "kind": type(node).__name__,
                        "line": node.lineno,
                        "end_line": node.end_lineno,
                        "condition": ast.unparse(condition) if condition else None,
                        "decision_line_observed": node.lineno in executed.get(relative, set()),
                        "body_entry_line_observed": bool(body and body[0].lineno in executed.get(relative, set())),
                        "else_entry_line_observed": bool(otherwise and otherwise[0].lineno in executed.get(relative, set())),
                        "observed_outgoing_source_line_arcs": sorted([
                            target for start, target in transitions.get(relative, set())
                            if start == node.lineno
                        ]),
                    })
            errors = [
                {"line": node.lineno, "expression": ast.unparse(node.exc)}
                for node in ast.walk(function) if isinstance(node, ast.Raise)
            ]
            functions.append({
                "file": relative, "function": function.name,
                "line": function.lineno, "end_line": function.end_lineno,
                "decisions": sorted(decisions, key=lambda node: node["line"]),
                "raises": errors,
                "executed_source_lines": sorted(
                    line for line in executed.get(relative, set())
                    if function.lineno <= line <= function.end_lineno
                ),
            })
    inventory = json.loads((root / "rust/tests/compatibility/commands/inventory.json").read_text(encoding="utf-8"))
    ledger = {
        "python_source_commit": PIN,
        "method": "Pinned AST decision/error inventory with measured lines/arcs from unchanged source suites. Line observation is NOT full branch or product parity.",
        "preserved_expectations": "cases.json and reference.py remain unchanged by the resumed increment",
        "leaves": [command for command in inventory["commands"] if command["command"].startswith("job ")],
        "resources": json.loads((Path(__file__).parent / "cases.json").read_text(encoding="utf-8"))["resources"],
        "functions": functions,
        "remaining_shared_gates": [
            "Lossless OperationError to shared resource error formatter conversion",
            "Skill header request plumbing and shared text renderer intent integration",
            "Shared JSON representation for arbitrary integers and non-finite numeric tokens",
            "Exact main-level unexpected-exception output normalization",
            "Documented typed response/origin/limit exceptions require separate review; source-line coverage does not waive them",
        ],
    }
    output.write_text(json.dumps(ledger, indent=2, ensure_ascii=True) + "\n", encoding="utf-8")
    print(f"Wrote {len(functions)} functions and {sum(len(item['decisions']) for item in functions)} source decisions to {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    create_ledger(parser.parse_args().output)

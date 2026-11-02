# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Source-derived branch/argument ledger, never a claim of complete CLI parity."""

import argparse
import ast
import hashlib
import json
import subprocess
from pathlib import Path

from reference import ROOT, SOURCE

HERE = Path(__file__).resolve().parent
FAMILIES = {
    "acl": ["acls/fab_acls.py", "acls/fab_acls_ls.py", "acls/fab_acls_get.py", "acls/fab_acls_set.py", "acls/fab_acls_rm.py"],
    "label": ["labels/fab_labels.py", "labels/fab_labels_list_local.py", "labels/fab_labels_set.py", "labels/fab_labels_rm.py"],
    "find": ["find/fab_find.py"],
    "api": ["api/fab_api.py", "api/fab_api_request.py"],
}
CLIENTS = [
    "fab_api_client.py", "fab_api_types.py", "fab_api_workspace.py", "fab_api_item.py",
    "fab_api_connection.py", "fab_api_gateway.py", "fab_api_onelake.py", "fab_api_labels.py",
    "fab_api_catalog.py", "fab_api_domain.py", "fab_api_sparkpool.py", "fab_api_managedprivateendpoint.py",
]


def source(path: str) -> bytes:
    """Read only the exact authority object, not merged/current Python files."""
    return subprocess.run(["git", "show", f"{SOURCE}:src/fabric_cli/{path}"],
        cwd=ROOT, check=True, capture_output=True).stdout


def branch(node: ast.AST) -> str:
    """Retain actual predicate/handler source, including failed/rejected branches."""
    if isinstance(node, (ast.If, ast.While, ast.IfExp, ast.Assert)):
        return ast.unparse(node.test)
    if isinstance(node, ast.For):
        return f"{ast.unparse(node.target)} in {ast.unparse(node.iter)}"
    if isinstance(node, ast.ExceptHandler):
        return ast.unparse(node.type) if node.type else "bare exception"
    if isinstance(node, ast.Match):
        return ast.unparse(node.subject)
    if isinstance(node, ast.Raise):
        return ast.unparse(node.exc) if node.exc else "re-raise"
    return ""


def document() -> dict:
    """Inventory all registered arguments plus whole-handler/client control flow."""
    inventory = json.loads((HERE.parent / "commands" / "inventory.json").read_text(encoding="utf-8"))
    leaves = [entry for entry in inventory["commands"] if entry["kind"] == "leaf"
        and entry["command"].split()[0] in FAMILIES]
    paths = [f"commands/{path}" for family in FAMILIES.values() for path in family]
    paths += [f"client/{path}" for path in CLIENTS]
    paths += ["utils/fab_cmd_label_utils.py", "utils/fab_cmd_ls_utils.py", "utils/fab_jmespath.py",
        "utils/fab_util.py", "utils/fab_storage.py", "core/fab_decorators.py"]
    entries = []
    for path in paths:
        raw = source(path)
        tree = ast.parse(raw.decode("utf-8"))
        functions = []
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                functions.append({"name": node.name, "line": node.lineno, "end_line": node.end_lineno,
                    "branches": [{"line": child.lineno, "kind": type(child).__name__, "predicate": branch(child)}
                        for child in ast.walk(node)
                        if isinstance(child, (ast.If, ast.For, ast.While, ast.IfExp, ast.ExceptHandler, ast.Match, ast.Raise, ast.Assert))]})
        entries.append({"path": f"src/fabric_cli/{path}",
            "sha256": hashlib.sha256(raw.replace(b"\r\n", b"\n")).hexdigest(), "functions": functions})
    support = source("core/fab_config/command_support.yaml").decode("utf-8")
    return {"source_commit": SOURCE, "leaves": leaves, "source_control_flow": entries,
        "source_support_matrix": support,
        "leaf_count": len(leaves),
        "modes": ["cli", "interactive", "batch/REPL front-door composition"],
        "status": "source branch denominator, not proof that every branch is implemented or tested",
        "independent_fixtures": ["cases.json", "label_local.json", "mutation_cases.json",
            "mutation_edges.json", "boundaries.json", "../catalog/cases.json",
            "../catalog/interactive_cases.json", "../raw_api/cases.json"]}


def main() -> None:
    """Freeze once and check future source inventory without regeneration."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--initial-capture", action="store_true")
    args = parser.parse_args()
    expected = document()
    path = HERE / "branch_ledger.json"
    if args.initial_capture:
        if path.exists():
            raise SystemExit("Refusing to overwrite branch ledger")
        path.write_text(json.dumps(expected, indent=2) + "\n", encoding="utf-8")
    else:
        assert expected == json.loads(path.read_text(encoding="utf-8")), "Source branch ledger drift"
    assert expected["leaf_count"] == 9
    print(f"source branch ledger: 9 leaves, {len(expected['source_control_flow'])} whole source files verified")


if __name__ == "__main__":
    main()

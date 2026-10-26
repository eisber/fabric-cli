# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Enumerate the pinned mutation source branches without inferring parity from names."""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import subprocess
from pathlib import Path
from typing import Any

from python_reference import PIN, ROOT, source

VERBS = ("mkdir", "rm", "mv", "cp", "set", "ln", "start", "stop", "assign", "unassign", "exists")
MANAGED = ("sparkpool", "managedidentity", "managedprivateendpoint", "domain", "connection", "gateway", "externaldatashare")
CONSUMERS = {
    "mkdir": ["offline_mutation", "creation_reference.py", "offline_onelake_write", "onelake_write_reference.py"],
    "rm": ["offline_mutation", "python_reference.py", "offline_onelake_write", "offline_shortcut"],
    "mv": ["offline_mutation", "python_reference.py", "offline_onelake_write"],
    "cp": ["offline_onelake_write", "onelake_write_reference.py"],
    "set": ["consumer.rs", "offline_shortcut", "shortcut_reference.py"],
    "ln": ["offline_shortcut", "shortcut_reference.py"],
    "start": ["offline_mutation", "python_reference.py"],
    "stop": ["offline_mutation", "python_reference.py"],
    "assign": ["offline_mutation", "python_reference.py"],
    "unassign": ["offline_mutation", "python_reference.py"],
    "exists": ["offline_mutation", "offline_onelake", "onelake_reference.py"],
}

VERIFIED_INCREMENTS = [
    {
        "id":"dependency-and-static-template-creation",
        "implementation_commit":"6151041f6e89d62e4443b5cd8121f0317a49c3c6",
        "source_functions":[
            ["commands/fs/mkdir/fab_fs_mkdir_item.py","exec"],
            ["utils/fab_cmd_mkdir_utils.py","add_type_specific_payload"],
            ["utils/fab_cmd_mkdir_utils.py","_create_payload"],
            ["utils/fab_cmd_mkdir_utils.py","_initialize_batch_collection_for_dependency_creation"],
        ],
        "verified_branches":[
            "KQL ReadWrite explicit Eventhouse and complete Shortcut payload",
            "KQL missing Eventhouse / unknown or empty dbType / incomplete Shortcut fallback creates Eventhouse",
            "DigitalTwinBuilderFlow explicit builder / missing or empty builder creates dependency",
            "SemanticModel raw definition parts and GenericMirror raw mirroring definition",
            "root versus nested Folder parent, inherited description and removed displayname/type params",
            "dependency200/201 metadata, missing ID, headerless202, root202 and failure after prior dependency",
            "source batch message/data/cache order, null parent ID and accepted-dependency single-name quirk",
        ],
        "verification_status":"implemented_and_tested_for_listed_cases_only",
        "public_consumer":"examples/offline_mutation.rs",
        "original_source_comparison":"tests/creation_reference.py",
        "original_case_count":82,
        "native_regressions":"tests/consumer.rs dependency/cache/acceptance/failure tests",
        "receipt":"validation-44.json",
        "remaining_function_branches":[
            "Report template plus SemanticModel auto creation",
            "dynamic MirroredDatabase source schemas/connections/tables",
            "MountedDataFactory escaped/Unicode/non-string definition values",
            "all create type/param help/support combinations and main namespace dispatch",
        ],
    },
]


def owner(path: str) -> tuple[str, list[str]]:
    """Keep actual helper ownership separate from original generic orchestration."""
    if "capacity" in path:
        return "capacity-1302759", ["Fabric/ARM capacity SDK/CLI and shared ARM operation policy"]
    if any(name in path for name in MANAGED):
        return "managed-resources-1302848", ["Managed-resource SDK/CLI committed public helper"]
    if path.endswith("fab_fs_cp_onelake.py"):
        return "mutations-1302844 + storage-files-1302849", ["Local filesystem branches owned by storage-files; remote DFS branch owned by mutations"]
    if "fab_fs_cp_" in path or "fab_fs_mv_item" in path:
        return "mutations-1302844", ["Generic definition transfer: deployment-1302708 (5215)", "Source workspace/item/folder bulk selection"]
    if "fab_fs_set_item" in path:
        return "mutations-1302844", ["Definition SET alias/payload codec: deployment-1302708", "Main context dispatch:1302707"]
    if "fab_fs_mkdir_item" in path:
        return "mutations-1302844", ["Source definition JSON encoding: output-1302761 (5274)", "Original template bytes", "Dependency/batch/cache evidence"]
    if "fab_fs_mkdir_utils" in path:
        return "mutations-1302844 + managed-resources-1302848", ["Per-resource helper ownership; source JSON encoder5274"]
    return "mutations-1302844", ["Main/parser/context front door:1302707/1302760"]


def family(path: str) -> str:
    """Select the source command family, not an invented namespace."""
    name = Path(path).stem
    for verb in VERBS:
        if name.startswith(f"fab_fs_{verb}") or f"/{verb}/" in path or name == f"fab_cmd_{verb}_utils":
            return verb
    return "shared"


def branch_description(node: ast.AST) -> str:
    """Record original branch conditions/loops verbatim as normalized source AST."""
    if isinstance(node, ast.If):
        return ast.unparse(node.test)
    if isinstance(node, ast.Match):
        return ast.unparse(node.subject)
    if isinstance(node, ast.match_case):
        return ast.unparse(node.pattern) + (f" if {ast.unparse(node.guard)}" if node.guard else "")
    if isinstance(node, (ast.For, ast.AsyncFor)):
        return f"{ast.unparse(node.target)} in {ast.unparse(node.iter)}"
    if isinstance(node, ast.While):
        return ast.unparse(node.test)
    if isinstance(node, ast.ExceptHandler):
        return ast.unparse(node.type) if node.type else "bare except"
    return type(node).__name__


def source_rows(path: str) -> list[dict[str, Any]]:
    """Enumerate every handler function and conditional/loop/failure branch."""
    text = source(path)
    tree = ast.parse(text)
    blob = hashlib.sha1(f"blob {len(text.encode('utf-8'))}\0".encode() + text.encode("utf-8")).hexdigest()
    selected_owner, deps = owner(path)
    verb = family(path)
    rows = []
    for function in ast.walk(tree):
        if not isinstance(function, (ast.FunctionDef, ast.AsyncFunctionDef)):
            continue
        branches: list[ast.AST] = [function]
        branches.extend(node for node in ast.walk(function) if isinstance(node, (
            ast.If, ast.Match, ast.match_case, ast.For, ast.AsyncFor, ast.While, ast.ExceptHandler,
        )))
        for index, branch in enumerate(branches):
            if isinstance(branch, ast.match_case):
                start = getattr(branch.pattern, "lineno", function.lineno)
                end = max(getattr(node, "end_lineno", start) for node in branch.body)
            else:
                start = getattr(branch, "lineno", function.lineno)
                end = getattr(branch, "end_lineno", function.end_lineno)
            rows.append({
                "id": f"{path}:{function.name}:{start}:{index}",
                "source": {"commit": PIN, "path": path, "blob": blob, "function": function.name,
                           "line_start": start, "line_end": end},
                "family": verb, "branch_kind": "function" if branch is function else type(branch).__name__,
                "condition": "function entry" if branch is function else branch_description(branch),
                "owner": selected_owner, "dependencies": deps,
                "implementation_status": "subset_implemented_not_full_branch_acceptance" if selected_owner.startswith("mutations") else "coordinated_owner",
                "verification_status": "requires_exact_row_reconciliation",
                "candidate_consumers": CONSUMERS.get(verb, ["creation_reference.py", "consumer.rs"]),
                "required_contracts": ["selected resource/options/mode", "actual request/response", "output/exit",
                                       "error/acceptance/cancellation", "partial/cache/cleanup where applicable"],
                "note": "Candidate consumers are evidence references, not a claim that every condition here is tested or complete.",
                "verified_increment_refs":[entry["id"] for entry in VERIFIED_INCREMENTS
                    if [path,function.name] in entry["source_functions"]],
            })
    return rows


def support_rows() -> list[dict[str, Any]]:
    """Read all pinned supported AND rejected element/type entries by indentation."""
    rows = []
    command = ""
    section = ""
    in_fs = False
    for line, raw in enumerate(source("core/fab_config/command_support.yaml").splitlines(), 1):
        stripped = raw.split("#", 1)[0].strip()
        depth = len(raw) - len(raw.lstrip())
        if depth == 2 and stripped.endswith(":"):
            in_fs = stripped == "fs:"
        if not in_fs:
            continue
        if depth == 6 and stripped.endswith(":"):
            command = stripped[:-1]
            section = ""
        elif depth == 8 and stripped.endswith(":"):
            section = stripped[:-1]
        elif depth == 10 and stripped.startswith("- ") and command in VERBS:
            rows.append({
                "source": {"commit": PIN, "path": "core/fab_config/command_support.yaml", "line": line},
                "command": command, "gate": section, "resource": stripped[2:].strip(),
                "required_expectation": "rejected" if section.startswith("unsupported") else "supported_dispatch",
                "verification_status": "requires_resource_policy_consumer_reconciliation",
            })
    if not rows or not any(row["required_expectation"] == "rejected" for row in rows):
        raise AssertionError("Pinned support/rejection policy was not fully inventoried")
    return rows


def main() -> None:
    """Persist a source-complete branch denominator with honest per-row status."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    all_paths = subprocess.check_output(["git", "ls-tree", "-r", "--name-only", PIN, "--",
                                        "src/fabric_cli/commands/fs", "src/fabric_cli/utils"], cwd=ROOT, text=True).splitlines()
    paths = []
    for absolute in all_paths:
        path = absolute.removeprefix("src/fabric_cli/")
        if not path.endswith(".py"):
            continue
        if path.startswith("commands/fs/") and (family(path) in VERBS):
            paths.append(path)
        elif path.startswith("utils/") and Path(path).stem in (
            "fab_cmd_mkdir_utils", "fab_cmd_set_utils", "fab_cmd_rm_utils", "fab_cmd_cp_utils",
            "fab_cmd_fs_utils", "fab_jmespath", "fab_cmd_ln_utils",
        ):
            paths.append(path)
    rows = [row for path in paths for row in source_rows(path)]
    ledger = {
        "source_commit": PIN, "native_scope": "resource mutations/OneLake; complete source denominator, not a completion assertion",
        "source_files": paths, "branches": rows, "support_and_rejection_gates": support_rows(),
        "verified_increments":VERIFIED_INCREMENTS,
        "counts": {"source_files": len(paths), "function_and_branch_rows": len(rows)},
        "standing_constraints": ["No protected oracle rewrite", "No duplicate HTTP/pager/LRO", "No live calls",
                                 "Missing helper blocks only dependent row", "All unreconciled rows remain backlog"],
    }
    output = Path(args.output)
    output.write_text(json.dumps(ledger, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"branch_ledger": str(output), **ledger["counts"],
                      "support_and_rejected_rows": len(ledger["support_and_rejection_gates"])}))


if __name__ == "__main__":
    main()

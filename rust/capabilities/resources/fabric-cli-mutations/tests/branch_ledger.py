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
        "id":"typed-creation-values",
        "implementation_commit":"4badbaaacda4fc46399aea1d0e19046957bbd028",
        "source_functions":[
            ["utils/fab_cmd_mkdir_utils.py","add_type_specific_payload"],
            ["utils/fab_cmd_mkdir_utils.py","_build_sql_database_creation_payload_if_exists"],
            ["utils/fab_cmd_mkdir_utils.py","_build_sql_database_new_payload"],
            ["utils/fab_cmd_mkdir_utils.py","_build_sql_database_restore_payload"],
            ["utils/fab_cmd_mkdir_utils.py","_build_sql_database_restore_deleted_payload"],
        ],
        "verified_branches":[
            "Lakehouse/Warehouse non-string flags retain source lower AttributeError through original outer handler",
            "KQL truthy explicit parent validates dbType before writes; falsey parent short-circuits to dependency",
            "KQL raw boolean/number/object parent and cluster/database reference values are not coerced to empty strings",
            "SQL default/unsupported mode source str formatting, None versus falsey values and restore required fields",
            "backup retention bool conversion, finite float truncation, signed/underscored strings and invalid numeric/type errors",
            "all 68 Python3.13 Unicode15.1 Nd decimal blocks, rejecting non-Nd numeric characters",
        ],
        "verification_status":"implemented_and_tested_for_listed_cases_only",
        "public_consumer":"examples/offline_mutation.rs",
        "original_source_comparison":"tests/creation_value_reference.py",
        "original_case_count":230,
        "native_regressions":"tests/consumer.rs raw KQL references and integer JSON type tests",
        "receipt":"validation-60.json",
        "remaining_function_branches":["other Python Unicode versions; values outside existing i64/u64/finite-f64 boundary; main composition"],
    },
    {
        "id":"item-metadata-set",
        "implementation_commit":"de2739f07e26b4c7b8bb603d3affa7e64c90d30f",
        "source_functions":[
            ["commands/fs/set/fab_fs_set_item.py","exec"],
            ["commands/fs/set/fab_fs_set_item.py","_update_item_metadata"],
            ["utils/fab_cmd_set_utils.py","validate_item_query"],
            ["utils/fab_cmd_set_utils.py","update_fabric_element"],
        ],
        "verified_branches":[
            "all 44 original item SET policies and 13 rejected delete alias diagnostics before I/O",
            "force/decline/confirmation, warning/read/progress/PATCH/output/cache order",
            "200/201/202/204 source Item updated and name cache effects remain independent from completion",
            "metadata top-key replacement and explicit missing/scalar/index wrapper failures",
            "allowlist/simple-path invalid queries fail before reads; public pure helper and finite-JSON safety errors remain unchanged",
        ],
        "verification_status":"implemented_and_tested_for_listed_cases_only",
        "public_consumer":"examples/offline_mutation.rs",
        "original_source_comparison":"tests/metadata_set_reference.py",
        "original_case_count":225,
        "native_regressions":"tests/consumer.rs",
        "receipt":"validation-59.json (native tests/build passed in 58 before corrected Python constant binding)",
        "remaining_function_branches":["definition SET aliases/codecs owned by deployment-1302708; main metadata adapter and context composition"],
    },
    {
        "id":"all-item-creation-policy",
        "implementation_commit":"c441271cf62095f179b598775502397e8e021f0f",
        "source_functions":[
            ["commands/fs/mkdir/fab_fs_mkdir_item.py","exec"],
            ["utils/fab_cmd_mkdir_utils.py","lowercase_keys"],
            ["utils/fab_cmd_mkdir_utils.py","get_params_per_item_type"],
            ["utils/fab_cmd_mkdir_utils.py","show_params_desc"],
            ["utils/fab_cmd_mkdir_utils.py","check_required_params"],
        ],
        "verified_branches":[
            "44 original types: source support before help/duplicate, exact rejected command diagnostics",
            "empty-param help including existing ID, populated duplicate before request",
            "typed source collection dispatch, folder payload and removed displayName/type options",
            "200/201 metadata and 202/204 output/cache suppression",
            "Mounted required None versus empty/bool/number/container values",
            "recursive lowercase across nested objects/arrays without mutating caller options",
        ],
        "verification_status":"implemented_and_tested_for_listed_cases_only",
        "public_consumer":"examples/offline_mutation.rs",
        "original_source_comparison":"tests/creation_policy_reference.py",
        "original_case_count":330,
        "native_regressions":"tests/consumer.rs",
        "receipt":"validation-57.json",
        "remaining_function_branches":["main dispatch, exact configured capacity resolution and broader option/resource combinations"],
    },
    {
        "id":"dynamic-item-definition-creation",
        "implementation_commit":"059ce490406054d85c804bbe44e8e1586bd0ec2e",
        "source_functions":[
            ["commands/fs/mkdir/fab_fs_mkdir_item.py","exec"],
            ["utils/fab_cmd_mkdir_utils.py","add_type_specific_payload"],
            ["utils/fab_cmd_mkdir_utils.py","_create_payload"],
            ["utils/fab_cmd_mkdir_utils.py","_initialize_batch_collection_for_dependency_creation"],
        ],
        "verified_branches":[
            "Report seven raw parts with truthy explicit or auto SemanticModel byConnection replacement",
            "Report incomplete/missing/accepted dependency ID preserves byPath raw bytes and source batch/cache/partial evidence",
            "AzureSQL warning, AzureSqlMI/Snowflake/CosmosDb dynamic templates and unknown-type modified GenericMirror fallback",
            "mirror connection missing/null/false/string; database truthiness; schema absent/default/empty/null/false",
            "mounted tables source split order, empty schema/table and untrimmed whitespace",
            "MountedDataFactory source f-string finite JSON strings/booleans/numbers/containers/Unicode/escapes",
            "actual original outer-handler UnexpectedError code/message for twelve malformed lower/split/unpack cases",
        ],
        "verification_status":"implemented_and_tested_for_listed_cases_only",
        "public_consumer":"examples/offline_mutation.rs",
        "original_source_comparison":"tests/dynamic_creation_reference.py",
        "original_case_count":128,
        "native_regressions":"tests/consumer.rs exact bytes, no-dispatch malformed definitions and retained dependency evidence",
        "receipt":"validation-54.json",
        "shared_dependency":{
            "source_commit":"f2763ab33de8b78ee467a64004128c1234b69175",
            "local_adoption_commit":"967fbdfc892756b529952569866224b245885caa",
            "api":"fabric_cli_output::json_dumps",
        },
        "remaining_function_branches":["main composition, complete parameter/resource/mode policy reconciliation"],
    },
    {
        "id":"public-copy-preflight",
        "implementation_commit":"713501b969b71ee8a6d7750a3ce702f92afb2d3d",
        "source_functions":[
            ["commands/fs/cp/fab_fs_cp_onelake.py","copy_local_2_onelake"],
            ["utils/fab_cmd_cp_utils.py","check_onelake_destination"],
            ["utils/fab_cmd_cp_utils.py","get_onelake_file_destination"],
        ],
        "verified_branches":[
            "existing root validation before local directory/file/missing checks",
            "file-name resolution after file check before progress/local read",
            "eight source item types, source roots and target file/folder/shortcut/undefined/table kinds",
            "source read-error/empty/real-body traces with no fake preflight upload or copied policy",
        ],
        "verification_status":"implemented_and_tested_for_listed_cases_only",
        "public_consumer":"examples/offline_copy_preflight.rs",
        "original_source_comparison":"tests/copy_preflight_reference.py",
        "original_case_count":400,
        "native_regressions":"tests/copy_preflight.rs",
        "receipt":"validation-53.json",
        "remaining_function_branches":["actual local filesystem orchestration and dispatch owned by storage-files-1302849"],
    },
    {
        "id":"workspace-and-tenant-removal",
        "implementation_commit":"7690755d60ea70e7f29b28f4a2030a0713f01c54",
        "source_functions":[
            ["commands/fs/rm/fab_fs_rm_workspace.py","single"],
            ["commands/fs/rm/fab_fs_rm_workspace.py","bulk"],
        ],
        "verified_branches":[
            "all original rm supported/rejected item types plus selectable Folder excluded from actual Item writes",
            "non-force selection/cancellation/confirmation, byname/bytype name ties, hard false/true",
            "empty workspace force false/true, all-rejected non-force, force catalog count before delete",
            "source 200/201/202 counters/cache versus 204 omission, accepted per-item deleted output",
            "tenant case-sensitive sort and mandatory selection/confirmation even with force",
            "later item/workspace failure retains prior accepted response/output/cache without final summary",
        ],
        "verification_status":"implemented_and_tested_for_listed_cases_only",
        "public_consumer":"examples/offline_removal.rs",
        "original_source_comparison":"tests/removal_reference.py",
        "original_case_count":71,
        "native_regressions":"tests/removal.rs",
        "receipt":"validation-50.json",
        "remaining_function_branches":["existing main complete-catalog, multi-select and context composition"],
    },
    {
        "id":"public-pure-set-helper",
        "implementation_commit":"c22dd9806ae3f2e31af74fabca0844050ddf0930",
        "source_functions":[["utils/fab_cmd_set_utils.py","update_fabric_element"]],
        "verified_branches":[
            "public external replacement of raw/plain or parsed JSON input, nested path/index, negative index",
            "source raw top-key projection and siblings, missing parents/out-of-bounds failure",
        ],
        "verification_status":"implemented_and_tested_for_listed_cases_only",
        "public_consumer":"examples/public_set.rs",
        "original_source_comparison":"tests/public_set_reference.py",
        "original_case_count":33,
        "receipt":"validation-49.json",
        "remaining_function_branches":["source command-specific validation and managed-resource caller integration"],
    },
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
        "later_increment_refs":["dynamic-item-definition-creation"],
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

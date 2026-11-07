# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
"""Generate native BPA identities from verified, inert pinned source syntax."""

import argparse
import ast
import hashlib
import json
from pathlib import Path

COMMIT = "2ddf46109c1209ceffa88149f4631e7b2275f9a1"
SOURCE_SHA256 = "bd28bf30d9815116defdb5664158f70a0a957dd6904a6ccdb33c4d007c4b648f"


def catalog(source: bytes) -> dict:
    """Derive identity membership without importing upstream product modules."""
    if hashlib.sha256(source).hexdigest() != SOURCE_SHA256:
        raise ValueError("Pinned BPA source hash does not match")
    module = ast.parse(source)
    factory = next(
        node
        for node in module.body
        if isinstance(node, ast.FunctionDef) and node.name == "model_bpa_rules"
    )
    assignment = next(
        node
        for node in factory.body
        if isinstance(node, ast.Assign)
        and any(isinstance(target, ast.Name) and target.id == "rules" for target in node.targets)
    )
    entries = assignment.value.args[0]
    if not isinstance(entries, ast.List) or len(entries.elts) != 64:
        raise ValueError("Pinned BPA inventory must contain exactly 64 rules")
    rules = []
    for entry in entries.elts:
        name = ast.literal_eval(entry.elts[3])
        scopes = ast.literal_eval(entry.elts[1])
        scopes = [scopes] if isinstance(scopes, str) else scopes
        key = name + "\0" + "\0".join(sorted(scopes))
        rules.append(
            {
                "id": "labs:" + hashlib.sha256(key.encode("utf-8")).hexdigest(),
                "name": name,
                "severity": ast.literal_eval(entry.elts[2]).lower(),
                "category": ast.literal_eval(entry.elts[0]),
                "scopes": scopes,
            }
        )
    if len({rule["id"] for rule in rules}) != len(rules):
        raise ValueError("Pinned rule identities must be unique")
    return {
        "copyright": "Copyright (c) Microsoft Corporation. Licensed under the MIT License.",
        "sourceCommit": COMMIT,
        "sourceSha256": SOURCE_SHA256,
        "rules": rules,
    }


def main() -> None:
    """Generate or check the production catalog; never rewrite source fixtures."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    source = root / "tests/fixtures/semantic_model_bpa/upstream/_model_bpa_rules.py"
    output = root / "sdk/semantic-model/client/src/rule_catalog.json"
    content = json.dumps(catalog(source.read_bytes()), ensure_ascii=True, indent=2) + "\n"
    if args.check:
        if output.read_text(encoding="utf-8") != content:
            raise ValueError("Native rule catalog differs from pinned source")
        print("PASS: exact 64-rule native identity catalog matches pinned source")
    else:
        output.write_text(content, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()

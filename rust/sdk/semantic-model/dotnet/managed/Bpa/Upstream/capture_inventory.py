# Copyright (c) Microsoft Corporation. Licensed under the MIT License.
"""Development-only inventory extraction. Not imported or executed by the product."""

import ast
import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).parent
SOURCE = ROOT / "_model_bpa_rules.py.txt"
TEXT = SOURCE.read_text(encoding="utf-8-sig")
TREE = ast.parse(TEXT)
EXTERNAL = {
    13: ["directLake.fallbackReasons"],
    14: ["directLake.fallbackReasons"],
    15: ["directLake.vOrder"],
    22: ["extendedStatistics.tables.rowCount"],
    40: ["options.dependencies=true", "dependencies.rows"],
    41: ["options.dependencies=true", "dependencies.rows"],
    44: ["options.dependencies=true", "dependencies.rows"],
    45: ["options.dependencies=true", "dependencies.rows"],
}
HELPERS = {
    "is_direct_lake": ["TOM.partitions.Mode"],
    "is_hybrid_table": ["TOM.table.partitions.Mode"],
    "has_hybrid_table": ["TOM.tables.partitions.Mode"],
    "used_in_relationships": ["TOM.relationships.endpoints", "TOM.relationships.cardinalities"],
    "used_in_sort_by": ["TOM.table.columns.SortByColumn"],
    "used_in_hierarchies": ["TOM.hierarchies.levels.Column"],
    "is_field_parameter": ["TOM.partitions.SourceType", "TOM.partitions.Source.Expression",
                           "TOM.columns.SourceColumn", "TOM.columns.ExtendedProperties"],
    "is_calculated_table": ["TOM.partitions.SourceType"],
    "all_rls": ["TOM.roles.TablePermissions"],
    "all_measures": ["TOM.tables.measures.Name", "TOM.tables.measures.Expression"],
    "all_partitions": ["TOM.tables.partitions.Mode"],
    "_uses_sum_or_average_on_string_column": ["TOM.expression", "TOM.columns.DataType", "TOM.columns.Name"],
}


def main() -> None:
    """Generate an auditable, source-order keyed inventory from the pinned source."""
    frame = next(
        node for node in ast.walk(TREE)
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
        and node.func.attr == "DataFrame" and node.args
        and isinstance(node.args[0], ast.List)
    )
    rows = []
    for index, row in enumerate(frame.args[0].elts, 1):
        values = row.elts
        scope = ast.literal_eval(values[1])
        scopes = [scope] if isinstance(scope, str) else scope
        name = ast.literal_eval(values[3])
        identity = name + "\0" + "\0".join(sorted(scopes))
        predicate = ast.get_source_segment(TEXT, values[4])
        required = sorted({
            "TOM." + ast.unparse(node)[4:]
            for node in ast.walk(values[4])
            if isinstance(node, ast.Attribute) and ast.unparse(node).startswith("obj.")
        })
        for helper, fields in HELPERS.items():
            if helper + "(" in predicate:
                required.extend(fields)
        required.extend(EXTERNAL.get(index, []))
        rows.append({
            "id": "labs:" + hashlib.sha256(identity.encode("utf-8")).hexdigest(),
            "predicateId": f"labs-{index:03}",
            "sourceKey": "src/sempy_labs/_model_bpa_rules.py#" + name,
            "category": ast.literal_eval(values[0]),
            "scopes": scopes,
            "severity": ast.literal_eval(values[2]),
            "ruleName": name,
            "predicate": predicate,
            "description": ast.literal_eval(values[5]) if len(values) > 5 else None,
            "url": ast.literal_eval(values[6]) if len(values) > 6 else None,
            "sourceLine": row.lineno,
            "requiredData": sorted(set(required)),
            "implementation": "Predicates.Evaluate",
            "unsupported": False,
        })
    assert len(rows) == 64
    (ROOT / "rules.json").write_text(json.dumps(rows, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    (ROOT.parent / "PinnedCatalog.g.cs").write_text(
        "// Copyright (c) Microsoft Corporation. Licensed under the MIT License.\n"
        "// Generated from the pinned source by Upstream/capture_inventory.py. Do not edit by hand.\n"
        "namespace Fabric.SemanticModel.Bpa;\n\n"
        "internal static class PinnedCatalog\n{\n"
        '    internal const string Json = """\n'
        + json.dumps(rows, ensure_ascii=True, separators=(",", ":"))
        + '\n""";\n}\n',
        encoding="utf-8",
    )
    hashes = {
        p.name: hashlib.sha256(p.read_bytes()).hexdigest()
        for p in ROOT.glob("*.txt")
    }
    (ROOT / "source-hashes.json").write_text(json.dumps(hashes, indent=2) + "\n", encoding="utf-8")
    print(f"Captured {len(rows)} pinned rules; {sum(len(r['scopes']) for r in rows)} rule/scope pairs.")


if __name__ == "__main__":
    main()

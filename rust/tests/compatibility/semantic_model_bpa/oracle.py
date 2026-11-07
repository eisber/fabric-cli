# Copyright (c) Microsoft Corporation. Licensed under the MIT License.
"""Offline developer oracle executing only hash-pinned upstream AST selections."""

from __future__ import annotations

import argparse
import ast
import builtins
import hashlib
import json
import re
from pathlib import Path
from types import SimpleNamespace
from typing import Any

PIN = "2ddf46109c1209ceffa88149f4631e7b2275f9a1"
REPOSITORY = "https://github.com/microsoft/semantic-link-labs"
FIXTURES = Path(__file__).resolve().parents[2] / "fixtures" / "semantic_model_bpa"
SOURCE_HASHES = {
    "_model_bpa_rules.py": "bd28bf30d9815116defdb5664158f70a0a957dd6904a6ccdb33c4d007c4b648f",
    "_model_bpa.py": "eb1c9851476e283e244e39437c80e009bbed68fede71fa3f8f2fd2b0519d0a0f",
    "_helper_functions.py": "86720495eff69165598a92e018462608f55ab1219f8663f8f848688dc4b0d572",
    "LICENSE": "c2cfccb812fe482101a8f04597dfc5a9991a6b2748266c47ac91b6a5aae15383",
}
RULE_COLUMNS = ["Category", "Scope", "Severity", "Rule Name", "Expression", "Description", "URL"]
RESULT_COLUMNS = ["Category", "Rule Name", "Severity", "Object Type", "Object Name", "Description", "URL"]


def digest(value: bytes) -> str:
    """Return the SHA-256 of exact bytes."""
    return hashlib.sha256(value).hexdigest()


def read_sources(directory: Path | None = None) -> dict[str, str]:
    """Verify all bytes before parsing; no module import or network access."""
    directory = directory or FIXTURES / "upstream"
    sources = {}
    for name, expected in SOURCE_HASHES.items():
        data = (directory / name).read_bytes()
        if digest(data) != expected:
            raise ValueError(f"source hash mismatch: {name}")
        sources[name] = data.decode("utf-8")
    return sources


def location(node: ast.AST) -> dict[str, int]:
    """Preserve one-based lines and zero-based AST UTF-8 column offsets."""
    return {key: getattr(node, key) for key in ("lineno", "col_offset", "end_lineno", "end_col_offset")}


def ast_hash(node: ast.AST) -> str:
    """Hash the structural AST, excluding interpreter-dependent source positions."""
    # Python 3.12 added this empty field to functions without type parameters.
    structural = ast.dump(node, annotate_fields=True, include_attributes=False)
    return digest(structural.replace(", type_params=[]", "").encode())


class Source:
    """Parsed, verified upstream source; never executes its module body."""

    def __init__(self, directory: Path | None = None) -> None:
        self.text = read_sources(directory)
        self.rules_tree = ast.parse(self.text["_model_bpa_rules.py"])
        self.evaluator_tree = ast.parse(self.text["_model_bpa.py"])
        self.factory = next(n for n in self.rules_tree.body if isinstance(n, ast.FunctionDef) and n.name == "model_bpa_rules")
        self.runner = next(n for n in self.evaluator_tree.body if isinstance(n, ast.FunctionDef) and n.name == "run_model_bpa")
        candidates = [
            n for n in ast.walk(self.factory)
            if isinstance(n, ast.List) and n.elts
            and all(isinstance(e, ast.Tuple) and len(e.elts) >= 5 and isinstance(e.elts[4], ast.Lambda) for e in n.elts)
        ]
        if len(candidates) != 1 or len(candidates[0].elts) != 64:
            raise ValueError("unexpected upstream rule table")
        self.rows = candidates[0].elts
        self.helpers = {
            n.name: n for n in self.rules_tree.body
            if isinstance(n, ast.FunctionDef) and n.name.startswith("_")
        }
        self.naming_helpers = [
            node for node in ast.parse(self.text["_helper_functions.py"]).body
            if isinstance(node, ast.FunctionDef)
            and node.name in ("format_dax_object_name", "create_relationship_name")
        ]

    def inventory(self) -> dict[str, Any]:
        """Extract literal metadata and provenance from the original tuple table."""
        rules = []
        for ordinal, row in enumerate(self.rows, 1):
            values = [ast.literal_eval(n) for n in row.elts[:4]]
            category, scope, severity, name = values
            expr = row.elts[4]
            scopes = [scope] if isinstance(scope, str) else scope
            globals_used = sorted({
                n.id for n in ast.walk(expr) if isinstance(n, ast.Name)
                and (n.id in self.helpers or n.id == "dependencies")
            })
            rules.append({
                "ordinal": ordinal, "category": category, "name": name,
                "severity": severity, "scopes": scopes, "sourceScope": scope,
                "description": ast.literal_eval(row.elts[5]) if len(row.elts) > 5 else None,
                "url": ast.literal_eval(row.elts[6]) if len(row.elts) > 6 else None,
                "expression": ast.get_source_segment(self.text["_model_bpa_rules.py"], expr),
                "expressionAstSha256": ast_hash(expr),
                "expressionLocation": location(expr),
                "sourceUrl": f"{REPOSITORY}/blob/{PIN}/src/sempy_labs/_model_bpa_rules.py#L{expr.lineno}-L{expr.end_lineno}",
                "tomWrapperMethods": sorted({
                    n.attr for n in ast.walk(expr) if isinstance(n, ast.Attribute)
                    and isinstance(n.value, ast.Name) and n.value.id == "tom"
                }),
                "tomEnumMembers": sorted({
                    ast.unparse(n) for n in ast.walk(expr) if isinstance(n, ast.Attribute)
                    and isinstance(n.value, ast.Attribute)
                    and isinstance(n.value.value, ast.Name) and n.value.value.id == "TOM"
                }),
                "objectAttributePaths": sorted({
                    ast.unparse(n) for n in ast.walk(expr) if isinstance(n, ast.Attribute)
                    and ast.unparse(n).startswith("obj.")
                }),
                "globalDependencies": globals_used,
            })
        return {
            "schemaVersion": 1, "repository": REPOSITORY, "commit": PIN,
            "sourceFiles": [
                {"file": name, "sha256": sha,
                 "byteLength": len(self.text[name].encode("utf-8")),
                 "path": name if name == "LICENSE" else f"src/sempy_labs/{name}"}
                for name, sha in SOURCE_HASHES.items()
            ],
            "ruleCount": len(rules), "ruleColumns": RULE_COLUMNS,
            "resultColumns": RESULT_COLUMNS,
            "missingTupleFields": "null in this JSON ledger; pandas pads absent tuple fields with missing values",
            "helpers": [
                {"name": name, "location": location(node), "astSha256": ast_hash(node),
                 "imports": [ast.unparse(n) for n in ast.walk(node) if isinstance(n, (ast.Import, ast.ImportFrom))]}
                for name, node in self.helpers.items()
            ],
            "namingHelpers": [
                {"name": node.name, "location": location(node), "astSha256": ast_hash(node)}
                for node in self.naming_helpers
            ],
            "evaluator": self.evaluator_provenance(),
            "rules": rules,
        }

    def evaluator_nodes(self) -> tuple[ast.If, list[ast.stmt]]:
        """Select intact source statements for no-tables and the evaluation core."""
        context = next(n for n in self.runner.body if isinstance(n, ast.With))
        preflight = next(
            n for n in context.body if isinstance(n, ast.If)
            and ast.unparse(n.test) == "tom.model.Tables.Count == 0"
        )
        start = next(
            i for i, n in enumerate(context.body) if isinstance(n, ast.Assign)
            and isinstance(n.targets[0], ast.Name) and n.targets[0].id == "violations"
        )
        return preflight, context.body[start:]

    def evaluator_provenance(self) -> dict[str, Any]:
        """Record default parameters and the exact statement selection boundary."""
        preflight, core = self.evaluator_nodes()
        defaults = self.runner.args.defaults
        arguments = self.runner.args.args[-len(defaults):]
        return {
            "defaults": {a.arg: ast.literal_eval(d) for a, d in zip(arguments, defaults)},
            "noTables": {"location": location(preflight), "astSha256": ast_hash(preflight)},
            "coreStatements": [
                {"kind": type(n).__name__, "location": location(n), "astSha256": ast_hash(n)}
                for n in core
            ],
            "executionBoundary": "Original no-tables branch, scope map, predicate dispatch, concat, merge and final column projection only. No imports, connection, dependency acquisition, severity icons, translation, HTML, Spark, export or product startup.",
        }

    def predicate(self, ordinal: int, tom: Any) -> Any:
        """Compile the original lambda and original local string-column helpers."""
        env: dict[str, Any] = {
            "__builtins__": dict(vars(builtins), __import__=synthetic_import),
            "TOM": ENUMS, "re": re, "Set": set,
        }
        # Dependencies are injected evidence, not a fabricated dependency dataframe.
        if "dependencies" in tom.calls:
            env["dependencies"] = tom.calls["dependencies"]
        for name in ("_memoize_on_tom", "_string_column_names", "_uses_sum_or_average_on_string_column"):
            node = self.helpers[name]
            exec(compile(ast.Module(body=[node], type_ignores=[]), "<pinned-helper>", "exec"), env)
        expr = ast.Expression(body=self.rows[ordinal - 1].elts[4])
        return eval(compile(expr, "<pinned-rule>", "eval"), env)

    def core(self, tom: Any, selected: list[int]) -> dict[str, Any]:
        """Execute original dispatch statements with a minimal dataframe double."""
        records = []
        for ordinal in selected:
            row = self.rows[ordinal - 1]
            values = [ast.literal_eval(n) for n in row.elts[:4]]
            values += [self.predicate(ordinal, tom)]
            values += [ast.literal_eval(n) for n in row.elts[5:]]
            values += [None] * (len(RULE_COLUMNS) - len(values))
            records.append(dict(zip(RULE_COLUMNS, values)))
        preflight, statements = self.evaluator_nodes()
        function = ast.FunctionDef(
            name="_evaluate", args=ast.arguments(posonlyargs=[], args=[], kwonlyargs=[], kw_defaults=[], defaults=[]),
            body=[preflight, *statements, ast.Return(value=ast.Name(id="finalDF", ctx=ast.Load()))],
            decorator_list=[],
        )
        module = ast.fix_missing_locations(ast.Module(body=[function], type_ignores=[]))
        output: list[str] = []
        env = {
            "tom": tom, "rules": Frame(records, RULE_COLUMNS), "pd": PandasDouble,
            "dataset_name": "Synthetic", "workspace_name": "Offline",
            "icons": SimpleNamespace(warning="<warning>"),
            "log": lambda function: function,
            "print": lambda text: output.append(text),
        }
        exec(compile(ast.Module(body=self.naming_helpers, type_ignores=[]), "<pinned-naming-helpers>", "exec"), env)
        exec(compile(module, "<pinned-evaluator-core>", "exec"), env)
        result = env["_evaluate"]()
        if result is None:
            return {"outcome": "no_tables", "returnValue": None, "messages": output}
        return {"outcome": "dataframe", "columns": result.columns, "rows": result.rows}


class Collection(list):
    """Only the TOM collection Count property in addition to Python iteration."""

    @property
    def Count(self) -> int:
        """Return the synthetic collection size."""
        return len(self)


def objects(value: Any) -> Any:
    """Map explicit fixture fields to attribute objects without default metadata."""
    if isinstance(value, dict):
        return SimpleNamespace(**{key: objects(item) for key, item in value.items()})
    if isinstance(value, list):
        return Collection(objects(item) for item in value)
    return value


ENUMS = objects({
    "DataType": {k: k for k in ("Double", "Decimal", "Int64", "String", "DateTime")},
    "ColumnType": {k: k for k in ("Data", "Calculated")},
    "RelationshipEndCardinality": {k: k for k in ("Many", "One")},
    "CrossFilteringBehavior": {"BothDirections": "BothDirections"},
    "ModeType": {k: k for k in ("Import", "DirectQuery", "Dual", "DirectLake")},
    "PartitionSourceType": {"M": "M"},
})


def synthetic_import(name: str, *args: Any, **kwargs: Any) -> Any:
    """Allow only the synthetic TOM namespace, never an actual external import."""
    if name != "Microsoft.AnalysisServices.Tabular":
        raise ImportError(f"oracle forbids import: {name}")
    return objects({"AnalysisServices": {"Tabular": ENUMS}})


class MissingEvidence(RuntimeError):
    """The synthetic test has not supplied a collaborator response."""


class Collaborators:
    """Explicit synthetic call responses, not implementations of TOM helpers."""

    def __init__(self, calls: dict[str, Any]) -> None:
        self.calls = calls
        self.observed: list[str] = []
        self.arguments: list[dict[str, Any]] = []

    def __getattr__(self, name: str) -> Any:
        if name.startswith("_bpa_"):
            raise AttributeError(name)

        def call(*args: Any, **kwargs: Any) -> Any:
            self.observed.append(name)
            self.arguments.append({"method": name, "keywords": sorted(kwargs), "positionalCount": len(args)})
            if name not in self.calls:
                raise MissingEvidence(name)
            return objects(self.calls[name])

        return call


def predicate_result(source: Source, case: dict[str, Any]) -> dict[str, Any]:
    """Capture boolean truthiness or a data/evidence error from the original AST."""
    tom = Collaborators(case["calls"])
    try:
        result = {"outcome": "predicate", "violates": bool(source.predicate(case["rule"], tom)(objects(case["object"]), tom))}
    except Exception as error:
        result = {"outcome": "error", "errorType": type(error).__name__}
    return {**result, "observedCalls": tom.arguments}


class Frame:
    """Small dataframe test double for the selected upstream evaluator operations."""

    def __init__(self, data: Any = None, columns: list[str] | None = None) -> None:
        self.columns = columns or (list(data[0]) if isinstance(data, list) and data else [])
        if isinstance(data, dict):
            count = len(next(v for v in data.values() if isinstance(v, list)))
            self.rows = [{k: v[i] if isinstance(v, list) else v for k, v in data.items()} for i in range(count)]
            self.columns = list(data)
        else:
            self.rows = data or []

    def __getitem__(self, columns: list[str]) -> Frame:
        return Frame([{key: row[key] for key in columns} for row in self.rows], columns)

    def iterrows(self) -> Any:
        """Yield the original rule records in source order."""
        return enumerate(self.rows)

    def rename(self, columns: dict[str, str], inplace: bool = False) -> Frame:
        """Rename projection keys, as used once by the original evaluator."""
        target = self if inplace else Frame(list(self.rows), list(self.columns))
        target.rows = [{columns.get(k, k): v for k, v in row.items()} for row in target.rows]
        target.columns = [columns.get(k, k) for k in target.columns]
        return target


class PandasDouble:
    """No pandas import: only concat and the selected unique-name left merge."""

    DataFrame = Frame

    @staticmethod
    def concat(frames: list[Frame], ignore_index: bool = False) -> Frame:
        """Append rows without changing values."""
        return Frame([row for frame in frames for row in frame.rows], frames[0].columns)

    @staticmethod
    def merge(left: Frame, right: Frame, left_on: str, right_on: str, how: str) -> Frame:
        """Join fixture rule metadata; not a general pandas compatibility layer."""
        if how != "left":
            raise ValueError("only source left merge is supported")
        return Frame(
            [{**a, **b} for a in left.rows for b in right.rows if a[left_on] == b[right_on]],
            list(dict.fromkeys([*left.columns, *right.columns])),
        )


def bim_model(document: dict[str, Any]) -> Collaborators:
    """Adapt only fixture metadata/collection shapes, without rule/helper logic."""
    tom = Collaborators({})
    model = objects({"Name": document.get("name", "Synthetic"), "Tables": [], "Relationships": [], "Roles": []})
    model.Model = model
    columns, measures, partitions = Collection(), Collection(), Collection()
    tables = {}
    for value in document["model"]["tables"]:
        table = objects({"Name": value["name"], "Columns": [], "Measures": [], "Partitions": []})
        model.Tables.append(table)
        tables[table.Name] = table
        for value_column in value.get("columns", []):
            column = objects({
                "Name": value_column["name"], "Parent": table, "Table": table,
                "DataType": {"double": "Double", "decimal": "Decimal", "int64": "Int64", "string": "String"}[value_column["dataType"]],
                "Type": "Data", "SourceColumn": value_column.get("sourceColumn"),
            })
            columns.append(column)
            table.Columns.append(column)
    for value in document["model"].get("relationships", []):
        from_table = tables[value["fromTable"]]
        to_table = tables[value["toTable"]]
        model.Relationships.append(objects({
            "Name": value["name"],
            "FromTable": from_table, "ToTable": to_table,
            "FromColumn": next(c for c in from_table.Columns if c.Name == value["fromColumn"]),
            "ToColumn": next(c for c in to_table.Columns if c.Name == value["toColumn"]),
            "FromCardinality": {"many": "Many", "one": "One"}[value["fromCardinality"]],
            "ToCardinality": {"many": "Many", "one": "One"}[value["toCardinality"]],
            "CrossFilteringBehavior": {
                "bothDirections": "BothDirections", "oneDirection": "OneDirection"
            }[value["crossFilteringBehavior"]],
        }))
    tom.model = model
    for method, values in {
        "all_columns": columns, "all_calculated_columns": [],
        "all_measures": measures, "all_hierarchies": [],
        "all_calculated_tables": [], "all_calculation_items": [],
        "all_rls": [], "all_partitions": partitions, "all_functions": [],
    }.items():
        tom.calls[method] = values
    return tom


def evaluation_result(source: Source, case: dict[str, Any]) -> dict[str, Any]:
    """Capture bounded evaluator results, distinguishing parser and predicate errors."""
    try:
        model = json.loads((FIXTURES / "models" / case["model"]).read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {"outcome": "input_error", "errorType": "JSONDecodeError", "origin": "oracle_json_reader_not_TOM"}
    try:
        return source.core(bim_model(model), case["selectedRules"])
    except Exception as error:
        return {"outcome": "error", "errorType": type(error).__name__, "origin": "original_predicate_or_selected_core"}


def coverage(source: Source, cases: list[dict[str, Any]]) -> dict[str, Any]:
    """Enumerate expression evidence separately from untested helper/runtime semantics."""
    inventory = source.inventory()
    return {
        "schemaVersion": 1, "commit": PIN, "ruleCount": 64,
        "predicateCaseCount": len(cases),
        "rules": [
            {
                "ordinal": row["ordinal"], "name": row["name"],
                "caseIds": [c["id"] for c in cases if c["rule"] == row["ordinal"]],
                "status": "original_expression_with_synthetic_collaborators" if any(c["rule"] == row["ordinal"] for c in cases) else "not_executed",
                "unverifiedWrapperMethods": row["tomWrapperMethods"],
                "unverifiedExternalHelpers": [
                    name for name in row["globalDependencies"] if name != "_uses_sum_or_average_on_string_column"
                ],
            }
            for row in inventory["rules"]
        ],
        "notCoveredRules": [
            {"ordinal": 14, "reason": "Original helper imports Direct Lake fallback-reason provider; real DMV/Fabric evidence not available."},
            {"ordinal": 15, "reason": "Original helper imports lakehouse V-Order provider and inspects source identity/partitions; no live storage access."},
        ],
        "helperExecution": {
            "originalLocalHelpersExecuted": ["_memoize_on_tom", "_string_column_names", "_uses_sum_or_average_on_string_column"],
            "scope": "String-column positive/negative paths only; no real TOM enums, query-error or cache-failure proof.",
            "dependencyMethods": "unqualified_columns, fully_qualified_measures, depends_on and referenced_by receive explicit synthetic responses; dependency parsing/graph semantics are NOT tested.",
            "rowCounts": "Explicit synthetic row_count responses test the original threshold only; no DMV or VertiPaq acquisition.",
        },
        "unexecutedRuntime": [
            "Full run_model_bpa invocation/import/startup and actual pandas dataframe behavior",
            "Real TOM package enum/property/null/collection behavior; TOM BIM/TMDL deserialization",
            "Default full-inventory scan against an actual model (bounded evaluator cases select rules explicitly)",
            "Connection, authentication, workspace/model resolution, XMLA, Fabric, Spark and notebook rendering",
            "Dependency acquisition/helper algorithms, real model row counts and extended VertiPaq annotations",
            "Direct Lake view detection, fallback reason queries, source identities and lakehouse V-Order inspection",
            "HTML/icons/translation/export, optional .NET host/Rust bridge/CLI, cancellation and native lifecycle",
        ],
    }


def write_new(path: Path, value: Any) -> None:
    """Never overwrite a frozen oracle artifact."""
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, indent=2, ensure_ascii=False)
        stream.write("\n")


def fixture_manifest() -> dict[str, Any]:
    """Hash the captured inputs, outputs, ledger, source bytes and BIM examples."""
    paths = sorted(
        p for p in FIXTURES.rglob("*")
        if p.is_file() and p.suffix != ".md" and p.name != "capture_manifest.json"
    )
    return {
        "schemaVersion": 1,
        "commit": PIN,
        "files": [
            {"path": path.relative_to(FIXTURES).as_posix(), "sha256": digest(path.read_bytes())}
            for path in paths
        ],
    }


def capture(source: Source) -> None:
    """First capture only; baseline changes require explicit independent approval."""
    from capture_inputs import predicate_inputs

    paths = [FIXTURES / name for name in ("inventory.json", "predicate_cases.json", "evaluation_cases.json", "coverage.json")]
    if (FIXTURES / "capture_manifest.json").exists():
        raise FileExistsError("capture refuses to replace the frozen manifest")
    if any(path.exists() for path in paths):
        raise FileExistsError("capture refuses to replace any frozen artifact")
    cases = predicate_inputs()
    for case in cases:
        case["expected"] = predicate_result(source, case)
        if case["label"] in ("positive", "negative"):
            expected = case["label"] == "positive"
            if case["expected"].get("violates") is not expected:
                raise ValueError(f"fixture construction mismatch: {case['id']}: {case['expected']}")
    evaluations = [
        {"id": "default-catalog-no-tables", "model": "empty.bim", "selectedRules": list(range(1, 65))},
        {"id": "no-violations-is-empty-dataframe", "model": "decimal.bim", "selectedRules": [1]},
        {"id": "numeric-metadata-violation", "model": "double.bim", "selectedRules": [1]},
        {"id": "predicate-data-error-is-not-empty-results", "model": "missing-source-column.bim", "selectedRules": [27]},
        {"id": "malformed-bim-json", "model": "malformed.bim", "selectedRules": [1]},
        {"id": "relationship-and-model-scopes", "model": "relationships.bim", "selectedRules": [3, 6, 19, 26, 58]},
        {"id": "multi-scope-case-and-names", "model": "naming.bim", "selectedRules": [61, 62, 63]},
    ]
    for case in evaluations:
        case["expected"] = evaluation_result(source, case)
    values = [source.inventory(), cases, evaluations, coverage(source, cases)]
    for path, value in zip(paths, values):
        write_new(path, value)
    write_new(FIXTURES / "capture_manifest.json", fixture_manifest())
    print(f"Captured {len(source.rows)} rules, {len(cases)} predicate cases, {len(evaluations)} evaluator cases.")


def validate(source: Source) -> None:
    """Compare current source execution with immutable captured expectations."""
    manifest = json.loads((FIXTURES / "capture_manifest.json").read_text(encoding="utf-8"))
    if manifest != fixture_manifest():
        raise AssertionError("captured fixture bytes changed")
    inventory = json.loads((FIXTURES / "inventory.json").read_text(encoding="utf-8"))
    if inventory != source.inventory():
        raise AssertionError("rule inventory/provenance drift")
    cases = json.loads((FIXTURES / "predicate_cases.json").read_text(encoding="utf-8"))
    for case in cases:
        actual = predicate_result(source, case)
        if actual != case["expected"]:
            raise AssertionError(f"{case['id']}: expected {case['expected']}, actual {actual}")
    evaluations = json.loads((FIXTURES / "evaluation_cases.json").read_text(encoding="utf-8"))
    for case in evaluations:
        actual = evaluation_result(source, case)
        if actual != case["expected"]:
            raise AssertionError(f"{case['id']}: expected {case['expected']}, actual {actual}")
    if json.loads((FIXTURES / "coverage.json").read_text(encoding="utf-8")) != coverage(source, cases):
        raise AssertionError("coverage inventory drift")
    print(f"PASS: {len(source.rows)} pinned rules; {len(cases)} predicates; {len(evaluations)} evaluator cases; 2 explicitly unexecuted rules.")


def main() -> None:
    """Run offline validation, or explicitly create the first immutable capture."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--capture", action="store_true", help="first capture only; refuses existing baselines")
    args = parser.parse_args()
    source = Source()
    if args.capture:
        capture(source)
    else:
        validate(source)


if __name__ == "__main__":
    main()

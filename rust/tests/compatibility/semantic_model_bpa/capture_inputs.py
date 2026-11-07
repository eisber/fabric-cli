# Copyright (c) Microsoft Corporation. Licensed under the MIT License.
"""Synthetic inputs only: no reimplementation of any BPA predicate."""

from __future__ import annotations

from copy import deepcopy
from typing import Any


def predicate_inputs() -> list[dict[str, Any]]:
    """Construct positive/negative input pairs for original upstream expressions."""
    cases: list[dict[str, Any]] = []

    def pair(
        rule: int,
        positive: dict[str, Any],
        negative: dict[str, Any],
        positive_calls: dict[str, Any] | None = None,
        negative_calls: dict[str, Any] | None = None,
    ) -> None:
        for label, obj, calls in [
            ("positive", positive, positive_calls),
            ("negative", negative, negative_calls),
        ]:
            cases.append(
                {
                    "id": f"r{rule:02d}-{label}",
                    "rule": rule,
                    "label": label,
                    "object": obj,
                    "calls": calls or {},
                }
            )

    def expression(rule: int, positive: str, negative: str) -> None:
        pair(rule, {"Expression": positive}, {"Expression": negative})

    def extra(rule: int, label: str, obj: dict[str, Any], **calls: Any) -> None:
        cases.append(
            {
                "id": f"r{rule:02d}-{label}",
                "rule": rule,
                "label": label,
                "object": obj,
                "calls": calls,
            }
        )

    many = {"FromCardinality": "Many", "ToCardinality": "Many"}
    single = {"FromCardinality": "Many", "ToCardinality": "One"}
    bidirectional = {**single, "CrossFilteringBehavior": "BothDirections"}
    forward = {**single, "CrossFilteringBehavior": "OneDirection"}
    table = {"Name": "Sales"}
    pair(1, {"DataType": "Double"}, {"DataType": "Decimal"})
    pair(2, {"Type": "Calculated"}, {"Type": "Data"})
    pair(3, {**many, "CrossFilteringBehavior": "OneDirection"}, forward)
    extra(3, "bidirectional-one-to-many", bidirectional)
    pair(4, {"FilterExpression": "userprincipalname()"}, {"FilterExpression": "TRUE()"})
    extra(4, "regex-parentheses-are-not-literal", {"FilterExpression": "USERNAME"})
    pair(
        5, table, table,
        {"used_in_relationships": [many], "all_rls": [table]},
        {"used_in_relationships": [single]},
    )
    pair(6, {**many, "CrossFilteringBehavior": "BothDirections"}, bidirectional)
    mdx = {
        "IsAvailableInMDX": True, "IsHidden": True,
        "Parent": {"IsHidden": False}, "SortByColumn": None,
    }
    pair(
        7, mdx, mdx,
        {"is_direct_lake": False, "used_in_sort_by": [], "used_in_hierarchies": []},
        {"is_direct_lake": True},
    )
    partition = {"Parent": table, "Mode": "DirectQuery", "DataCoverageDefinition": None}
    pair(
        8, partition, {**partition, "DataCoverageDefinition": "TRUE()"},
        {"is_hybrid_table": True}, {"is_hybrid_table": True},
    )
    pair(
        9, {}, {},
        {"all_partitions": [{"Mode": "Dual"}]},
        {"all_partitions": [{"Mode": "Dual"}, {"Mode": "DirectQuery"}]},
    )
    dimension = {**table, "Partitions": [{"Mode": "Import"}]}
    pair(
        10, dimension, dimension,
        {"has_hybrid_table": True, "used_in_relationships": [
            {"ToCardinality": "One", "ToTable": table}
        ]},
        {"has_hybrid_table": False},
    )
    pair(
        11,
        {"SourceType": "M", "Source": {"Expression": 'Table.Join("Sales", "Key")'}},
        {"SourceType": "M", "Source": {"Expression": 'table.join("Sales", "Key")'}},
    )
    snowflake = {**table, "CalculationGroup": None}
    pair(
        12, snowflake, {**table, "CalculationGroup": {}},
        {"used_in_relationships": [{"FromTable": table, "ToTable": table}]},
    )
    pair(13, {}, {}, {"is_direct_lake_using_view": True}, {"is_direct_lake_using_view": False})
    expression(16, "divide([Amount], [Count], 0)", "DIVIDE([Amount], [Count])")
    extra(16, "literal-space-stripping", {"Expression": "0 + [Amount]"})
    extra(16, "tab-not-stripped", {"Expression": "0\t+ [Amount]"})
    pair(
        17, table, table,
        {"is_field_parameter": False, "is_calculated_table": True},
        {"is_field_parameter": True},
    )
    pair(
        18, {"Type": "Calculated", "Expression": "related ('T'[Amount])"},
        {"Type": "Data", "Expression": "RELATED('T'[Amount])"},
    )
    pair(19, {"Relationships": [bidirectional]}, {"Relationships": [forward]})
    extra(19, "threshold-exactly-30-percent", {"Relationships": [bidirectional] * 3 + [forward] * 7})
    extra(19, "threshold-40-percent", {"Relationships": [bidirectional] * 4 + [forward] * 6})
    extra(19, "double-counts-both-and-many", {
        "Relationships": [{**many, "CrossFilteringBehavior": "BothDirections"}] + [forward] * 4
    })
    extra(19, "empty-relationships", {"Relationships": []})
    pair(
        20, {"Name": "LocalDateTable_test"}, {"Name": "localdatetable_test"},
        {"is_calculated_table": True}, {"is_calculated_table": True},
    )
    pair(21, {"Name": "Calendar", "DataCategory": ""}, {"Name": "Calendar", "DataCategory": "Time"})
    large = {"Partitions": [{}]}
    pair(
        22, large, large,
        {"is_direct_lake": False, "row_count": 25000001},
        {"is_direct_lake": False, "row_count": 25000000},
    )
    extra(22, "fractional-above-threshold", large, is_direct_lake=False, row_count=25000000.5)
    extra(22, "missing-row-count-is-not-zero", large, is_direct_lake=False)
    pair(23, {"FilterExpression": "LEFT([Code],1)"}, {"FilterExpression": "LEFT ([Code],1)"})
    date = {"DataCategory": "Time", "Columns": [{"IsKey": True, "DataType": "DateTime"}]}
    pair(24, {"Tables": []}, {"Tables": [date]})
    expression(25, "", "SELECTEDMEASURE()")
    extra(25, "none-expression-data-error", {"Expression": None})
    pair(
        26, {"FromColumn": {"DataType": "Double"}, "ToColumn": {"DataType": "Int64"}},
        {"FromColumn": {"DataType": "Int64"}, "ToColumn": {"DataType": "Int64"}},
    )
    pair(27, {"Type": "Data", "SourceColumn": ""}, {"Type": "Data", "SourceColumn": "Amount"})
    pair(
        28, {"IsAvailableInMDX": False, "SortByColumn": {}},
        {"IsAvailableInMDX": True},
        {"is_direct_lake": False, "used_in_sort_by": [], "used_in_hierarchies": []},
        {"is_direct_lake": False},
    )
    pair(
        29, table, table,
        {"all_measures": [{"Expression": "USERELATIONSHIP('A'[Id], 'Sales'[Id])"}],
         "all_rls": [{"Table": table}]},
        {"all_measures": [{"Expression": "SUM('Sales'[Amount])"}]},
    )
    expression(30, "IfError ([M], 0)", "IF([M] > 0, [M], 0)")
    expression(31, "intersect ([A], [B])", "TREATAS([A], 'B'[Id])")
    expression(32, "evaluateandlog ([M])", "[M]")
    pair(
        33, {"Expression": "[Total]"}, {"Expression": " [Total] "},
        {"all_measures": [{"Name": "Total"}]}, {"all_measures": [{"Name": "Total"}]},
    )
    pair(
        34, {"Name": "A", "Expression": "SUM ( 'T'[X] )"},
        {"Name": "A", "Expression": "sum('T'[X])"},
        {"all_measures": [{"Name": "B", "Expression": "SUM('T'[X])"}]},
        {"all_measures": [{"Name": "B", "Expression": "SUM('T'[X])"}]},
    )
    expression(35, "DIVIDE([A],[B]) + 1", "DIVIDE([A],[B]) + 2")
    expression(36, "1 - DIVIDE([A],[B])", "DIVIDE([A],[B])")
    expression(37, "CALCULATE([M], FILTER('T', [Flag] > 0))", "CALCULATE([M], 'T'[Flag] > 0)")
    expression(38, "CALCULATE([M], FILTER('T', 'T'[Flag] > 0))", "CALCULATE([M], 'T'[Flag] > 0)")
    expression(39, "[A] / [B]", "DIVIDE([A],[B])")
    extra(39, "line-comment-not-division", {"Expression": "[A] // comment"})
    extra(39, "block-comment-not-division", {"Expression": "[A] /* comment */"})
    pair(40, {}, {}, {"unqualified_columns": ["Amount"]}, {"unqualified_columns": []})
    pair(41, {}, {}, {"fully_qualified_measures": ["Total"]}, {"fully_qualified_measures": []})
    pair(
        42, {}, {},
        {"_get_expression": "sum ('T'[Label])", "all_columns": [{"Name": "Label", "DataType": "String"}]},
        {"_get_expression": "SUM('T'[label])", "all_columns": [{"Name": "Label", "DataType": "String"}]},
    )
    extra(42, "average-on-string", {}, _get_expression="AVERAGE([Label])",
          all_columns=[{"Name": "Label", "DataType": "String"}])
    extra(42, "cross-table-name-only-match", {}, _get_expression="SUM('NumericTable'[Label])",
          all_columns=[{"Name": "Label", "DataType": "String", "Parent": {"Name": "OtherTable"}}])
    relation = {
        "IsActive": False, "FromTable": {"Name": "A"}, "ToTable": table,
        "FromColumn": {"Name": "Id"}, "ToColumn": {"Name": "Id"},
    }
    pair(
        43, relation, relation,
        {"all_measures": [{"Expression": "SUM('A'[Amount])"}]},
        {"all_measures": [{"Expression": "USERELATIONSHIP('A'[Id], 'Sales'[Id])"}]},
    )
    extra(43, "reverse-activation-order", relation, all_measures=[
        {"Expression": "USERELATIONSHIP('Sales'[Id], 'A'[Id])"}
    ])
    maintenance = {"IsHidden": True, "Parent": {"IsHidden": False}}
    pair(
        44, maintenance, maintenance,
        {"used_in_relationships": [], "used_in_hierarchies": [], "used_in_sort_by": [],
         "depends_on": ["dependency"]},
        {"used_in_relationships": [], "used_in_hierarchies": [], "used_in_sort_by": [],
         "depends_on": []},
    )
    pair(45, {"IsHidden": True}, {"IsHidden": True}, {"referenced_by": []}, {"referenced_by": ["consumer"]})
    pair(
        46, snowflake, snowflake,
        {"used_in_relationships": [], "is_field_parameter": False},
        {"used_in_relationships": [single]},
    )
    pair(
        47, {"CalculationGroup": {"CalculationItems": []}},
        {"CalculationGroup": {"CalculationItems": [{"Name": "YTD"}]}},
    )
    pair(48, {"IsHidden": False, "Description": ""}, {"IsHidden": False, "Description": " "})
    pair(
        49, {"Name": "Date", "DataType": "DateTime", "FormatString": "General"},
        {"Name": "DATE", "DataType": "DateTime", "FormatString": "YYYY-MM-DD"},
    )
    numeric = {"DataType": "Int64", "IsHidden": False, "Parent": {"IsHidden": False}}
    pair(50, {**numeric, "SummarizeBy": "Sum"}, {**numeric, "SummarizeBy": "None"})
    pair(
        51, {"IsHidden": False, "FormatString": "", "FormatStringDefinition": None},
        {"IsHidden": False, "FormatString": "", "FormatStringDefinition": {"Expression": "'0'"}},
    )
    pair(52, {"Name": "CITYCode", "DataCategory": ""}, {"Name": "MyCity", "DataCategory": ""})
    pair(53, {"FormatString": "0%"}, {"FormatString": "#,0.0%;-#,0.0%;#,0.0%"})
    pair(54, {"FormatString": "0"}, {"FormatString": "#,0.0"})
    extra(54, "currency-symbol-exemption", {"FormatString": "$0"})
    pair(
        55, {"Name": "Id", "IsHidden": False}, {"Name": "Id", "IsHidden": True},
        {"used_in_relationships": [{"FromColumn": {"Name": "Id"}, "FromCardinality": "Many"}]},
    )
    key = {"Name": "Id", "Table": {"Name": "Sales", "DataCategory": ""}, "IsKey": False}
    key_rel = {"ToTable": table, "ToColumn": {"Name": "Id"}, "ToCardinality": "One"}
    pair(
        56, key, {**key, "Table": {"Name": "Sales", "DataCategory": "Time"}},
        {"used_in_relationships": [key_rel]}, {"used_in_relationships": [key_rel]},
    )
    pair(
        57, {"Name": "Month", "DataType": "String", "SortByColumn": ""},
        {"Name": "Months", "DataType": "String", "SortByColumn": ""},
    )
    extra(57, "null-is-not-empty-string", {"Name": "Month", "DataType": "String", "SortByColumn": None})
    pair(
        58, {"FromColumn": {"DataType": "String"}, "ToColumn": {"DataType": "Int64"}},
        {"FromColumn": {"DataType": "Int64"}, "ToColumn": {"DataType": "Int64"}},
    )
    pair(
        59, {"Name": "Month", "DataType": "DateTime", "FormatString": "MMMM YYYY"},
        {"Name": "Month", "DataType": "DateTime", "FormatString": "MMMM yyyy"},
    )
    flag = {"IsHidden": False, "Parent": {"IsHidden": False}}
    pair(60, {**flag, "Name": "IsActive", "DataType": "Int64"},
         {**flag, "Name": "IsActive", "DataType": "String"})
    extra(60, "space-flag-suffix", {**flag, "Name": "Active Flag", "DataType": "Double"})
    pair(61, {"Name": " Sales"}, {"Name": "Sales"})
    extra(61, "empty-name-data-error", {"Name": ""})
    pair(62, {"Name": "sales"}, {"Name": "7sales"})
    extra(62, "empty-name-data-error", {"Name": ""})
    pair(63, {"Name": "Sales\tTotal"}, {"Name": "Sales-Total!"})
    pair(
        64, {"Name": "Import", "Parent": {"Name": "Sales", "Partitions": [{}]}},
        {"Name": "Import", "Parent": {"Name": "Sales", "Partitions": [{}, {}]}},
    )
    for case in cases:
        if case["rule"] in (40, 41, 44, 45):
            case["calls"]["dependencies"] = {"oracleSynthetic": "opaque-not-a-dataframe"}
    return deepcopy(cases)

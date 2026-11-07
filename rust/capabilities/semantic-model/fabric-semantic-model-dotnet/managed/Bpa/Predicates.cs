// Copyright (c) Microsoft Corporation. Licensed under the MIT License.
// Translated from pinned Semantic Link Labs predicates; Upstream/LICENSE.txt applies.
using System;
using System.Linq;
using System.Text.RegularExpressions;
using Microsoft.AnalysisServices.Tabular;

namespace Fabric.SemanticModel.Bpa;

internal static class Predicates
{
    private static readonly TimeSpan RegexTimeout = TimeSpan.FromSeconds(2);
    private static readonly string[] Transformations =
    {
        "Table.Combine(\"", "Table.Join(\"", "Table.NestedJoin(\"", "Table.AddColumn(\"",
        "Table.Group(\"", "Table.Sort(\"", "Table.Pivot(\"", "Table.Unpivot(\"",
        "Table.UnpivotOtherColumns(\"", "Table.Distinct(\"", "[Query=(\"\"SELECT",
        "Value.NativeQuery", "OleDb.Query", "Odbc.Query"
    };
    private static readonly string[] DateFormats =
        { "mm/dd/yyyy", "mm-dd-yyyy", "dd/mm/yyyy", "dd-mm-yyyy", "yyyy-mm-dd", "yyyy/mm/dd" };

    internal static bool Evaluate(string id, object obj, EvaluationContext context)
    {
        return id switch
        {
            "labs-001" => ((Column)obj).DataType == DataType.Double,
            "labs-002" => ((Column)obj).Type == ColumnType.Calculated,
            "labs-003" => ManyToMany(Relationship(obj)) || Relationship(obj).CrossFilteringBehavior == CrossFilteringBehavior.BothDirections,
            "labs-004" => new[] { "USERPRINCIPALNAME()", "USERNAME()" }
                .Any(p => Match(p, EvaluationContext.Text(((TablePermission)obj).FilterExpression))),
            "labs-005" => context.UsedRelationships(obj).Any(ManyToMany)
                && context.Rls.Any(r => r.Name == ((Table)obj).Name),
            "labs-006" => ManyToMany(Relationship(obj))
                && Relationship(obj).CrossFilteringBehavior == CrossFilteringBehavior.BothDirections,
            "labs-007" => !context.IsDirectLake && ((Column)obj).IsAvailableInMDX
                && Hidden((Column)obj) && ((Column)obj).SortByColumn is null
                && !EvaluationContext.UsedSortBy((Column)obj) && !context.UsedHierarchy((Column)obj),
            "labs-008" => EvaluationContext.IsHybrid(((Partition)obj).Table)
                && ((Partition)obj).Mode == ModeType.DirectQuery && ((Partition)obj).DataCoverageDefinition is null,
            "labs-009" => !context.Partitions.Any(p => p.Mode == ModeType.DirectQuery)
                && context.Partitions.Any(p => p.Mode == ModeType.Dual),
            "labs-010" => ((Table)obj).Partitions.Count(p => p.Mode == ModeType.Import) == 1
                && ((Table)obj).Partitions.Count == 1 && context.Tables.Any(EvaluationContext.IsHybrid)
                && context.UsedRelationships(obj).Any(r => r.ToCardinality == RelationshipEndCardinality.One
                    && r.ToTable.Name == ((Table)obj).Name),
            "labs-011" => ((Partition)obj).SourceType == PartitionSourceType.M
                && Transformations.Any(t => EvaluationContext.PartitionExpression((Partition)obj).Contains(t, StringComparison.Ordinal)),
            "labs-012" => ((Table)obj).CalculationGroup is null
                && context.UsedRelationships(obj).Any(r => r.FromTable.Name == ((Table)obj).Name)
                && context.UsedRelationships(obj).Any(r => r.ToTable.Name == ((Table)obj).Name),
            "labs-013" => context.IsDirectLake && context.ViewTables().Count > 0,
            "labs-014" => ((Partition)obj).Mode == ModeType.DirectLake
                && context.ViewTables().Contains(((Partition)obj).Table.Name),
            "labs-015" => ((Partition)obj).Mode == ModeType.DirectLake
                && context.NonVOrderedTables().Contains(((Partition)obj).Table.Name),
            "labs-016" => AddedZero((Measure)obj),
            "labs-017" => !context.IsFieldParameter((Table)obj) && EvaluationContext.IsCalculated((Table)obj),
            "labs-018" => ((Column)obj).Type == ColumnType.Calculated
                && Match(@"related\s*\(", EvaluationContext.Expression(obj)),
            "labs-019" => (context.Relationships.Count(r => Relationship(r).CrossFilteringBehavior == CrossFilteringBehavior.BothDirections)
                + context.Relationships.Count(r => ManyToMany(Relationship(r))))
                / (double)Math.Max(context.Relationships.Length, 1) > 0.3,
            "labs-020" => EvaluationContext.IsCalculated((Table)obj)
                && (((Table)obj).Name.StartsWith("DateTableTemplate_", StringComparison.Ordinal)
                    || ((Table)obj).Name.StartsWith("LocalDateTable_", StringComparison.Ordinal)),
            "labs-021" => (Match("date", ((Table)obj).Name) || Match("calendar", ((Table)obj).Name))
                && ((Table)obj).DataCategory != "Time",
            "labs-022" => !context.IsDirectLake && ((Table)obj).Partitions.Count == 1
                && context.RowCount((Table)obj) > 25000000,
            "labs-023" => new[] { "right(", "left(", "filter(", "upper(", "lower(", "find(" }
                .Any(s => EvaluationContext.Text(((TablePermission)obj).FilterExpression).ToLowerInvariant().Contains(s, StringComparison.Ordinal)),
            "labs-024" => !context.Tables.Any(t => t.Columns.Any(c =>
                c.IsKey && c.DataType == DataType.DateTime && t.DataCategory == "Time")),
            "labs-025" => EvaluationContext.Text(((CalculationItem)obj).Expression).Length == 0,
            "labs-026" => Relationship(obj).FromColumn.DataType != Relationship(obj).ToColumn.DataType,
            "labs-027" => ((Column)obj).Type == ColumnType.Data
                && EvaluationContext.Text(((DataColumn)obj).SourceColumn).Length == 0,
            "labs-028" => !context.IsDirectLake && !((Column)obj).IsAvailableInMDX
                && (EvaluationContext.UsedSortBy((Column)obj) || context.UsedHierarchy((Column)obj)
                    || ((Column)obj).SortByColumn is not null),
            "labs-029" => context.Measures.Any(m => Match(
                @"USERELATIONSHIP\s*\(\s*.+?(?=])\]\s*,\s*'*" + Regex.Escape(((Table)obj).Name) + @"'*\[",
                EvaluationContext.Text(m.Expression)))
                && context.Rls.Any(r => r.Table.Name == ((Table)obj).Name),
            "labs-030" => Match(@"iferror\s*\(", EvaluationContext.Expression(obj)),
            "labs-031" => Match(@"intersect\s*\(", EvaluationContext.Expression(obj)),
            "labs-032" => Match(@"evaluateandlog\s*\(", EvaluationContext.Expression(obj)),
            "labs-033" => context.Measures.Any(m => EvaluationContext.Expression(obj) == "[" + m.Name + "]"),
            "labs-034" => context.Measures.Any(m => StripWhitespace(EvaluationContext.Expression(obj))
                == StripWhitespace(EvaluationContext.Text(m.Expression)) && ((Measure)obj).Name != m.Name),
            "labs-035" => Match(@"DIVIDE\s*\((\s*.*?)\)\s*[+-]\s*1|\/\s*.*(?=[-+]\s*1)", EvaluationContext.Expression(obj)),
            "labs-036" => Match(@"[0-9]+\s*[-+]\s*[\(]*\s*SUM\s*\(\s*\'*[A-Za-z0-9 _]+\'*\s*\[[A-Za-z0-9 _]+\]\s*\)\s*/",
                EvaluationContext.Expression(obj))
                || Match(@"[0-9]+\s*[-+]\s*DIVIDE\s*\(", EvaluationContext.Expression(obj)),
            "labs-037" => Match(@"CALCULATE\s*\(\s*[^,]+,\s*FILTER\s*\(\s*\'*[A-Za-z0-9 _]+\'*\s*,\s*\[[^\]]+\]",
                EvaluationContext.Expression(obj))
                || Match(@"CALCULATETABLE\s*\(\s*[^,]*,\s*FILTER\s*\(\s*\'*[A-Za-z0-9 _]+\'*\s*,\s*\[",
                    EvaluationContext.Expression(obj)),
            "labs-038" => Match(@"CALCULATE\s*\(\s*[^,]+,\s*FILTER\s*\(\s*'*[A-Za-z0-9 _]+'*\s*,\s*'*[A-Za-z0-9 _]+'*\[[A-Za-z0-9 _]+\]",
                EvaluationContext.Expression(obj))
                || Match(@"CALCULATETABLE\s*\([^,]*,\s*FILTER\s*\(\s*'*[A-Za-z0-9 _]+'*\s*,\s*'*[A-Za-z0-9 _]+'*\[[A-Za-z0-9 _]+\]",
                    EvaluationContext.Expression(obj)),
            "labs-039" => Match(@"\]\s*\/(?!\/)(?!\*)|\)\s*\/(?!\/)(?!\*)", EvaluationContext.Expression(obj)),
            "labs-040" => context.UnqualifiedColumns(obj),
            "labs-041" => context.FullyQualifiedMeasures(obj),
            "labs-042" => SumAverageOnString(obj, context),
            "labs-043" => !Relationship(obj).IsActive && !Activated(Relationship(obj), context),
            "labs-044" => Hidden((Column)obj) && !context.UsedRelationships(obj).Any()
                && !context.UsedHierarchy((Column)obj) && !EvaluationContext.UsedSortBy((Column)obj)
                && context.DependsOn(obj).Any(),
            "labs-045" => ((Measure)obj).IsHidden && !context.ReferencedBy((Measure)obj).Any(),
            "labs-046" => !context.UsedRelationships(obj).Any() && ((Table)obj).CalculationGroup is null
                && !context.IsFieldParameter((Table)obj),
            "labs-047" => ((Table)obj).CalculationGroup is not null
                && !((Table)obj).CalculationGroup.CalculationItems.Any(),
            "labs-048" => VisibleWithoutDescription(obj),
            "labs-049" => Match("date", ((Column)obj).Name) && ((Column)obj).DataType == DataType.DateTime
                && !DateFormats.Contains(EvaluationContext.Text(((Column)obj).FormatString).ToLowerInvariant(), StringComparer.Ordinal),
            "labs-050" => ((Column)obj).DataType is DataType.Int64 or DataType.Decimal or DataType.Double
                && ((Column)obj).SummarizeBy != AggregateFunction.None && !Hidden((Column)obj),
            "labs-051" => !((Measure)obj).IsHidden && EvaluationContext.Text(((Measure)obj).FormatString).Length == 0
                && ((Measure)obj).FormatStringDefinition is null,
            "labs-052" => EvaluationContext.Text(((Column)obj).DataCategory).Length == 0
                && new[] { "country", "city", "continent", "latitude", "longitude" }
                    .Any(p => ((Column)obj).Name.ToLowerInvariant().StartsWith(p, StringComparison.Ordinal)),
            "labs-053" => EvaluationContext.Text(((Measure)obj).FormatString).Contains('%')
                && ((Measure)obj).FormatString != "#,0.0%;-#,0.0%;#,0.0%",
            "labs-054" => !EvaluationContext.Text(((Measure)obj).FormatString).Contains('$')
                && !((Measure)obj).FormatString.Contains('%') && ((Measure)obj).FormatString is not ("#,0" or "#,0.0"),
            "labs-055" => !((Column)obj).IsHidden && context.UsedRelationships(obj).Any(r =>
                r.FromColumn.Name == ((Column)obj).Name && r.FromCardinality == RelationshipEndCardinality.Many),
            "labs-056" => context.UsedRelationships(obj).Any(r => r.ToTable.Name == ((Column)obj).Table.Name
                && r.ToColumn.Name == ((Column)obj).Name && r.ToCardinality == RelationshipEndCardinality.One)
                && !((Column)obj).IsKey && ((Column)obj).Table.DataCategory != "Time",
            // Python str(None) is "None", not "". This deliberately retains the pinned non-match.
            "labs-057" => Match("month", ((Column)obj).Name) && !Match("months", ((Column)obj).Name)
                && ((Column)obj).DataType == DataType.String
                && (((Column)obj).SortByColumn?.ToString() ?? "None").Length == 0,
            "labs-058" => Relationship(obj).FromColumn.DataType != DataType.Int64
                || Relationship(obj).ToColumn.DataType != DataType.Int64,
            "labs-059" => Match("month", ((Column)obj).Name) && ((Column)obj).DataType == DataType.DateTime
                && ((Column)obj).FormatString != "MMMM yyyy",
            "labs-060" => (((Column)obj).Name.ToLowerInvariant().StartsWith("is", StringComparison.Ordinal)
                && ((Column)obj).DataType == DataType.Int64 && !Hidden((Column)obj))
                || (((Column)obj).Name.ToLowerInvariant().EndsWith(" flag", StringComparison.Ordinal)
                    && ((Column)obj).DataType != DataType.String && !Hidden((Column)obj)),
            "labs-061" => Name(obj)[0] == ' ' || Name(obj)[^1] == ' ',
            "labs-062" => FirstLetterNotCapitalized(Name(obj)),
            "labs-063" => Match(@"[\t\r\n]", Name(obj)),
            "labs-064" => ((Partition)obj).Table.Partitions.Count == 1 && ((Partition)obj).Name != ((Partition)obj).Table.Name,
            _ => throw new InvalidOperationException("No predicate for pinned rule " + id)
        };
    }

    internal static bool Match(string pattern, string expression, bool ignoreCase = true) =>
        Regex.IsMatch(ignoreCase ? PythonRegex.Fold(expression) : expression,
            PythonRegex.Whitespace(ignoreCase ? PythonRegex.Fold(pattern, true) : pattern),
            RegexOptions.CultureInvariant, RegexTimeout);

    private static string StripWhitespace(string expression) =>
        PythonRegex.StripWhitespace(expression, RegexTimeout);
    private static bool ManyToMany(SingleColumnRelationship r) =>
        r.FromCardinality == RelationshipEndCardinality.Many && r.ToCardinality == RelationshipEndCardinality.Many;
    private static SingleColumnRelationship Relationship(object obj) => obj as SingleColumnRelationship
        ?? throw new EvidenceException("relationship_type_unsupported", "TOM.SingleColumnRelationship");
    private static bool Hidden(Column column) => column.IsHidden || column.Table.IsHidden;

    private static bool AddedZero(Measure measure)
    {
        string expression = EvaluationContext.Text(measure.Expression);
        string compact = expression.Replace(" ", "", StringComparison.Ordinal);
        return compact.StartsWith("0+", StringComparison.Ordinal) || compact.EndsWith("+0", StringComparison.Ordinal)
            || Match(@"DIVIDE\s*\(\s*[^,]+,\s*[^,]+,\s*0\s*\)", expression)
            || Match(@"IFERROR\s*\(\s*[^,]+,\s*0\s*\)", expression);
    }

    private static bool SumAverageOnString(object obj, EvaluationContext context)
    {
        string expression = EvaluationContext.Expression(obj);
        var strings = context.Columns.Where(c => c.DataType == DataType.String).Select(c => c.Name)
            .ToHashSet(StringComparer.Ordinal);
        if (expression.Length == 0 || strings.Count == 0) return false;
        const string pattern = @"(?<![A-Za-z0-9_])(?:SUM|AVERAGE)\s*\(\s*(?:'[^']+'|[A-Za-z0-9_]+)?\[([^\]]+)\]\s*\)";
        // Keep capture text in its original case: upstream matches function/table case-insensitively
        // but compares the captured column name against the string-column set case-sensitively.
        var folded = PythonRegex.Fold(expression);
        return Regex.Matches(folded, PythonRegex.Whitespace(PythonRegex.Fold(pattern, true)),
                RegexOptions.CultureInvariant, RegexTimeout)
            .Cast<Match>().Any(m => strings.Contains(PythonRegex.Trim(
                expression.Substring(m.Groups[1].Index, m.Groups[1].Length))));
    }

    private static bool Activated(SingleColumnRelationship r, EvaluationContext context) => context.Measures.Any(m => Match(
        @"USERELATIONSHIP\s*\(\s*\'*" + Regex.Escape(r.FromTable.Name) + @"'*\[" + Regex.Escape(r.FromColumn.Name)
        + @"\]\s*,\s*'*" + Regex.Escape(r.ToTable.Name) + @"'*\[" + Regex.Escape(r.ToColumn.Name) + @"\]",
        EvaluationContext.Text(m.Expression)));

    private static bool VisibleWithoutDescription(object obj) => obj switch
    {
        Column c => !c.IsHidden && EvaluationContext.Text(c.Description).Length == 0,
        Measure m => !m.IsHidden && EvaluationContext.Text(m.Description).Length == 0,
        Table t => !t.IsHidden && EvaluationContext.Text(t.Description).Length == 0,
        _ => throw new InvalidOperationException("Invalid visible-object scope.")
    };

    private static string Name(object obj) => obj switch
    {
        Table t => t.Name, Column c => c.Name, Measure m => m.Name,
        Partition p => p.Name, Hierarchy h => h.Name,
        _ => throw new InvalidOperationException("Invalid naming scope.")
    };

    private static bool FirstLetterNotCapitalized(string name)
    {
        var first = System.Text.Rune.GetRuneAt(name, 0);
        // Python upper() permits expanding mappings, unlike char.ToUpperInvariant().
        string value = first.ToString();
        return UnicodeCaseData.ExpandingUpper.Contains(first.Value) || value != System.Text.Rune.ToUpperInvariant(first).ToString();
    }
}

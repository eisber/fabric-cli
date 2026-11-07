// Copyright (c) Microsoft Corporation. Licensed under the MIT License.
// Ported helper semantics: see Upstream/LICENSE.txt and README.md.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Text.RegularExpressions;
using Microsoft.AnalysisServices.Tabular;

namespace Fabric.SemanticModel.Bpa;

internal sealed class EvaluationContext
{
    private readonly JsonElement evidence;
    private readonly bool dependencies;
    private JsonElement[]? dependencyRows;
    private HashSet<string>? viewTables;
    private HashSet<string>? nonVOrderedTables;

    internal EvaluationContext(Model model, JsonElement options, JsonElement evidence)
    {
        Model = model;
        if (options.ValueKind is not (JsonValueKind.Undefined or JsonValueKind.Null or JsonValueKind.Object))
            throw new EvidenceException("invalid_options", "options", true);
        if (evidence.ValueKind is not (JsonValueKind.Undefined or JsonValueKind.Null or JsonValueKind.Object))
            throw new EvidenceException("invalid_evidence", "evidence", true);
        _ = Option(options, "extended", false);
        dependencies = Option(options, "dependencies", true);
        ValidateVersion(options);
        ValidateVersion(evidence);
        this.evidence = evidence;
        Tables = model.Tables.ToArray();
        Columns = Tables.SelectMany(t => t.Columns).Where(c => c.Type != ColumnType.RowNumber).ToArray();
        Measures = Tables.SelectMany(t => t.Measures).ToArray();
        Partitions = Tables.SelectMany(t => t.Partitions).ToArray();
        Relationships = model.Relationships.ToArray();
        Rls = model.Roles.SelectMany(r => r.TablePermissions).ToArray();
    }

    internal Model Model { get; }
    internal Table[] Tables { get; }
    internal Column[] Columns { get; }
    internal Measure[] Measures { get; }
    internal Partition[] Partitions { get; }
    internal Relationship[] Relationships { get; }
    internal TablePermission[] Rls { get; }
    internal bool IsDirectLake => Partitions.Any(p => p.Mode == ModeType.DirectLake);

    internal IEnumerable<object> Objects(string scope) => scope switch
    {
        "Model" => new object[] { Model },
        "Table" => Tables,
        "Column" => Columns,
        "Calculated Table" => Tables.Where(IsCalculated),
        "Calculated Column" => Columns.Where(c => c.Type == ColumnType.Calculated),
        "Measure" => Measures,
        "Partition" => Partitions,
        "Relationship" => Relationships,
        "Row Level Security" => Rls,
        "Hierarchy" => Tables.SelectMany(t => t.Hierarchies),
        "Calculation Item" => Tables.Where(t => t.CalculationGroup is not null)
            .SelectMany(t => t.CalculationGroup.CalculationItems),
        _ => throw new InvalidOperationException("Unsupported pinned scope.")
    };

    internal string ObjectName(string scope, object obj) => obj switch
    {
        Model _ => "Model",
        Table t => t.Name,
        Column c => DaxName(c.Table.Name, c.Name),
        Measure m => m.Name,
        Partition p => DaxName(p.Table.Name, p.Name),
        Hierarchy h => DaxName(h.Table.Name, h.Name),
        CalculationItem i => DaxName(i.CalculationGroup.Table.Name, i.Name),
        TablePermission p => DaxName(p.Role.Name, p.Name),
        SingleColumnRelationship r => DaxName(r.FromTable.Name, r.FromColumn.Name)
            + " -> " + DaxName(r.ToTable.Name, r.ToColumn.Name),
        _ => throw new InvalidOperationException("Object has no pinned name formatter: " + scope)
    };

    internal static string DaxName(string table, string column) => "'" + table + "'[" + column + "]";
    internal static bool IsCalculated(Table table) =>
        table.Partitions.Any(p => p.SourceType == PartitionSourceType.Calculated);
    internal static bool IsHybrid(Table table) =>
        table.Partitions.Any(p => p.Mode == ModeType.Import)
        && table.Partitions.Any(p => p.Mode == ModeType.DirectQuery);

    internal static string PartitionExpression(Partition partition) => partition.Source switch
    {
        CalculatedPartitionSource s => Text(s.Expression),
        MPartitionSource s => Text(s.Expression),
        _ => throw new EvidenceException("expression_unavailable", "TOM.partition.source.expression")
    };

    internal static string Expression(object obj) => obj switch
    {
        Measure m => Text(m.Expression),
        CalculatedColumn c => Text(c.Expression),
        CalculationItem i => Text(i.Expression),
        Table t => CalculatedTableExpression(t),
        _ => throw new EvidenceException("expression_unavailable", "TOM.expression")
    };

    // TOM string getters normally normalize unset metadata to "". Do not normalize an actual null.
    internal static string Text(string? value) => value
        ?? throw new EvidenceException("metadata_missing", "TOM.string");

    private static string CalculatedTableExpression(Table table)
    {
        var first = table.Partitions.First();
        if (first.SourceType != PartitionSourceType.Calculated)
            throw new EvidenceException("expression_unavailable", "TOM.first_partition.calculated_expression");
        return PartitionExpression(first);
    }

    internal bool IsFieldParameter(Table table) =>
        IsCalculated(table)
        && table.Columns.Count is 4 or 5
        && table.Partitions.Any(p => PartitionExpression(p).Replace(" ", "", StringComparison.Ordinal)
            .Contains("NAMEOF(", StringComparison.Ordinal))
        && table.Columns.OfType<DataColumn>().All(c => Text(c.SourceColumn).Contains("[Value", StringComparison.Ordinal))
        && table.Columns.Any(c => c.ExtendedProperties.Any(p => p.Name == "ParameterMetadata"));

    internal IEnumerable<SingleColumnRelationship> UsedRelationships(object obj)
    {
        foreach (var relationship in Relationships)
        {
            if (relationship is not SingleColumnRelationship r)
                throw new EvidenceException("relationship_type_unsupported", "TOM.SingleColumnRelationship");
            if (obj is Table t && (r.FromTable.Name == t.Name || r.ToTable.Name == t.Name))
                yield return r;
            else if (obj is Column c
                && ((r.FromTable.Name == c.Table.Name && r.FromColumn.Name == c.Name)
                    || (r.ToTable.Name == c.Table.Name && r.ToColumn.Name == c.Name)))
                yield return r;
        }
    }

    internal bool UsedHierarchy(Column column) => Tables.SelectMany(t => t.Hierarchies)
        .SelectMany(h => h.Levels)
        .Any(l => l.Hierarchy.Table.Name == column.Table.Name && l.Column.Name == column.Name);

    internal static bool UsedSortBy(Column column) =>
        column.Table.Columns.Any(c => ReferenceEquals(c.SortByColumn, column));

    internal long RowCount(Table table)
    {
        var rows = Rows(Block(evidence, "extendedStatistics"), "tables", "extendedStatistics.tables");
        foreach (var row in rows)
        {
            string name = String(row, "tableName", "extendedStatistics.tables");
            if (name != table.Name) continue;
            if (!row.TryGetProperty("rowCount", out var count) || !count.TryGetInt64(out long value) || value < 0)
                throw new EvidenceException("invalid_row_count", "extendedStatistics.tables.rowCount", true);
            return value;
        }
        throw new EvidenceException("table_statistics_missing", "extendedStatistics.tables:" + table.Name);
    }

    internal HashSet<string> ViewTables()
    {
        if (viewTables is not null) return viewTables;
        var rows = Rows(Block(Block(evidence, "directLake", false), "fallbackReasons"),
            "rows", "directLake.fallbackReasons.rows");
        var names = new HashSet<string>(StringComparer.Ordinal);
        foreach (var row in rows)
        {
            string table = String(row, "Table Name", "directLake.fallbackReasons.rows");
            if (!row.TryGetProperty("FallbackReasonID", out var reason)
                || reason.ValueKind != JsonValueKind.Number || !reason.TryGetDouble(out double id))
                throw new EvidenceException("invalid_fallback_reason", "directLake.fallbackReasons.rows.FallbackReasonID", true);
            if (id == 2) names.Add(table);
        }
        return viewTables = names;
    }

    internal HashSet<string> NonVOrderedTables()
    {
        if (nonVOrderedTables is not null) return nonVOrderedTables;
        var block = Block(Block(evidence, "directLake", false), "vOrder");
        if (!block.TryGetProperty("lakehouseSource", out var source)
            || source.ValueKind is not (JsonValueKind.True or JsonValueKind.False))
            throw new EvidenceException("lakehouse_source_missing", "directLake.vOrder.lakehouseSource");
        var rows = Rows(block, "rows", "directLake.vOrder.rows");
        var names = new HashSet<string>(StringComparer.Ordinal);
        if (source.ValueKind == JsonValueKind.False) return nonVOrderedTables = names;
        foreach (var p in Partitions.Where(p => p.Mode == ModeType.DirectLake))
        {
            bool found = false;
            foreach (var row in rows)
            {
                if (String(row, "tableName", "directLake.vOrder.rows") != p.Table.Name
                    || String(row, "partitionName", "directLake.vOrder.rows") != p.Name) continue;
                found = true;
                if (row.TryGetProperty("status", out var rowStatus))
                {
                    if (rowStatus.GetString() == "failed")
                        throw new EvidenceException("source_inspection_failed", "directLake.vOrder.rows", true);
                    if (rowStatus.GetString() == "unavailable")
                        throw new EvidenceException("source_inspection_unavailable", "directLake.vOrder.rows");
                    if (rowStatus.GetString() != "available")
                        throw new EvidenceException("invalid_evidence_status", "directLake.vOrder.rows", true);
                }
                if (!row.TryGetProperty("isVOrdered", out var ordered)
                    || ordered.ValueKind is not (JsonValueKind.True or JsonValueKind.False))
                    throw new EvidenceException("vorder_missing", "directLake.vOrder.rows.isVOrdered");
                if (!ordered.GetBoolean()) names.Add(p.Table.Name);
            }
            if (!found)
                throw new EvidenceException("partition_source_missing", "directLake.vOrder.rows:" + DaxName(p.Table.Name, p.Name));
        }
        return nonVOrderedTables = names;
    }

    internal IEnumerable<object> DependsOn(object obj, bool directOnly = false)
    {
        var (table, name, types) = DependencyIdentity(obj);
        var rows = Dependencies().Where(r => S(r, "Table Name") == table && S(r, "Object Name") == name
            && types.Contains(S(r, "Object Type"), StringComparer.Ordinal)
            && (!directOnly || S(r, "Object Name") == S(r, "Parent Node"))).ToArray();
        foreach (var measure in Measures)
            if (rows.Any(r => S(r, "Referenced Object Type") == "Measure" && S(r, "Referenced Object") == measure.Name))
                yield return measure;
        foreach (var column in Columns)
            if (rows.Any(r => S(r, "Referenced Object Type") is "Column" or "Calc Column"
                && S(r, "Referenced Full Object Name") == DaxName(column.Table.Name, column.Name)))
                yield return column;
        foreach (var t in Tables)
            if (rows.Any(r => S(r, "Referenced Object Type") is "Table" or "Calc Table" && S(r, "Referenced Table") == t.Name))
                yield return t;
    }

    internal IEnumerable<object> ReferencedBy(Measure measure)
    {
        var rows = Dependencies().Where(r => S(r, "Referenced Object Type") == "Measure"
            && S(r, "Referenced Table") == measure.Table.Name && S(r, "Referenced Object") == measure.Name).ToArray();
        foreach (var m in Measures)
            if (rows.Any(r => S(r, "Object Type") == "Measure" && S(r, "Object Name") == m.Name)) yield return m;
        foreach (var c in Columns)
            if (rows.Any(r => S(r, "Object Type") is "Column" or "Calc Column"
                && S(r, "Full Object Name") == DaxName(c.Table.Name, c.Name))) yield return c;
        foreach (var t in Tables)
            if (rows.Any(r => S(r, "Object Type") is "Table" or "Calc Table" && S(r, "Table Name") == t.Name)) yield return t;
    }

    internal bool UnqualifiedColumns(object obj)
    {
        var expression = Expression(obj);
        foreach (var c in DependsOn(obj, true).OfType<Column>())
        {
            string pattern = string.Concat(Columns.Where(x => x.Name == c.Name)
                .Select(x => "(?<!" + Regex.Escape(x.Table.Name) + ")(?<!'" + Regex.Escape(x.Table.Name) + "')"))
                + Regex.Escape("[" + c.Name + "]");
            if (Predicates.Match(pattern, expression, false)) return true;
        }
        return false;
    }

    internal bool FullyQualifiedMeasures(object obj)
    {
        var expression = Expression(obj);
        return DependsOn(obj, true).OfType<Measure>().Any(m =>
            expression.Contains(m.Table.Name + "[" + m.Name + "]", StringComparison.Ordinal)
            || expression.Contains(DaxName(m.Table.Name, m.Name), StringComparison.Ordinal));
    }

    private JsonElement[] Dependencies()
    {
        if (!dependencies) throw new EvidenceException("dependencies_disabled", "options.dependencies=true");
        if (dependencyRows is not null) return dependencyRows;
        var rows = Rows(Block(evidence, "dependencies"), "rows", "dependencies.rows");
        string[] columns = { "Table Name", "Object Name", "Object Type", "Expression", "Referenced Table",
            "Referenced Object", "Referenced Object Type", "Full Object Name", "Referenced Full Object Name", "Parent Node" };
        foreach (var row in rows)
            foreach (string column in columns) _ = String(row, column, "dependencies.rows");
        return dependencyRows = rows;
    }

    private static (string Table, string Name, string[] Types) DependencyIdentity(object obj) => obj switch
    {
        Measure m => (m.Table.Name, m.Name, new[] { "Measure" }),
        Column c => (c.Table.Name, c.Name, new[] { "Column", "Calc Column" }),
        Table t => (t.Name, t.Name, new[] { "Table", "Calc Table" }),
        CalculationItem i => (i.CalculationGroup.Table.Name, i.Name, new[] { "Calculation Item" }),
        _ => throw new EvidenceException("dependency_object_unsupported", "TOM.dependency_object", true)
    };

    private static string S(JsonElement row, string property) => String(row, property, "dependencies.rows");

    private static JsonElement Block(JsonElement parent, string property, bool withStatus = true)
    {
        if (parent.ValueKind != JsonValueKind.Object || !parent.TryGetProperty(property, out var block))
            throw new EvidenceException("evidence_missing", property);
        if (block.ValueKind != JsonValueKind.Object)
            throw new EvidenceException("invalid_evidence", property, true);
        if (!withStatus) return block;
        if (!block.TryGetProperty("status", out var status) || status.ValueKind != JsonValueKind.String)
            throw new EvidenceException("evidence_status_missing", property + ".status");
        return status.GetString() switch
        {
            "available" => block,
            "unavailable" => throw new EvidenceException("evidence_unavailable", property),
            "failed" => throw new EvidenceException("evidence_provider_failed", property, true),
            _ => throw new EvidenceException("invalid_evidence_status", property + ".status", true)
        };
    }

    private static JsonElement[] Rows(JsonElement block, string name, string required)
    {
        if (!block.TryGetProperty(name, out var rows))
            throw new EvidenceException("evidence_rows_missing", required);
        if (rows.ValueKind != JsonValueKind.Array)
            throw new EvidenceException("invalid_evidence_rows", required, true);
        var result = rows.EnumerateArray().ToArray();
        if (result.Any(r => r.ValueKind != JsonValueKind.Object))
            throw new EvidenceException("invalid_evidence_row", required, true);
        return result;
    }

    private static string String(JsonElement row, string property, string required)
    {
        if (!row.TryGetProperty(property, out var value))
            throw new EvidenceException("evidence_field_missing", required + "." + property);
        if (value.ValueKind != JsonValueKind.String)
            throw new EvidenceException("invalid_evidence_field", required + "." + property, true);
        return value.GetString()!;
    }

    private static bool Option(JsonElement options, string name, bool fallback)
    {
        if (options.ValueKind != JsonValueKind.Object || !options.TryGetProperty(name, out var value)) return fallback;
        if (value.ValueKind is not (JsonValueKind.True or JsonValueKind.False))
            throw new EvidenceException("invalid_option", "options." + name, true);
        return value.GetBoolean();
    }

    private static void ValidateVersion(JsonElement element)
    {
        if (element.ValueKind != JsonValueKind.Object || !element.TryGetProperty("sourceCommit", out var version)) return;
        if (version.ValueKind != JsonValueKind.String || version.GetString() != BpaEngine.SourceCommit)
            throw new EvidenceException("unsupported_source_version", "sourceCommit", true);
    }
}

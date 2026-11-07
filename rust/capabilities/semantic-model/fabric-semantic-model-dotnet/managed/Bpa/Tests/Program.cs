// Copyright (c) Microsoft Corporation. Licensed under the MIT License.
// Offline synthetic tests; these do not replace the independently owned source oracle.
using System.Text.Json;
using Fabric.SemanticModel.Bpa;
using Microsoft.AnalysisServices.Tabular;
using JsonSerializer = System.Text.Json.JsonSerializer;

internal static class Program
{
    private static int assertions;

    private static int Main()
    {
        CatalogAndContract();
        MetadataPredicates();
        ExpressionPredicates();
        DependenciesAndSources();
        Console.WriteLine($"BPA synthetic checks passed: {assertions} assertions.");
        return 0;
    }

    private static JsonElement Json(string value) => JsonDocument.Parse(value).RootElement.Clone();

    private static void Equal<T>(T expected, T actual, string label)
    {
        assertions++;
        if (!EqualityComparer<T>.Default.Equals(expected, actual))
            throw new InvalidOperationException($"{label}: expected {expected}, got {actual}");
    }

    private static bool Rule(int id, object obj, Fixture fixture, string options = "{}", string evidence = "{}") =>
        Predicates.Evaluate($"labs-{id:000}", obj,
            new EvaluationContext(fixture.Database.Model, Json(options), Json(evidence)));

    private static JsonElement Analyze(Fixture fixture, string options = "{}", string evidence = "{}") =>
        Json(BpaEngine.Analyze(fixture.Database, Json(options), Json(evidence)));

    private static JsonElement Result(JsonElement result, int id) => result.GetProperty("rules")
        .EnumerateArray().Single(r => r.GetProperty("id").GetString()
            == BpaEngine.LoadCatalog().Single(rule => rule.PredicateId == $"labs-{id:000}").Id);

    private static void CatalogAndContract()
    {
        var catalog = BpaEngine.LoadCatalog();
        Equal(64, catalog.Length, "inventory");
        Equal(87, catalog.Sum(r => r.Scopes.Length), "rule/scope inventory");
        foreach (var r in catalog)
        {
            Equal(true, r.SourceLine > 0 && r.Predicate.StartsWith("lambda obj, tom:"), r.Id + " provenance");
            Equal(true, r.RequiredData.Length > 0, r.Id + " input ledger");
            string identity = r.RuleName + "\0" + string.Join("\0", r.Scopes.OrderBy(s => s, StringComparer.Ordinal));
            string stableId = "labs:" + Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(
                System.Text.Encoding.UTF8.GetBytes(identity))).ToLowerInvariant();
            Equal(stableId, r.Id, r.PredicateId + " stable name/scope identity");
        }
        var f = new Fixture();
        var result = Analyze(f);
        Equal("partial", result.GetProperty("status").GetString(), "missing evidence is partial");
        Equal("unavailable", Result(result, 22).GetProperty("status").GetString(), "missing row count");
        Equal(64, result.GetProperty("rules").GetArrayLength(), "all rules returned");
        Equal(0, result.GetProperty("coverage").GetProperty("failed").GetInt32(), "metadata scan has no errors");
        result = Analyze(f, """{"dependencies":true,"extended":true}""",
            """{"dependencies":{"status":"available","rows":[]},"extendedStatistics":{"status":"available","tables":[{"tableName":"Sales","rowCount":0}]}}""");
        Equal("succeeded", result.GetProperty("status").GetString(), "known-empty evidence completes metadata");
        Equal(64, result.GetProperty("coverage").GetProperty("evaluated").GetInt32(), "full rule evaluation");
        string before = JsonSerializer.Serialize(new { ColumnName = f.Amount.Name, f.Amount.DataType, TableName = f.Table.Name });
        _ = Analyze(f);
        Equal(before, JsonSerializer.Serialize(new { ColumnName = f.Amount.Name, f.Amount.DataType, TableName = f.Table.Name }), "read-only metadata");
        result = Analyze(f, """{"extended":"true"}""");
        Equal("failed", result.GetProperty("status").GetString(), "malformed option");
        result = Analyze(f, """{"sourceCommit":"not-the-pin"}""");
        Equal("unsupported_source_version", result.GetProperty("error").GetProperty("code").GetString(), "pin");
        f.Database.Model.Tables.Clear();
        result = Analyze(f);
        Equal("not_applicable_no_tables", result.GetProperty("outcome").GetString(), "empty model");
        Equal(0, result.GetProperty("coverage").GetProperty("evaluated").GetInt32(), "empty did not pass rules");
        Equal(64, result.GetProperty("coverage").GetProperty("unavailable").GetInt32(), "empty explicit coverage");
    }

    private static void MetadataPredicates()
    {
        var f = new Fixture();
        Equal(false, Rule(1, f.Amount, f), "int64");
        f.Amount.DataType = DataType.Double;
        Equal(true, Rule(1, f.Amount, f), "double");
        var calculated = new CalculatedColumn { Name = "Computed", DataType = DataType.Int64, Expression = "RELATED('Dim'[Key])" };
        f.Table.Columns.Add(calculated);
        Equal(true, Rule(2, calculated, f), "calculated");
        Equal(false, Rule(2, f.Amount, f), "data column");
        Equal(true, Rule(18, calculated, f), "related");
        Equal(false, Rule(18, f.Amount, f), "related data");
        var r = f.Relationship();
        Equal(false, Rule(3, r, f), "normal relationship");
        r.CrossFilteringBehavior = CrossFilteringBehavior.BothDirections;
        Equal(true, Rule(3, r, f), "bidi");
        Equal(false, Rule(6, r, f), "bidi not many to many");
        r.ToCardinality = RelationshipEndCardinality.Many;
        Equal(true, Rule(6, r, f), "bidi many many");
        Equal(true, Rule(19, f.Database.Model, f), "excessive relationships");
        f.Rls("USERNAME");
        Equal(true, Rule(4, f.Permission!, f), "unescaped upstream username regex");
        Equal(true, Rule(5, f.Table, f), "RLS many many");
        f.Permission!.FilterExpression = "LEFT([Key], 1) = \"A\"";
        Equal(true, Rule(23, f.Permission, f), "complex RLS");
        f.Permission.FilterExpression = "TRUE()";
        Equal(false, Rule(4, f.Permission, f), "static RLS");
        Equal(false, Rule(23, f.Permission, f), "simple RLS");
        f.Amount.IsHidden = true;
        f.Amount.IsAvailableInMDX = true;
        Equal(true, Rule(7, f.Amount, f), "unneeded MDX");
        Equal(false, Rule(28, f.Amount, f), "MDX available");
        f.Amount.SortByColumn = f.Key;
        f.Amount.IsAvailableInMDX = false;
        Equal(true, Rule(28, f.Amount, f), "sort needs MDX");
        Equal(false, Rule(7, f.Amount, f), "sort MDX not flagged");
        f.Partition.Mode = ModeType.Dual;
        Equal(true, Rule(9, f.Database.Model, f), "dual without DQ");
        f.Partition.Mode = ModeType.Import;
        var dq = new Partition { Name = "DQ", Mode = ModeType.DirectQuery, Source = new MPartitionSource { Expression = "let x=1 in x" } };
        f.Table.Partitions.Add(dq);
        Equal(true, Rule(8, dq, f), "hybrid coverage missing");
        Equal(false, Rule(8, f.Partition, f), "import coverage irrelevant");
        Equal(false, Rule(9, f.Database.Model, f), "DQ exists");
        r.ToCardinality = RelationshipEndCardinality.One;
        Equal(true, Rule(10, r.ToTable, f), "dimension import with hybrid");
        Equal(false, Rule(10, f.Table, f), "hybrid fact not dimension");
        ((MPartitionSource)f.Partition.Source).Expression = "Table.Join(\"x\")";
        Equal(true, Rule(11, f.Partition, f), "transformation exact");
        ((MPartitionSource)f.Partition.Source).Expression = "table.join(\"x\")";
        Equal(false, Rule(11, f.Partition, f), "transform case sensitive");
        Equal(false, Rule(12, f.Table, f), "non snowflake");
        var third = Fixture.NewTable("Third");
        third.Columns.Add(new DataColumn { Name = "Key", DataType = DataType.Int64, SourceColumn = "Key" });
        f.Database.Model.Tables.Add(third);
        f.Database.Model.Relationships.Add(new SingleColumnRelationship
        {
            Name = "Back", FromColumn = third.Columns["Key"], ToColumn = f.Key,
            FromCardinality = RelationshipEndCardinality.Many, ToCardinality = RelationshipEndCardinality.One
        });
        Equal(true, Rule(12, f.Table, f), "snowflake");
        var calcTable = Fixture.NewTable("DateTableTemplate_1");
        calcTable.Partitions.Add(new Partition { Name = "Calc", Source = new CalculatedPartitionSource { Expression = "CALENDAR(1,2)" } });
        f.Database.Model.Tables.Add(calcTable);
        Equal(true, Rule(17, calcTable, f), "calculated table");
        Equal(false, Rule(17, f.Table, f), "ordinary table");
        Equal(true, Rule(20, calcTable, f), "auto date");
        Equal(false, Rule(20, f.Table, f), "not auto date");
        Equal(true, Rule(21, calcTable, f), "date not marked");
        Equal(true, Rule(24, f.Database.Model, f), "no date table");
        calcTable.DataCategory = "Time";
        calcTable.Columns.Add(new DataColumn { Name = "Date", DataType = DataType.DateTime, IsKey = true, SourceColumn = "Date" });
        Equal(false, Rule(21, calcTable, f), "marked date");
        Equal(false, Rule(24, f.Database.Model, f), "date table");
        var group = Fixture.NewTable("Group");
        group.CalculationGroup = new CalculationGroup();
        f.Database.Model.Tables.Add(group);
        Equal(true, Rule(47, group, f), "empty calculation group");
        var item = new CalculationItem { Name = "Item", Expression = "" };
        group.CalculationGroup.CalculationItems.Add(item);
        Equal(true, Rule(25, item, f), "empty calculation item");
        Equal(false, Rule(47, group, f), "nonempty calculation group");
        item.Expression = "SELECTEDMEASURE()";
        Equal(false, Rule(25, item, f), "nonempty calculation item");
        Equal(false, Rule(26, r, f), "matching data types");
        r.ToColumn.DataType = DataType.String;
        Equal(true, Rule(26, r, f), "different types");
        Equal(true, Rule(58, r, f), "relationship non-int64");
        r.ToColumn.DataType = DataType.Int64;
        Equal(false, Rule(58, r, f), "integer relationship");
        f.Amount.SourceColumn = "";
        Equal(true, Rule(27, f.Amount, f), "missing source");
        f.Amount.SourceColumn = "Amount";
        Equal(false, Rule(27, f.Amount, f), "has source");
        Equal(true, Rule(46, calcTable, f), "no relationship");
        Equal(false, Rule(46, f.Table, f), "related");
        Equal(true, Rule(48, f.Table, f), "visible no description");
        f.Table.Description = "Sales fact";
        Equal(false, Rule(48, f.Table, f), "has description");
        var date = (DataColumn)calcTable.Columns["Date"];
        date.FormatString = "";
        Equal(true, Rule(49, date, f), "date format missing");
        date.FormatString = "YYYY-MM-DD";
        Equal(false, Rule(49, date, f), "date format case folded");
        f.Amount.IsHidden = false;
        f.Amount.SummarizeBy = AggregateFunction.Sum;
        Equal(true, Rule(50, f.Amount, f), "summarize numeric");
        f.Amount.SummarizeBy = AggregateFunction.None;
        Equal(false, Rule(50, f.Amount, f), "no summarization");
        var city = new DataColumn { Name = "City", DataType = DataType.String, DataCategory = "", SourceColumn = "City" };
        f.Table.Columns.Add(city);
        Equal(true, Rule(52, city, f), "category absent");
        city.DataCategory = "City";
        Equal(false, Rule(52, city, f), "category set");
        Equal(true, Rule(55, f.Key, f), "foreign key visible");
        f.Key.IsHidden = true;
        Equal(false, Rule(55, f.Key, f), "foreign key hidden");
        Equal(true, Rule(56, r.ToColumn, f), "mark primary key");
        r.ToColumn.IsKey = true;
        Equal(false, Rule(56, r.ToColumn, f), "primary key marked");
        city.Name = "Month";
        Equal(false, Rule(57, city, f), "str(None) preserved");
        city.SortByColumn = f.Key;
        Equal(false, Rule(57, city, f), "month sorted");
        date.Name = "Month";
        Equal(true, Rule(59, date, f), "month format");
        date.FormatString = "MMMM yyyy";
        Equal(false, Rule(59, date, f), "month format correct");
        f.Amount.Name = "IsEnabled";
        f.Amount.DataType = DataType.Int64;
        Equal(true, Rule(60, f.Amount, f), "integer is flag");
        f.Amount.IsHidden = true;
        Equal(false, Rule(60, f.Amount, f), "hidden flag");
        city.Name = " name ";
        Equal(true, Rule(61, city, f), "spaces");
        city.Name = "name";
        Equal(false, Rule(61, city, f), "no spaces");
        Equal(true, Rule(62, city, f), "lower initial");
        city.Name = "Name";
        Equal(false, Rule(62, city, f), "upper initial");
        city.Name = "ßName";
        Equal(true, Rule(62, city, f), "expanding uppercase initial");
        city.Name = "\U00010428Name";
        Equal(true, Rule(62, city, f), "supplementary lowercase initial");
        city.Name = "Na\tme";
        Equal(true, Rule(63, city, f), "tab");
        city.Name = "Name";
        Equal(false, Rule(63, city, f), "no special");
        Equal(false, Rule(64, f.Partition, f), "multi partition exempt");
        Equal(true, Rule(64, r.ToTable.Partitions[0], f), "single mismatched partition");
    }

    private static void ExpressionPredicates()
    {
        var f = new Fixture();
        var m = new Measure { Name = "Measure", Expression = "1", FormatString = "" };
        f.Table.Measures.Add(m);
        var other = new Measure { Name = "Other", Expression = "2", FormatString = "#,0" };
        f.Table.Measures.Add(other);
        (int Id, string Expression)[] positive =
        {
            (16, "DIVIDE([A],[B],0)"), (30, "IFERROR([A],0)"), (31, "INTERSECT(A,B)"),
            (32, "EVALUATEANDLOG(1)"), (33, "[Other]"), (34, " 2 "),
            (35, "DIVIDE([A],[B])+1"), (36, "1-DIVIDE([A],[B])"),
            (37, "CALCULATE([A],FILTER('Sales',[A]>0))"),
            (38, "CALCULATE([A],FILTER('Sales','Sales'[Amount]>0))"), (39, "[A]/[B]")
        };
        foreach (var (id, expression) in positive)
        {
            m.Expression = "42";
            Equal(false, Rule(id, m, f), id + " negative");
            m.Expression = expression;
            Equal(true, Rule(id, m, f), id + " positive");
        }
        f.Rls("TRUE()");
        m.Expression = "USERELATIONSHIP('Sales'[Key],'Sales'[Amount])";
        Equal(true, Rule(29, f.Table, f), "userelationship with RLS");
        m.Expression = "1";
        Equal(false, Rule(29, f.Table, f), "no userelationship");
        var relationship = f.Relationship();
        relationship.IsActive = false;
        Equal(true, Rule(43, relationship, f), "inactive never activated");
        m.Expression = "USERELATIONSHIP('Sales'[Key],'Dim'[Key])";
        Equal(false, Rule(43, relationship, f), "inactive activated");
        f.Table.Columns.Add(new DataColumn { Name = "Text", SourceColumn = "Text", DataType = DataType.String });
        m.Expression = "SUM('Sales'[Text])";
        Equal(true, Rule(42, m, f), "sum string");
        m.Expression = "SUM('Sales'[text])";
        Equal(false, Rule(42, m, f), "column name matching remains case sensitive");
        m.Expression = "CONSUM('Sales'[Text])";
        Equal(false, Rule(42, m, f), "sum token boundary");
        m.Expression = "ſUM('Sales'[Text])";
        Equal(true, Rule(42, m, f), "Python long-s IGNORECASE");
        m.Expression = "SUM\u001c('Sales'[Text])";
        Equal(true, Rule(42, m, f), "Python extra whitespace");
        m.Expression = "\u0130FERROR(1,0)";
        Equal(true, Rule(30, m, f), "Python dotted-I IGNORECASE");
        Equal(true, Rule(51, m, f), "measure format missing");
        m.FormatString = "#,0";
        Equal(false, Rule(51, m, f), "measure format set");
        m.FormatString = "0%";
        Equal(true, Rule(53, m, f), "percentage bad");
        m.FormatString = "#,0.0%;-#,0.0%;#,0.0%";
        Equal(false, Rule(53, m, f), "percentage exact");
        m.FormatString = "0";
        Equal(true, Rule(54, m, f), "whole number bad");
        m.FormatString = "#,0.0";
        Equal(false, Rule(54, m, f), "whole number accepted one decimal upstream");
    }

    private static void DependenciesAndSources()
    {
        var f = new Fixture();
        var m = new Measure { Name = "Total", Expression = "[Amount]", IsHidden = true };
        f.Table.Measures.Add(m);
        const string dependency = """
            {"dependencies":{"status":"available","rows":[{
              "Table Name":"Sales","Object Name":"Total","Object Type":"Measure","Expression":"[Amount]",
              "Referenced Table":"Sales","Referenced Object":"Amount","Referenced Object Type":"Column",
              "Full Object Name":"'Sales'[Total]","Referenced Full Object Name":"'Sales'[Amount]","Parent Node":"Total"
            }]}}
            """;
        Equal(true, Rule(40, m, f, evidence: dependency), "unqualified column dependency");
        m.Expression = "'Sales'[Amount]";
        Equal(false, Rule(40, m, f, evidence: dependency), "qualified column");
        var other = new Measure { Name = "Other", Expression = "'Sales'[Total]" };
        f.Table.Measures.Add(other);
        string measureDependency = dependency.Replace("\"Total\"", "\"Other\"", StringComparison.Ordinal)
            .Replace("\"Amount\"", "\"Total\"", StringComparison.Ordinal)
            .Replace("\"Referenced Object Type\":\"Column\"", "\"Referenced Object Type\":\"Measure\"", StringComparison.Ordinal);
        Equal(true, Rule(41, other, f, evidence: measureDependency), "qualified measure");
        other.Expression = "[Total]";
        Equal(false, Rule(41, other, f, evidence: measureDependency), "unqualified measure");
        Equal(false, Rule(45, m, f, evidence: measureDependency), "hidden measure referenced");
        Equal(true, Rule(45, m, f, evidence: """{"dependencies":{"status":"available","rows":[]}}"""), "hidden unreferenced");
        f.Amount.IsHidden = true;
        string columnDependency = dependency.Replace("\"Object Name\":\"Total\"", "\"Object Name\":\"Amount\"", StringComparison.Ordinal)
            .Replace("\"Object Type\":\"Measure\"", "\"Object Type\":\"Column\"", StringComparison.Ordinal)
            .Replace("\"Referenced Object\":\"Amount\"", "\"Referenced Object\":\"Key\"", StringComparison.Ordinal)
            .Replace("\"Referenced Full Object Name\":\"'Sales'[Amount]\"", "\"Referenced Full Object Name\":\"'Sales'[Key]\"", StringComparison.Ordinal);
        Equal(true, Rule(44, f.Amount, f, evidence: columnDependency), "upstream depends_on not referenced_by");
        Equal(false, Rule(44, f.Amount, f, evidence: """{"dependencies":{"status":"available","rows":[]}}"""), "no column dependency");
        const string stats = """{"extendedStatistics":{"status":"available","tables":[{"tableName":"Sales","rowCount":25000000}]}}""";
        Equal(false, Rule(22, f.Table, f, """{"extended":true}""", stats), "25M strict boundary");
        Equal(true, Rule(22, f.Table, f, """{"extended":true}""", stats.Replace("25000000", "25000001", StringComparison.Ordinal)), "25M plus one");
        Equal(true, Rule(22, f.Table, f, """{"extended":false}""", stats.Replace("25000000", "25000001", StringComparison.Ordinal)),
            "already supplied statistics remain usable without extended collection");
        var result = Analyze(f, """{"extended":true}""", """{"extendedStatistics":{"status":"available","tables":[]}}""");
        Equal("unavailable", Result(result, 22).GetProperty("status").GetString(), "known empty stats not zero");
        result = Analyze(f, evidence: """{"dependencies":{"status":"failed","rows":[]}}""");
        Equal("failed", Result(result, 40).GetProperty("status").GetString(), "dependency provider failed");
        Equal("failed", result.GetProperty("status").GetString(), "any failed rule fails report");
        result = Analyze(f, """{"dependencies":false}""", dependency);
        Equal("unavailable", Result(result, 40).GetProperty("status").GetString(), "disabled not no violations");
        f.Partition.Mode = ModeType.DirectLake;
        const string source = """
            {"directLake":{"fallbackReasons":{"status":"available","rows":[{"Table Name":"Sales","FallbackReasonID":2}]},
             "vOrder":{"status":"available","lakehouseSource":true,"rows":[{"tableName":"Sales","partitionName":"Sales","isVOrdered":false}]}}}
            """;
        Equal(true, Rule(13, f.Database.Model, f, evidence: source), "model uses view");
        Equal(true, Rule(14, f.Partition, f, evidence: source), "partition uses view");
        Equal(true, Rule(15, f.Partition, f, evidence: source), "not VOrdered");
        Equal(false, Rule(13, f.Database.Model, f, evidence: source.Replace("\"FallbackReasonID\":2", "\"FallbackReasonID\":1", StringComparison.Ordinal)), "fallback not view");
        Equal(false, Rule(14, f.Partition, f, evidence: source.Replace("\"FallbackReasonID\":2", "\"FallbackReasonID\":1", StringComparison.Ordinal)), "partition no view");
        Equal(false, Rule(15, f.Partition, f, evidence: source.Replace("\"isVOrdered\":false", "\"isVOrdered\":true", StringComparison.Ordinal)), "VOrdered");
        result = Analyze(f);
        Equal("unavailable", Result(result, 13).GetProperty("status").GetString(), "missing view inspection");
        Equal("unavailable", Result(result, 14).GetProperty("status").GetString(), "suppressed helper now explicit");
        Equal("unavailable", Result(result, 15).GetProperty("status").GetString(), "missing VOrder inspection");
        Equal(false, Rule(15, f.Partition, f, evidence: """{"directLake":{"vOrder":{"status":"available","lakehouseSource":false,"rows":[]}}}"""), "no lakehouse explicit");
    }

    private sealed class Fixture
    {
        internal Fixture()
        {
            Database = new Database { Name = "Offline", CompatibilityLevel = 1604, Model = new Model { Name = "Offline" } };
            Table = NewTable("Sales");
            Database.Model.Tables.Add(Table);
            Amount = new DataColumn { Name = "Amount", DataType = DataType.Int64, SourceColumn = "Amount", Description = "", DataCategory = "", FormatString = "" };
            Key = new DataColumn { Name = "Key", DataType = DataType.Int64, SourceColumn = "Key", Description = "", FormatString = "" };
            Table.Columns.Add(Amount);
            Table.Columns.Add(Key);
            Partition = new Partition { Name = "Sales", Mode = ModeType.Import, Source = new MPartitionSource { Expression = "let Source=1 in Source" } };
            Table.Partitions.Add(Partition);
        }
        internal Database Database { get; }
        internal Table Table { get; }
        internal DataColumn Amount { get; }
        internal DataColumn Key { get; }
        internal Partition Partition { get; }
        internal TablePermission? Permission { get; private set; }
        internal static Table NewTable(string name) => new() { Name = name, Description = "", DataCategory = "" };
        internal SingleColumnRelationship Relationship()
        {
            var dim = NewTable("Dim");
            Database.Model.Tables.Add(dim);
            var key = new DataColumn { Name = "Key", DataType = DataType.Int64, SourceColumn = "Key" };
            dim.Columns.Add(key);
            dim.Partitions.Add(new Partition { Name = "NotDim", Mode = ModeType.Import, Source = new MPartitionSource { Expression = "let x=1 in x" } });
            var r = new SingleColumnRelationship
            {
                Name = "Sales_Dim", FromColumn = Key, ToColumn = key,
                FromCardinality = RelationshipEndCardinality.Many, ToCardinality = RelationshipEndCardinality.One,
                CrossFilteringBehavior = CrossFilteringBehavior.OneDirection
            };
            Database.Model.Relationships.Add(r);
            return r;
        }
        internal void Rls(string expression)
        {
            var role = new ModelRole { Name = "Readers" };
            Database.Model.Roles.Add(role);
            Permission = new TablePermission { Table = Table, FilterExpression = expression };
            role.TablePermissions.Add(Permission);
        }
    }
}

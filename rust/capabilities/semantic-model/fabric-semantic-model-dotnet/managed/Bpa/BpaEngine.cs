// Copyright (c) Microsoft Corporation. Licensed under the MIT License.
// Port of microsoft/semantic-link-labs@2ddf46109c1209ceffa88149f4631e7b2275f9a1.
// See Upstream/LICENSE.txt and README.md for provenance and intentional deviations.
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Serialization;
using Microsoft.AnalysisServices.Tabular;
using JsonSerializer = System.Text.Json.JsonSerializer;

namespace Fabric.SemanticModel.Bpa;

/// <summary>Evaluates the pinned Semantic Link Labs BPA rules without network access or model mutation.</summary>
public static class BpaEngine
{
    /// <summary>The only upstream source version supported by this evaluator.</summary>
    public const string SourceCommit = "2ddf46109c1209ceffa88149f4631e7b2275f9a1";

    internal static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = true,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull
    };

    /// <summary>
    /// Analyzes caller-owned TOM metadata and explicit offline evidence. Missing evidence is reported,
    /// never replaced with an empty dependency graph, zero statistics, or an assumed source state.
    /// </summary>
    /// <param name="database">An offline, already loaded TOM database. It is not modified.</param>
    /// <param name="options">See README.md for the boolean extended/dependencies options.</param>
    /// <param name="evidence">Caller-supplied dependency, statistics and source-inspection results.</param>
    /// <returns>A versioned JSON result with complete, unfiltered findings and per-rule coverage.</returns>
    public static string Analyze(Database database, JsonElement options, JsonElement evidence)
    {
        try
        {
            ArgumentNullException.ThrowIfNull(database);
            if (database.Model is null)
                return Failure("missing_model");

            var context = new EvaluationContext(database.Model, options, evidence);
            var catalog = LoadCatalog();
            var findings = new List<Finding>();
            var results = new List<RuleResult>();
            bool empty = database.Model.Tables.Count == 0;
            foreach (var rule in catalog)
            {
                var result = new RuleResult(rule);
                results.Add(result);
                if (empty)
                {
                    result.Status = "unavailable";
                    result.Issues.Add(new Issue("Model", "Model", "unavailable", "no_tables", new[] { "model.tables" }));
                    continue;
                }

                foreach (string scope in rule.Scopes)
                {
                    // Materialization and naming are inside the boundary as malformed TOM can fail here too.
                    try
                    {
                        foreach (var obj in context.Objects(scope).ToArray())
                        {
                            string name = "unresolved";
                            try
                            {
                                name = context.ObjectName(scope, obj);
                                bool violates = Predicates.Evaluate(rule.PredicateId, obj, context);
                                result.EvaluatedObjects++;
                                if (violates)
                                    findings.Add(new Finding(rule.Category, rule.RuleName, rule.Severity,
                                        scope, name, rule.Description, rule.Url));
                            }
                            catch (EvidenceException ex)
                            {
                                if (ex.Failed) result.FailedObjects++;
                                else result.UnavailableObjects++;
                                result.Issues.Add(new Issue(scope, name, ex.Failed ? "failed" : "unavailable",
                                    ex.Code, new[] { ex.Required }));
                            }
                            catch (Exception ex) when (ex is not OutOfMemoryException)
                            {
                                result.FailedObjects++;
                                result.Issues.Add(new Issue(scope, name, "failed",
                                    "predicate_error", Array.Empty<string>(), ex.GetType().Name));
                            }
                        }
                    }
                    catch (Exception ex) when (ex is not OutOfMemoryException)
                    {
                        result.FailedObjects++;
                        result.Issues.Add(new Issue(scope, "unresolved", "failed",
                            "scope_error", Array.Empty<string>(), ex.GetType().Name));
                    }
                }
                result.Status = result.FailedObjects > 0 ? "failed"
                    : result.UnavailableObjects > 0 ? "unavailable" : "evaluated";
            }

            int evaluated = results.Count(r => r.Status == "evaluated");
            int unavailable = results.Count(r => r.Status == "unavailable");
            int failed = results.Count(r => r.Status == "failed");
            string status = failed > 0 ? "failed"
                : unavailable > 0 ? "partial" : "succeeded";
            return JsonSerializer.Serialize(new
            {
                schemaVersion = "1",
                source = Source(),
                status,
                outcome = empty ? "not_applicable_no_tables" : "analyzed",
                findings,
                rules = results,
                coverage = new
                {
                    total = catalog.Length, evaluated, unavailable, failed,
                    applicable = !empty,
                    evaluatedObjects = results.Sum(r => r.EvaluatedObjects),
                    unavailableObjects = results.Sum(r => r.UnavailableObjects),
                    failedObjects = results.Sum(r => r.FailedObjects),
                    implementation = "64_pinned_predicates",
                    parity = "ported_with_documented_fail_explicit_deviations"
                }
            }, JsonOptions);
        }
        catch (EvidenceException ex)
        {
            return Failure(ex.Code);
        }
        catch (Exception ex) when (ex is not OutOfMemoryException)
        {
            return Failure("analysis_error", ex.GetType().Name);
        }
    }

    internal static RuleDefinition[] LoadCatalog()
    {
        var rules = JsonSerializer.Deserialize<RuleDefinition[]>(PinnedCatalog.Json, JsonOptions)
            ?? throw new InvalidOperationException("Invalid pinned rule catalog.");
        if (rules.Length != 64 || rules.Select(r => r.Id).Distinct(StringComparer.Ordinal).Count() != 64
            || rules.Select(r => r.PredicateId).Distinct(StringComparer.Ordinal).Count() != 64)
            throw new InvalidOperationException("Incomplete pinned rule catalog.");
        return rules;
    }

    private static object Source() => new
    {
        repository = "https://github.com/microsoft/semantic-link-labs",
        commit = SourceCommit,
        rulesPath = "src/sempy_labs/_model_bpa_rules.py",
        evaluatorPath = "src/sempy_labs/_model_bpa.py",
        license = "MIT",
        resultColumns = new[] { "Category", "Rule Name", "Severity", "Object Type", "Object Name", "Description", "URL" }
    };

    private static string Failure(string code, string? errorType = null)
    {
        RuleResult[] rules;
        try
        {
            rules = LoadCatalog().Select(definition =>
            {
                var result = new RuleResult(definition) { Status = "failed" };
                result.Issues.Add(new Issue("Model", "Model", "failed", code, Array.Empty<string>(), errorType));
                return result;
            }).ToArray();
        }
        catch (Exception ex) when (ex is not OutOfMemoryException)
        {
            rules = Array.Empty<RuleResult>();
        }
        return JsonSerializer.Serialize(new
        {
            schemaVersion = "1", source = Source(), status = "failed",
            findings = Array.Empty<Finding>(), rules,
            coverage = new { total = rules.Length, expectedTotal = 64, evaluated = 0, unavailable = 0,
                failed = rules.Length, applicable = false, inventoryAvailable = rules.Length == 64 },
            error = new { code, errorType }
        }, JsonOptions);
    }
}

internal sealed class RuleDefinition
{
    public RuleDefinition() { }
    public string Id { get; set; } = "";
    public string PredicateId { get; set; } = "";
    public string Category { get; set; } = "";
    public string Severity { get; set; } = "";
    public string RuleName { get; set; } = "";
    public string[] Scopes { get; set; } = Array.Empty<string>();
    public string Predicate { get; set; } = "";
    public string? Description { get; set; }
    public string? Url { get; set; }
    public int SourceLine { get; set; }
    public string[] RequiredData { get; set; } = Array.Empty<string>();
}

internal sealed class RuleResult
{
    internal RuleResult(RuleDefinition definition) => Definition = definition;
    [JsonIgnore] internal RuleDefinition Definition { get; }
    public string Id => Definition.Id;
    public string RuleName => Definition.RuleName;
    public string Category => Definition.Category;
    public string Severity => Definition.Severity;
    public string[] Scopes => Definition.Scopes;
    public string Predicate => Definition.Predicate;
    public string[] RequiredData => Definition.RequiredData;
    public object Provenance => new
    {
        repository = "microsoft/semantic-link-labs", commit = BpaEngine.SourceCommit,
        path = "src/sempy_labs/_model_bpa_rules.py", line = Definition.SourceLine,
        sourceKey = "src/sempy_labs/_model_bpa_rules.py#" + Definition.RuleName
    };
    public string Status { get; set; } = "evaluated";
    public int EvaluatedObjects { get; set; }
    public int UnavailableObjects { get; set; }
    public int FailedObjects { get; set; }
    public List<Issue> Issues { get; } = new();
}

internal sealed record Finding(string Category, string RuleName, string Severity, string ObjectType,
    string ObjectName,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.Never)] string? Description,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.Never)] string? Url);
internal sealed record Issue(string ObjectType, string ObjectName, string Status, string Code,
    string[] MissingEvidence, string? ErrorType = null);

internal sealed class EvidenceException : Exception
{
    internal EvidenceException(string code, string required, bool failed = false) : base(code)
    {
        Code = code; Required = required; Failed = failed;
    }
    internal string Code { get; }
    internal string Required { get; }
    internal bool Failed { get; }
}

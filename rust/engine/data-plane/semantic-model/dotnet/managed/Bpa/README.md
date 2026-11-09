<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Offline Semantic Link Labs BPA port

This directory implements all **64 predicates (87 rule/scope pairs)** in
`microsoft/semantic-link-labs@2ddf46109c1209ceffa88149f4631e7b2275f9a1`.
It does not substitute the smaller Fabio rule set. Implementation coverage is
not proof of full behavioral parity: synthetic validation, independent acceptance,
host integration, and the particular evidence supplied must also be considered.

## Provenance and completeness ledger

`Upstream/rules.json` records every rule's stable source key, source-order ID
(`predicateId`: `labs-001` through `labs-064`), name, category, severity, all scopes, verbatim
Python predicate, source line, required data and implementation location.
Public `id` is `labs:` followed by the SHA-256 hex digest of UTF-8 bytes for
`ruleName + NUL + join(NUL, scopes sorted ordinally)`. IDs do not depend on
source ordering; source-order `predicateId` is only an internal port/ledger alias.
`PinnedCatalog.g.cs` contains the same inventory as static data, without requiring
host resource configuration. `sourceKey` includes the upstream path and exact
rule name. The numeric aliases in this document refer to `predicateId`.

The inventory contains 24 Performance, 5 Error Prevention, 14 DAX Expressions,
5 Maintenance, 14 Formatting and 2 Naming Conventions rules; 47 Warning,
10 Info and 7 Error severities. No pinned predicate is intentionally unimplemented.
The data-dependent subset is:

| Rules | Explicit external input |
|---|---|
| `labs-013`, `labs-014` | Complete Direct Lake fallback-reason result |
| `labs-015` | First lakehouse source selection and V-Order inspection of each Direct Lake partition |
| `labs-022` | Extended table row counts |
| `labs-040`, `labs-041`, `labs-044`, `labs-045` | Complete, already-expanded upstream calculation dependency rows |

`Upstream/_model_bpa_rules.py.txt` and `_model_bpa.py.txt` are the pinned source,
not executable runtime dependencies. The TOM helper, dependency and formatting
source files capture the helper semantics used by the port. `source-hashes.json`
records SHA-256 digests of retained source artifacts. `LICENSE.txt` reproduces
the actual upstream MIT license. Ported code retains Microsoft's copyright.
The capture scripts are development tools only; analysis does not invoke Python,
pandas, a notebook, dynamic compilation, or expression evaluation.

## API and options

```csharp
string json = Fabric.SemanticModel.Bpa.BpaEngine.Analyze(database, options, evidence);
```

`database` is already-loaded TOM metadata. The engine does not connect, execute
queries, inspect a lakehouse, refresh annotations, save, or change the model.
The caller owns model lifetime and must not mutate it concurrently with analysis.
The managed host selects the TOM NuGet package and bounds request/response sizes.

Options are an object:

```json
{"extended": false, "dependencies": true,
 "sourceCommit": "2ddf46109c1209ceffa88149f4631e7b2275f9a1"}
```

`extended` and `dependencies` must be JSON booleans when present. Their defaults
match upstream `extended=False` / `check_dependencies=True`. Turning dependency
checking off makes rules that actually need it unavailable, not evaluated without
violations. `extended` records a request for extended collection at the caller
boundary; this offline engine never collects or writes statistics. Explicitly
supplied statistics remain usable with `extended: false`, just as upstream
preexisting annotations remain usable without calling `set_vertipaq_annotations`.
`sourceCommit`, if supplied on options or evidence, must equal the
pin. Severity is intentionally not an evaluation option; the result is complete
and unfiltered for caller-side policy and presentation.

## Evidence contract

Each provider has an explicit `status`: `available`, `unavailable`, or `failed`.
An omitted provider is unavailable. An available provider must include its array,
even when empty. Status alone or an omitted field is never interpreted as an
empty result. Failed provider status makes dependent rules failed.
Names and dependency type labels are case-sensitive, as in the pinned helpers.

```json
{
  "dependencies": {
    "status": "available",
    "rows": [{
      "Table Name": "Sales",
      "Object Name": "Total",
      "Object Type": "Measure",
      "Expression": "SUM([Amount])",
      "Referenced Table": "Sales",
      "Referenced Object": "Amount",
      "Referenced Object Type": "Column",
      "Full Object Name": "'Sales'[Total]",
      "Referenced Full Object Name": "'Sales'[Amount]",
      "Parent Node": "Total"
    }]
  },
  "extendedStatistics": {
    "status": "available",
    "tables": [{"tableName": "Sales", "rowCount": 25000001}]
  },
  "directLake": {
    "fallbackReasons": {
      "status": "available",
      "rows": [{"Table Name": "Sales", "FallbackReasonID": 2}]
    },
    "vOrder": {
      "status": "available",
      "lakehouseSource": true,
      "rows": [{
        "tableName": "Sales", "partitionName": "Sales", "isVOrdered": false
      }]
    }
  }
}
```

Dependency rows are the **output** of pinned `get_model_calc_dependencies`,
including its transitive rows and `Parent Node` semantics, not raw DMV output.
The ten illustrated columns are required strings in nonempty rows. Types include
`Measure`, `Column`, `Calc Column`, `Table`, `Calc Table`, `Calculation Item`;
the engine does not guess aliases or compute a different dependency graph.
Qualification rules first retain rows where `Object Name == Parent Node`.
The helper resolves referenced objects against TOM, in measure/column/table
order; references outside those TOM categories yield no resolved object, exactly
as upstream. `rows: []` explicitly attests to a complete known-empty graph.

Statistics require a nonnegative integral `rowCount` for each candidate table.
A table absent from `tables: []` is unavailable, not zero. Row counts are supplied
explicitly even when a BIM contains old VertiPaq annotations; the engine neither
trusts their freshness implicitly nor replaces them. The strict large-table
threshold is **greater than 25,000,000**, with one partition, and no Direct Lake
partition anywhere in the model.

Fallback rows use the upstream numeric `FallbackReasonID` and `Table Name`.
`rows: []` means a complete inspection found no fallback rows; reason **2**
alone indicates a view. No mode or source state is inferred from absence.

For V-Order, `lakehouseSource: false` explicitly states that pinned
`get_direct_lake_sources()` has no source with `itemType == "Lakehouse"`.
With `true`, rows describe inspections against the **first** such source,
using each Direct Lake partition's `EntityName` (or TOM table name) and
`SchemaName` (or no schema). Every Direct Lake partition needs a matching
`tableName`/`partitionName` row; an empty array is not a successful inspection.
Row `status` may explicitly be `available`, `unavailable`, or `failed`.
An available row needs a boolean `isVOrdered`. As upstream, one non-V-Ordered
partition marks its TOM table, so all Direct Lake partitions of that table
violate the rule. The engine never executes a source query or maintenance action.

The caller attests provider completeness by setting `available`. The engine
cannot prove the origin, permissions, freshness or correctness of caller evidence.
Do not pass live credentials, tokens, connection strings or settings in evidence.

## Results and deliberate failure-explicit differences

Results retain all seven Labs columns as camel-case finding fields:
`category`, `ruleName`, `severity`, `objectType`, `objectName`, `description`,
`url`. Severity uses stable source names, not notebook icons. Unpopulated source
description/URL cells remain JSON null. Object naming matches upstream:
measures use their name, model uses `Model`, columns/hierarchies/partitions/items
use `'Table'[Object]`, RLS uses `'Role'[Permission]`, and relationships use
`'From'[Column] -> 'To'[Column]`. Source formatting does not escape embedded
apostrophes; neither does this port.

The report is `failed` if any rule failed, otherwise `partial` if any rule is
unavailable, otherwise `succeeded`. Every rule includes provenance, inputs, predicate, status, evaluated/unavailable/
failed object counts, and issues. Short-circuit predicates only require evidence
if the corresponding branch is reached. Findings from successfully evaluated
objects remain present if another object makes that rule unavailable or failed.
Zero eligible objects is an evaluated empty scope on a nonempty model, distinct
from an entirely empty model. A no-tables model returns `partial`,
`outcome: not_applicable_no_tables`, 64 unavailable rules and zero evaluated rules.
Upstream instead warns and returns Python `None` before dependency acquisition
or default-rule construction; the explicit native envelope is the owner-required
representation of that no-result outcome, not a purported upstream DataFrame.
Invalid options/source version/missing model return `failed` with explicit error.
Corrupt compiled inventory is an analysis failure, never a clean scan.

The following changes are intentional owner-required failure-explicit deviations:

* Upstream `_memoize_on_tom` catches all factory exceptions and caches an empty
  set; source-inspection and cache failures here remain unavailable/failed.
* Upstream V-Order inspection continues after each inaccessible source table.
  This engine requires evidence for all selected partitions instead.
* Upstream `_uses_sum_or_average_on_string_column` suppresses expression
  acquisition errors. Missing expressions here are explicit issues.
* Upstream `row_count` substitutes zero for a missing annotation.
  Missing explicit statistics here are unavailable.
* Upstream `check_dependencies=False` supplies an empty DataFrame.
  Disabled dependency evaluation here is unavailable when needed.
* Invalid metadata/predicate failures are isolated to the rule/object and
  reported instead of aborting the whole notebook scan.

Other source oddities are deliberately preserved: dynamic-RLS regex parentheses
are unescaped; relationship percentages double-count relationships that are both
bidirectional and many-to-many; rule 44 uses `depends_on`, not `referenced_by`;
rule 57 compares the length of `str(SortByColumn)`, so `None` is `"None"` and does
not match; whole-number formatting also accepts `#,0.0`; expression matching is
regex/string-based, not a DAX parser. The port does not silently repair these.

Python regex compatibility includes Unicode simple case-fold exceptions,
case-sensitive captured string-column names, Python whitespace controls U+001C
through U+001F, and expanding first-character uppercase mappings. Static Unicode
15.1 data is captured during development; the upstream pin itself does not pin
its Python interpreter's Unicode database. Regex execution is bounded to two
seconds per operation; timeout is an explicit failed object, not no violation.

## Validation

`Tests/Fabric.SemanticModel.Bpa.Tests.csproj` is an offline console assertion
suite with no test-framework dependency. It covers the inventory, all 64
predicate IDs, positive/negative metadata and expression cases, strict
thresholds, missing/empty/failed/disabled evidence, unsupported source version,
Direct Lake sources, empty model, naming and read-only behavior.
Rule 57 has negative cases only because the pinned `str(None)` expression
does not yield the intended missing-sort violation.

From the managed directory, with the coordinator's shared build-slot grant:

```powershell
dotnet run --project Bpa\Tests\Fabric.SemanticModel.Bpa.Tests.csproj
```

The test project uses only `rust/NuGet.Config`, disables interactive NuGet, and
defaults to locked restore. The coordinator must bootstrap the actual
`Tests/dependencies.lock.json` under the sanitized restore environment with
`-p:RestoreLockedMode=false` before running the locked test command. No lock is
fabricated from dependency declarations, and no restore is authorized without
the shared build slot.

Authored tests are not execution evidence. Independent upstream acceptance and
whole-host validation remain separate required gates.

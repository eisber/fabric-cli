<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Independent Semantic Link Labs BPA source oracle

This is a **developer-only, offline acceptance oracle**, not product code or a
Python dependency of the optional .NET/TOM port. Its author did not inspect the
managed BPA port, Rust bridge, or CLI implementation. Existing independent
oracles are unchanged.

## Run

From the repository root, using Python 3.10 or later and only its standard library:

```powershell
$env:PYTHONDONTWRITEBYTECODE = '1'
python rust\tests\compatibility\semantic_model_bpa\oracle.py
python -m unittest discover -s rust\tests\compatibility\semantic_model_bpa -p test_oracle.py -v
```

From `rust`, omit the leading `rust\` in both paths. No network, native build,
TOM package, sempy, pandas, Fabric, XMLA, credential, or settings access is needed.
The same validation works under `python -O`; comparisons do not depend on
Python's removable `assert` statements.

Capture/validation was executed with **CPython 3.13.7, Windows x64**. Other Python
versions and platforms were not executed here. AST fingerprints normalize the
empty `type_params` field introduced in Python 3.12; source line positions and
original source bytes are never normalized.

`--capture` is a first-capture developer tool. It **refuses to overwrite any
existing expectation or manifest**. Normal validation never changes files.
Do not delete baselines or regenerate outputs to make a product implementation
pass. Additional cases require independent, source-based acceptance ownership;
source pin changes require an explicit baseline decision.

## Immutable source

Repository: `https://github.com/microsoft/semantic-link-labs`

Commit: `2ddf46109c1209ceffa88149f4631e7b2275f9a1`

| Upstream path | Exact-byte SHA-256 |
| --- | --- |
| `src/sempy_labs/_model_bpa_rules.py` | `bd28bf30d9815116defdb5664158f70a0a957dd6904a6ccdb33c4d007c4b648f` |
| `src/sempy_labs/_model_bpa.py` | `eb1c9851476e283e244e39437c80e009bbed68fede71fa3f8f2fd2b0519d0a0f` |
| `src/sempy_labs/_helper_functions.py` | `86720495eff69165598a92e018462608f55ab1219f8663f8f848688dc4b0d572` |
| `LICENSE` | `c2cfccb812fe482101a8f04597dfc5a9991a6b2748266c47ac91b6a5aae15383` |

These are complete, unmodified public source snapshots, with the original MIT
license. The runner checks hardcoded hashes **before parsing or execution**.
It does not fetch replacements. `capture_manifest.json` additionally hashes
all input BIM files, captured cases, coverage, inventory, and source snapshots.

## Inventory

`rust\tests\fixtures\semantic_model_bpa\inventory.json` is derived from the
original factory's literal tuple table. It contains **64 rules**, not a substitute
heuristic set:

| Category | Count |
| --- | ---: |
| Performance | 24 |
| Error Prevention | 5 |
| DAX Expressions | 14 |
| Maintenance | 5 |
| Formatting | 14 |
| Naming Conventions | 2 |

Severity counts: **47 Warning, 10 Info, 7 Error**.

Every entry preserves ordinal, exact category/name/severity/description/URL,
original scalar/list scope and normalized scope list, exact lambda text,
line/column ranges, structural AST hash, and commit-qualified source link.
Ordinals are this ledger's source-order identifiers, **not upstream rule IDs**.
Missing description/URL tuple members are JSON `null`, not invented descriptions
or URLs. Pandas normally pads absent tuple members with missing values.

The ledger also lists each expression's TOM enum references, object attribute
paths, TOM-wrapper methods and global helper/dependency references. It records
local helper import dependencies and AST provenance, evaluator defaults,
no-tables branch, selected dispatch statements, and original output-name helpers.

## What executes

**145 frozen predicate cases exercise 62 original upstream lambdas**, including
one positive and one negative case for each of those 62. Four cases capture
errors: one missing synthetic evidence response, one null-expression `TypeError`,
and two empty-name `IndexError` cases. The other 141 cases capture truthiness.
Regex predicates can return match objects rather than `bool`; the oracle uses
the same truthiness decision as the original evaluator.

The runner parses verified source with `ast`, compiles the original lambda nodes,
and executes them with:

- Explicit JSON attribute objects and `Count`-bearing synthetic collections.
- Synthetic TOM enum members, represented as distinct strings.
- Original `_memoize_on_tom`, `_string_column_names`, and
  `_uses_sum_or_average_on_string_column` helper definitions for rule 42.
- A restricted synthetic import response for the helper's TOM namespace.
  No actual Microsoft/TOM import occurs.
- Explicit collaborator responses, not handwritten helper implementations.
  Missing responses raise `MissingEvidence`, never an assumed zero/empty result.
  Observed method calls and keyword names are frozen alongside results.

`capture_inputs.py` constructs data only. It contains **no ported predicates**.
The input builder is checked against frozen JSON to catch silent input drift.

### Evaluator/result shapes

Seven evaluator cases use the original no-tables branch, scope map, dispatch
loop, concat/metadata merge, rename, and final result-column projection. These
are intact AST statements selected from `_model_bpa.py`; the inventory records
their exact provenance.

The two original naming helpers execute from `_helper_functions.py`; their
logging decorator is an inert identity collaborator. A deliberately small
dataframe double supplies only the selected source's operations. It does not
reimplement predicates and is **not proof of full pandas compatibility**.

| Fixture | Original expressions selected | Captured shape |
| --- | --- | --- |
| Empty BIM | Full default catalog is supplied, but no predicate runs | Warning, `None`; not successful empty findings |
| Decimal column BIM | Rule 1 | Empty dataframe, seven original result columns |
| Double column BIM | Rule 1 | One numeric metadata finding |
| Null source-column BIM | Rule 27 | Original `TypeError`, not an empty dataframe |
| Malformed BIM | None; JSON parsing fails first | Harness `JSONDecodeError`, explicitly not a TOM error |
| Relationships BIM | Rules 3, 6, 19, 26, 58 | Five relationship/model findings |
| Naming BIM | Rules 61, 62, 63 | Five table/column findings from multi-scope dispatch |

Result columns, in source order:

```text
Category, Rule Name, Severity, Object Type, Object Name, Description, URL
```

The captured JSON is a **normalized developer projection**: severity retains
the rule's textual level, absent metadata becomes `null`, and the empty-model
warning icon is `<warning>`. Actual `run_model_bpa` replaces severities with
notebook icons. Its default `return_dataframe=False` takes the later HTML
rendering path; that path is recorded as **unexecuted**, not falsely presented as
a returned dataframe. The dataframe cases validate only the selected core.

BIM adaptation is limited to explicit fixture metadata and object/collection
links. It is not a TOM loader and does not establish that these deliberately
invalid-data fixtures deserialize through a real TOM package.

## Dependencies and noncoverage

`coverage.json` enumerates **all 64 rules**, their case IDs, every unverified
wrapper method and external helper, and runtime gaps. An executed expression
with synthetic helper responses is **not full-rule semantic acceptance**.

| Dependency family | What this oracle establishes | Still unavailable |
| --- | --- | --- |
| Metadata, regex, enum equality | Original expression behavior on explicit values | Actual TOM enum/null/property/collection representations |
| Relationship traversal, RLS, partitions, field parameters | Original expression consumes supplied collaborator responses | TOM-wrapper traversal/detection implementations |
| Rules 40, 41, 44, 45 | Original predicate behavior on supplied dependency responses | Dependency acquisition, dataframe interpretation, DAX dependency graph construction |
| Rule 22 | Strict original `> 25000000` threshold | Real row counts, DMV/VertiPaq acquisition |
| Rule 13 | Original call consumes an explicit boolean | Actual Direct Lake/view detection |
| Rule 42 | Original regex and local string-column helper path | Real TOM enums, DAX parsing/type resolution, cache/query-failure paths |
| **Rule 14** | **Not executed** | `_direct_lake_view_tables`, Direct Lake detection, model identity, fallback-reason provider/DMV/Fabric evidence |
| **Rule 15** | **Not executed** | `_non_vordered_direct_lake_tables`, Direct Lake source/partition identity, actual TOM mode values, lakehouse `is_v_ordered` evidence |

Explicitly unexecuted: complete upstream invocation/import/startup, actual pandas,
default full-catalog scanning against a real model, real TOM BIM/TMDL loading,
dependency/helper implementations, authentication, Fabric/XMLA, extended
VertiPaq, remote/lakehouse data, Spark, translations, export, HTML/icons, optional
.NET runtime, Rust bridge/CLI, cancellation, and native lifetime/threading.
Passing these checks does not certify the port, optional-feature integration,
native safety, live-model acceptance, or all upstream rules.

### Owner-required native contract, separate from the source oracle

The delivery scope is offline BIM/TMDL analysis and an optional in-process .NET
host. Live XMLA and data providers are absent. The native output must use
`schemaVersion: "1"` (a string), the exact pin in `source.commit`, findings with
`category`, `ruleName`, `severity`, `objectType`, `objectName`, `description` and
`url`, per-rule statuses `evaluated`, `unavailable` or `failed`, and coverage
counts `total`, `evaluated`, `unavailable` and `failed`.

All 64 inventory entries remain required even where external evidence is
unavailable. Missing evidence is not an available empty result. In particular,
missing dependency evidence cannot be treated like an explicitly available
empty dependency table. The stdlib invariants additionally demonstrate that the
original predicates for rules 40/41 distinguish a missing dependency binding
from an explicitly supplied empty helper result. They do **not** implement
dependency-table interpretation or validate the product's evidence protocol.
Native schema/loader/provider-state enforcement needs separate product tests;
these source fixtures must not be relabeled as native execution evidence.

## Source observations preserved, not repaired

Each observation below is tied to the immutable source expression and a frozen
synthetic reproduction. These are potential follow-up discussion topics for the
coordinator, not permission to redesign the port or oracle:

- **Rule 19:** exactly 30% is clean; a relationship that is both bidirectional
  and many-to-many counts twice in the numerator. The double-counting case has
  one such relationship among five and violates because `2 / 5 > 0.3`.
- **Rule 4:** the regex patterns contain unescaped parentheses, so `USERNAME`
  without literal `()` still matches.
- **Rule 11:** Power Query substring detection is case-sensitive.
- **Rules 16/34:** literal-space stripping versus whitespace stripping and
  case-sensitive expression comparison are different operations.
- **Rule 42:** function names are case-insensitive but column-name membership is
  case-sensitive and checks unqualified names across the whole model.
- **Rule 43:** reversing USERELATIONSHIP argument order does not match the
  original activation regex.
- **Rule 44:** the original unnecessary-column predicate uses
  `any(tom.depends_on(...))`, not a guessed inverse.
- **Rule 57:** `str(None)` is not empty, so a null sort-by value does not behave
  like the synthetic empty-string sort-by case.
- **Rules 61/62:** empty names raise `IndexError`; rule 25 raises `TypeError` for
  a null expression. The original dispatch has no per-rule exception handler.

The original memoizer also catches helper factory exceptions and caches
`set()`, and the string-expression helper catches expression retrieval errors.
Those source fallbacks must not be mistaken for available evidence. The
owner's port acceptance explicitly requires unsupported/unavailable/error
coverage for missing evidence rather than false-clean results. The oracle does
not invent successful outcomes for the two unexecuted external-provider rules.

## Files and validation boundary

All changes are confined to:

- `rust\tests\compatibility\semantic_model_bpa\`: this README, `oracle.py`,
  `capture_inputs.py`, `test_oracle.py`.
- `rust\tests\fixtures\semantic_model_bpa\`: fixture README, five frozen JSON
  files, seven BIM inputs and four upstream source/license files.

No product manifests, dependencies, implementation files, existing oracles,
branches, commits, pushes, credentials, or settings were changed.
No native build slot was requested or consumed. Native/product acceptance
remains the coordinator's separately validated work.

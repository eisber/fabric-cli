<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Embedded Fabric documentation search

## Read and explore documentation locally

Search results use local retrieval references instead of web links:

```text
fab-rs search docs --text "semantic link" --format text
fab-rs get 'docs/data-science/semantic-link-overview.md'
fab-rs get 'docs/data-science/semantic-link-overview.md#section-1' --format json
fab-rs get docs/
fab-rs item lakehouse docs
fab-rs item lakehouse docs --text tables --format text
fab-rs item lakehouse docs --suggest lakeh
```

`get docs/...` reads the complete normalized stored article or one exact
snapshot-local section, not the search snippet or original Markdown bytes.
It uses no credentials, network, extraction, configuration, or writable cache.
The default retrieval bound is 2 MiB, configurable with `--max-bytes` up to
8 MiB. Missing articles, invalid addresses, corrupt stored records, and exceeded
limits are distinct nonzero failures. `get docs/` shows the local guide.
Both `get docs/<path>` and `get docs <path>` are accepted. Ordinary service GETs
remain separate, and a docs-looking option value is not a namespace selector.

Human search output uses ranked cards, two-decimal score display, heading
context, bounded-width prose, safe highlights, and copyable local references.
Machine-readable scores retain precision. Documentation version, schema and
snapshot identity are available in `search docs -h`; normal results omit the
snapshot banner. Explicit `--corpus-info` retains full source and license
provenance, including legally relevant original URLs.

Grouping defaults to one result per source article and also suppresses identical
stored article URLs across aliases; it never merges different articles just
because titles are similar. `--sections` explicitly requests section results.
A shared URL from different sections is not another article. The exact older
binary/flags used for a reported duplicate are not inferred from source inspection.

### Item documentation areas are not item-type guesses

The Lakehouse area has 13 explicitly reviewed source articles: eleven linked
by the `Store and manage` / `Lakehouse` node in the pinned Data Engineering TOC,
plus two individually reviewed links from the Lakehouse overview. The registry
in `data/areas.json` records its source revision, exact paths, evidence lines,
and deliberately partial, non-exclusive scope.

Area search and completion intersect this exact path union with user criteria
**before retrieval**. They do not select the whole Data Engineering workload,
recursively crawl related links, or infer membership from mentions. Notebook
navigation remains a sibling area, not automatically Lakehouse documentation.
This area registry is separate from the ten exact article `item_type`
classifications. A different corpus pin or missing metadata produces an explicit
unsupported result, not a broader fallback.

To extend item coverage, review a unique source TOC subtree and explicit related
links, validate each exact source path, record evidence, and add boundary tests.
Future distillation can assist that authoring-time review, but its output must
be a versioned reviewed registry rather than runtime model inference.

### NOINDEX coverage

Exactly two documents in the pinned source have `ROBOTS: NOINDEX`:
`docs/enterprise/cross-tenant-access.md` and
`docs/enterprise/cross-tenant-access-for-providers.md`. Both would otherwise be
standalone article inputs; both are disclosed in `manifest.skipped_inputs` and
excluded uniformly from the index. Their surrounding text describes limited
provider/preview availability, but the source does not state the editorial reason
for the directive.

NOINDEX is a discoverability instruction, **not an authorization or secrecy
boundary**. Exclusion currently honors the explicit source intent; this change
does not silently include those articles or claim that NOINDEX restricts access.

The optional `fabric-cli/docs-search` feature adds the explicit offline source
`fab-rs search docs`. It doesn't change Python `fab`, default Rust builds, or the
Catalog, relevance, and hybrid metadata-search contracts. The same local source
is available in `fab-cli-v2` when both `headless` and `docs-search` are selected.
Docs execute before configuration, credentials, update checks, and remote hosts.

The `Rust embedded docs` workflow checks the exact PR head on a hosted Linux
runner. It tests the SDK producer, builds the full pinned public corpus, runs
the embedded CLI contracts alongside existing runtime/create routes, and matches
the executable's corpus probe to the generated manifest. Its retained provenance
is feature-specific evidence, not a replacement for other required native checks.

```text
fab-rs search docs --text "OneLake shortcuts"
fab-rs search docs --text '"workspace identity"' --format text
fab-rs search docs --text '(identity OR "service principal") AND firewall'
fab-rs search docs --text onlake --fuzzy 1
fab-rs search docs --text "lakehouse tabl" --prefix --max-expansions 64
fab-rs search docs --suggest "firewal"
fab-rs search docs --filters
fab-rs search docs --corpus-info
```

Quote query text using your shell's rules. JSON is the default output. Success,
including no matches, goes to stdout; invalid arguments exit 2, and unavailable
or damaged indexes exit 1 with diagnostics on stderr. No failed search becomes
a successful empty result or switches to a remote source.

`--text`, `--suggest`, `--filters`, and `--corpus-info` are mutually exclusive.
Duplicate scalar options, malformed filters, conflicting modes, and numeric bounds
are rejected before opening the index or configuring a remote host. Only
`--filter`, `--facet`, and their documented multi-value behavior are repeatable.
`--format json|text` works with every mode; explicit conflicting global
`--output_format` and local `--format` selections are rejected.

## Discover exact metadata before filtering

Run `--filters` against the executable you will query. It reports an exact
field-to-value dictionary, with counts of **unique indexed source articles**
for each value. It is not a dictionary of proposed friendly aliases or values
guessed from body text. The metadata index profile is required; a baseline
index reports an unsupported-feature error rather than an empty vocabulary.

Source-authored metadata and inherited DocFX global/file metadata retain their
raw public keys, such as `ms.topic`, `ms.service`, and `ms.date`, when present.
The inventory can also include source-authored title, description, alias, or
other bounded public scalar/array fields. Do not silently substitute `topic`,
`service`, `date`, or a guessed item-type field for the actual observed key.
`workload` is the first source-directory component; `source_path` is the exact
repository-relative article path. These two fields are structural facts, not
inferences from product names in prose.
More than 256 retained metadata values in one article fails the index build
rather than silently truncating the filter vocabulary.

For a configured DocFX file-metadata key, ingestion conservatively retains all
distinct values from all matching file rules. It uses the global value only
when no file rule for that key matches. An explicit article-header value
overrides that whole key. This is a declared source-inventory policy, **not**
a claim that the index has verified DocFX's publishing precedence or selected
the publisher's winning value. One article can therefore contribute to several
observed values for a field, while still counting only once for each value.

After confirming the exact keys and values in `--filters`, use them verbatim:

```text
fab-rs search docs --filters --format text
fab-rs search docs --text "lakehouse" --filter "ms.topic=conceptual" --facet ms.topic --facet workload
fab-rs search docs --text "lakehouse" --filter "ms.topic=conceptual" --filter "ms.topic=how-to"
fab-rs search docs --text "lakehouse" --filter "item_type=Lakehouse" --facet item_type
fab-rs search docs --suggest "firewal" --filter "ms.topic=conceptual"
```

The example `ms.topic` values are source vocabulary, not a promise that every
profile or future snapshot contains those values. Select a workload from that
same inventory before adding `--workload EXACT_VALUE`. Repeated `--filter`
values for the same field are ORed; different fields are ANDed. Matching is exact
and case-sensitive. Unknown fields, unknown values, and malformed clauses are
errors, not successful no-hit responses. A value may contain `=`; only the first
`=` separates the field from its value. At most 32 explicit metadata-filter
clauses are accepted. Search `--workload` is a separate legacy scalar and does
not consume one of those slots. Suggestions pass at most 32 normalized metadata
filters to the SDK; their explicit workload needs one slot unless workload
alternatives already supply it. Every original clause is validated before that
normalization. When combining `--workload` with `--filter workload=...`,
the explicit workload must be among those values and narrows the result to that
one workload; contradictory selections are rejected.

Source metadata may have no item-type field. `--filters` explicitly reports
which exact source spellings are observed among `item_type`, `itemType`, and
`ms.item_type`, with source revision and coverage. `missingItemType` is true only
when all three are absent from an available metadata catalog; it is unavailable
when the profile has no metadata inventory. The catalog retains exact keys, not
a merged or renamed item-type vocabulary.
Mentioning a Lakehouse, Warehouse, or semantic model in an article does not make
the article carry an item-type metadata value. Missing fields are omitted, never
filled from mentions. Use the actual inventory rather than assuming an item-type
taxonomy or that a source workload named `onelake` exists.

For the pinned source revision, metadata-enabled profiles apply the owner-reviewed
supplement in `rust/engine/control-plane/search/docs/data/item-types.json`.
It classifies exactly ten named articles using existing Fabric API spellings
in canonical `item_type`; for example, the reviewed Lakehouse overview can be
selected with `--filter item_type=Lakehouse` after verifying that value in
`--filters`. This is not a claim that the original frontmatter supplied that
field, or that all articles mentioning Lakehouse are classified as Lakehouse.
No directory-prefix, body-mention, navigation-card, or code-example rule expands
the map. Unmapped articles remain unknown unless source authors supply a type.
The map applies only to its exact source commit with metadata enabled;
conflicting authored item-type values fail the build, as do missing or
non-indexed mapped inputs. Other revisions, including synthetic test revisions,
do not inherit it.

`--filters` reports whether the reviewed supplement is applied, using the
explicit manifest attribution. `--corpus-info` retains its source pin, exact
paths, evidence, and policy in `TRANSFORMATION-ATTRIBUTION`. Keep that provenance
with exported facet counts rather than treating curated classifications as
original source-authored metadata or a complete Fabric taxonomy.
For this exact pinned repository snapshot, coverage is **10 dedicated curated
articles and 0 source-authored item-type articles** across all three source
spellings. `--filters` reports both counts. Curated counts come from distinct
article evidence paths in the transformation notice; the zero authored count
is a fact of the verified immutable source inventory, not a default for other
revisions. Unverified or unavailable coverage is null/unavailable rather than
an invented zero.

`--suggest PREFIX` takes one alphanumeric token of 2-20 characters and returns
actual indexed article titles or headings containing a word with that prefix,
plus their source path and workload. The input is at most 80 UTF-8 bytes, and its
Unicode lowercase form must also fit within 20 characters: some characters
lowercase to multiple Unicode scalars. It supports `--limit`, exact `--filter` clauses,
`--workload`, and `--format`; search-only controls such as `--prefix`,
`--no-stemming`, `--facet`, `--candidate-limit`, and `--fuzzy` are rejected even
when supplied with their default values. Corpus information and the filter catalog
accept only presentation options, not query flags. Suggestions require an
autocomplete-enabled index; filtered suggestions also require metadata postings.
Suggestions are collected from a finite pool of up to 500 matching source
sections. They are bounded source-backed completions, not an exhaustive catalog
of every matching title or heading. Many matching sections do not by themselves
cause an expansion-limit error; term-vocabulary expansion is a separate bound.
Choose a longer prefix or exact filters to narrow overly broad completions.

## Corpus and provenance

The initial source is the English `MicrosoftDocs/fabric-docs` snapshot
`3ae7afc73169ac57b874d5aee09217bb35a4f8fc`. Its Git tree contains 3,258 Markdown
files under `docs/`, totaling 26,332,581 source-blob bytes. Checkout byte counts
can differ with line endings; neither number is an index-size measurement.

The Osmos pipeline's `refresh_fabric_knowledge.py` also vendors
`microsoft/skills-for-fabric`. This source is intentionally **not included** in
the initial docs-only index. Fabric's publishing configuration references
external Power BI, Kusto, Power Query, and reusable-content repositories.
Unavailable external includes are disclosed in the bundle manifest. This index
doesn't claim to contain every linked Learn article.

The producer extracts article titles, headings, prose, code, and supported landing
content. Local includes are resolved within the supplied snapshot. Image payloads
aren't selected by acquisition or embedded in the index. Include fragments and
navigation aren't misleadingly returned as standalone articles.
Explicit `robots` metadata containing `NOINDEX` excludes the article uniformly
across profiles. The manifest's `skipped_inputs` records
`<path>: robots NOINDEX metadata`; catalog counts cover only retained indexed
articles, not excluded articles or every raw source Markdown file.

Results retain their source path, immutable source URL, published article URL,
section identifier, workload, and local score. Metadata-enabled profiles add
exact source metadata. Hierarchy-enabled profiles retain ancestor heading paths
and explicit source heading anchors. The source content license is
CC BY 4.0; code has a separate `LICENSE-CODE`. The bundle retains source notices
and transformation attribution, accessible offline through `--corpus-info`.
The CLI's MIT license doesn't relicense documentation.

## Build a self-contained executable

From `rust/`, on an authorized build host:

```text
python -B scripts\prepare_docs_corpus.py --output <new-absolute-corpus-directory>
cargo run --locked --release -p fabric-docs-search --features index-builder --bin build-docs-index -- --source <absolute-corpus-directory> --output <new-absolute-bundle-directory> --revision 3ae7afc73169ac57b874d5aee09217bb35a4f8fc --compression lz4 --profile full
```

Set `FAB_DOCS_INDEX` to that bundle directory in the build process environment:

```text
cargo build --locked --release -p fabric-cli --bin fab-rs --features docs-search
```

The package's build script verifies every declared component length and SHA-256,
the bundle schema, and the pinned Tantivy version before including its bytes.
Changing `FAB_DOCS_INDEX` or component files invalidates the Cargo build input.
An unset variable deliberately produces an engine-only build with an explicit
not-configured error, not a full-corpus distribution. Preserve the immutable
bundle and manifest with the executable's build receipt.

The acquisition and index tools are development-time tools. Shipped search
requires neither Python nor Rust, network access, extraction, nor a writable
directory. New docs require rebuilding the index and executable.

### Source-profile ablations

The builder accepts exactly `--profile baseline|stemmed|metadata|hierarchy|autocomplete|full`;
the `|` notation lists alternatives and is not one argument. The default is
`full`. Every profile uses the same pinned source, article selection, and sectioning.
Profiles change declared index additions, not the corpus, and the manifest's
`features` object records what is available.

| Profile | Optional index additions |
| --- | --- |
| `baseline` | None; lexical title, heading, body, and identifier postings remain |
| `stemmed` | Additional English-stemmed prose postings |
| `metadata` | Source metadata, description/alias retrieval, and exact filter postings/catalog |
| `hierarchy` | Ancestor-heading context and explicit source heading anchors |
| `autocomplete` | Dedicated bounded title/heading edge-ngram completion postings |
| `full` | All four additions |

Build separate new bundle directories before embedding each into a matched
executable. For example, in PowerShell from `rust/` on an authorized build host:

```powershell
$corpus = 'C:\docs-search\fabric-docs'
$bundles = 'C:\docs-search\bundles'
foreach ($profile in @('baseline', 'stemmed', 'metadata', 'hierarchy', 'autocomplete', 'full')) {
    cargo run --locked --release -p fabric-docs-search --features index-builder --bin build-docs-index -- --source $corpus --output "$bundles\$profile" --revision 3ae7afc73169ac57b874d5aee09217bb35a4f8fc --compression lz4 --profile $profile
    if ($LASTEXITCODE -ne 0) { throw "Index build failed for $profile" }
}
$env:FAB_DOCS_INDEX = "$bundles\full"
cargo build --locked --release -p fabric-cli --bin fab-rs --features docs-search
```

The corpus must already be prepared, and each output leaf must not exist.
Retain each executable and exact build receipt before building the next profile.
Do not compare a full default query to a reduced profile that silently ignores
requested features: missing features fail explicitly. Use `--no-stemming` with
profiles without stemmed postings. Metadata filters/facets and suggestions
require their respective additions. `--corpus-info` remains available for every
embedded profile. `SearchOptions` and the legacy SDK search API are unchanged;
the richer CLI uses the separate `RichSearchOptions` API.

Article grouping and deterministic reranking use the original stored source
fields, so neither requires hierarchy postings. Synonyms are query-time behavior
and need no added index feature. Search `--prefix` uses original lexical
dictionaries and does not require the separate autocomplete feature.
When metadata or hierarchy is disabled, corresponding `metadata` and
`heading_path` hit fields are empty rather than fabricated.

Search `--workload` alone retains the legacy structural filter even without
metadata postings. In a metadata-enabled profile its value must appear in the
catalog; a metadata-disabled profile has no such inventory and can return no
hits for an unobserved, syntactically valid workload. Explicit
`--filter workload=...` and `--filter source_path=...`, all facets, and filtered
suggestions require metadata postings. Explicit field-qualified queries against
`body_stem`, `description`, `aliases`, or `heading_context` require the corresponding
stemmed, metadata, or hierarchy feature rather than silently using another field.

## Search behavior and tradeoffs

Tantivy is pinned to 0.27.0, with dependency defaults disabled and explicit
stored-document LZ4/Zstd codecs. Indexes use section-level records, frequencies,
positions, field norms, stored bodies, and separate identifier tokenization.
Title and heading boosts prioritize those fields; scores aren't probabilities
or comparable with remote relevance scores.

Queries default to conjunction and support explicit Boolean operators, quoted
phrases, and phrase proximity. `--workload` is an exact score-neutral filter.
`--fuzzy` permits edit distance 1 or 2 for at most eight tokens of at least three
characters each; fuzzy phrases aren't accepted. General wildcard and regex
syntax are disabled. The library rejects empty queries, more than 2,048 input
bytes or 64 analyzed tokens, nesting beyond eight levels, and result limits
outside 1-50. A result cap isn't a CPU-time bound. The synchronous search API
doesn't promise preemptive cancellation.

The richer lexical defaults are English stemming, reviewed synonym expansion,
article grouping, and deterministic reranking. `--no-stemming`, `--no-synonyms`,
`--sections`, and `--no-rerank` disable each behavior independently. This is
lexical retrieval, not embeddings, semantic similarity, a trained ranking model,
or a probability. Synonym expansion uses a small reviewed Fabric acronym
dictionary for plain-text queries only; it is not a general thesaurus. Boolean
and phrase syntax retain their literal query semantics rather than being
silently rewritten.

The query-time dictionary has exactly four reviewed entries; it does not learn
aliases from query traffic or add arbitrary product-name associations:

| Input token | Additional phrase | Terminology source |
| --- | --- | --- |
| `RLS` | `row level security` | [Fabric row-level security](https://learn.microsoft.com/en-us/fabric/security/service-admin-row-level-security) |
| `OLS` | `object level security` | [Fabric object-level security](https://learn.microsoft.com/en-us/fabric/security/service-admin-object-level-security) |
| `ADLS` | `azure data lake storage` | [Azure Data Lake Storage](https://learn.microsoft.com/en-us/azure/storage/blobs/data-lake-storage-introduction) |
| `KQL` | `kusto query language` | [Kusto Query Language](https://learn.microsoft.com/en-us/kusto/query/) |

These are token alternatives, not replacement of a whole query. Applied
alternatives are reported in `expansions` and share the finite `--max-expansions`
budget with prefix alternatives. Source-authored metadata aliases are a separate
metadata-profile retrieval field, not permission to enlarge this dictionary.

`--prefix` opts in to expansion of the final plain-text token. It is separate
from title/heading suggestions. General wildcard/regex syntax remains disabled.
`--max-expansions` defaults to 64 and accepts 1-128. Excessive expansion fails
explicitly rather than silently dropping alternatives; use a longer prefix,
or raise the search expansion budget within its bounds.

The candidate pool defaults to 200 sections; `--candidate-limit` accepts 1-500
and cannot be smaller than `--limit`. The engine reranks only fetched candidates
using deterministic exact-title, heading, and identifier evidence, with the
additive `rerank_bonus` separate from the source BM25 `score`. Article grouping
selects a representative matching section per exact source article by default.
`--sections` returns section results instead. A finite pool can omit matching
articles even when the displayed result limit is not filled; `candidatesLimited`
reports that limitation. Disabling reranking does not invent an alternative
relevance score.

The larger pool broadens collection beyond the displayed result cap; it does
not itself relax the query's conjunction into OR matching. A query with no
matching candidates can still return no hits, and reranking cannot synthesize
missing matches. Advanced Boolean and quoted-phrase criteria are preserved
exactly, without a hidden OR fallback or synonym/prefix rewrite.

Repeat `--facet FIELD` for at most eight observed fields. Facets count unique
**matching source articles**, not section occurrences, displayed hits, or only
the top ten results. `--facet-budget` defaults to 5,000 matching sections and
accepts 1-10,000. When the budget truncates examination, `facets.complete` is
false and `examined_sections` records the coverage; text output warns that counts
are partial. Do not present partial counts as corpus-wide totals. The budget
bounds examined records, not elapsed CPU time.

### Output evidence

Rich JSON retains `status`, `source`, `data`, and `meta`. Each `data` entry keeps
the original hit fields flat (`id`, `title`, `heading`, `url`, `source_url`,
`source_path`, `workload`, `score`, and `snippet`) and adds `article_id`,
`heading_path`, `metadata`, `highlights`, and `rerank_bonus`.
`meta` preserves `sourceCommit`, `indexedSections`, `resultLimit`, `openUs`, and
`timings`, and adds `candidatesExamined`, `candidatesLimited`, applied `expansions`,
query-local `facets`, `indexFeatures`, and `sourceCoverage`.
The original coverage description and unresolved-include count are retained.

Highlight `start`/`end` values are inclusive/exclusive **UTF-8 byte offsets into
the raw JSON snippet**, not character indices, HTML, or offsets into sanitized
terminal text. Text output shows heading hierarchy and literal `[[...]]`
highlight markers; it does not automatically emit ANSI styling. It validates
the raw UTF-8 boundaries before escaping control characters for display.
Ordinary Unicode, path separators, and printable punctuation are preserved.
Escaped terminal controls can change displayed lengths, so never reuse JSON
offsets against the displayed text.

Not-embedded builds report `docs_search_not_configured`. Missing profile additions
report `docs_search_unsupported_feature`; invalid query/metadata/bounds report
`invalid_arguments`; excessive expansion reports `docs_search_expansion_limit`.
Diagnostics do not echo rejected query text or supplied filter values. These
failures do not initialize credentials or fall back to a remote search.

Positional postings enable phrase search but increase size. Stored bodies enable
offline snippets; postings alone cannot recover the original text. LZ4, Zstd,
and no compression trade stored-text bytes against retrieval work. Compressing
the entire bundle would sacrifice direct static-slice reads. Static component
backing avoids the RAM-copy control's full-index copy, but metadata and
decompressed stored blocks still allocate. One merged segment reduces repeated
structures but doesn't provide intersegment search parallelism.

The producer pins inputs and emits a content-addressed component inventory.
Random segment identifiers mean sorted input alone doesn't establish
byte-identical independent index regeneration.

## Measurement protocol

Compare matched release builds with docs disabled, the engine enabled without a
bundle, and the engine with a full embedded bundle. Record target, compiler,
features, lockfile, source digest, corpus revision, component hashes, and exact
executable bytes. Keep index bytes distinct from the final executable delta.
The RAM-copy reader is a separate loading-strategy control.

The benchmark tooling in `tests/performance/docs_search.py` measures fresh-process
help/version startup and full docs invocations. The crate's `benchmark` example
reuses one reader for repeated queries and reports open time separately from
query preparation, top-K search, stored-text fetch, and snippets. Cases include
source-backed Fabric terms, phrases, Boolean expressions, proximity, workload
filters, fuzzy matching, dotted configuration names, and a no-hit control.
They are representative domain queries, **not collected user search traffic**.

Report sample counts, median/p95 distributions, raw observations, and expected
source-hit judgments. A new process is process-cold, not proof of an OS-cold
filesystem cache. Shared-host timings aren't clean-machine or cross-platform
performance guarantees. Measurements must come from actual completed binaries;
source corpus sizes and compile-time estimates aren't substitutes.

For rich-feature ablations, additionally retain the exact profile, query toggles,
candidate/expansion/facet budgets, applied expansions, facet completeness, and
article-versus-section mode. Compare each optional addition against the same
source/profile baseline and report size, startup, memory, and query-stage costs
separately. This guide makes no unmeasured improvement or overhead claim;
impact numbers belong to completed, revision-bound measurements.

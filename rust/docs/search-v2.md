<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded search preview

Search locates metadata candidates. It does not retrieve item definitions,
execute data queries, search all item contents, or grant access to data.
Python `fab find` and the existing `fab-rs` compatibility surface are unchanged.

The explicit resource head is `fab-cli-v2`, built with `headless`. It requires
the existing explicit identity configuration; search never starts interactive
sign-in, changes consent, or selects another identity after a denial.

The native Catalog binding uses the public Fabric API. Relevance and hybrid
also require an explicitly approved `FAB_RUST_RELEVANCE_HOST` and
`FAB_RUST_RELEVANCE_AUDIENCE=fabric` selection for this preview's existing
Fabric-audience policy. No relevance authority or alternative token resource
is inferred from the controller path. The operator must establish that this
binding is admitted by the service; source inspection and a configured
hostname do not prove deployment or consent. No setting is written by search.

```text
fab-cli-v2 search / --text sales
fab-cli-v2 search / --mode relevance --text "revenue performance"
fab-cli-v2 search / --mode hybrid --text "revenue performance"
fab-cli-v2 search /workspaces/<workspace-uuid> --type Lakehouse
fab-cli-v2 search / --schema
```

`/` selects the explicitly configured tenant collection.
`/workspaces/<workspace-uuid>` selects that exact workspace collection, not a
workspace display name. Other paths, item contents, `docs/`, files, shortcuts,
tables and command-catalog search are outside this preview.

## Modes and permissions

| Mode | Behavior | Coverage |
|---|---|---|
| `catalog` (default) | One page from the public Preview Catalog API, with native type and workspace filters. | Metadata the caller can discover; a terminal page is not proof of complete tenant inventory. |
| `relevance` | Explicit hosted, relevance-ranked artifact discovery. | Bounded top-k retrieval; no exhaustive-coverage or stable-ranking guarantee. |
| `hybrid` | Independently authorized Catalog and relevance discovery, with a bounded merged result. | Source outcomes remain independent; missing, denied, limited and failed sources are not hidden by useful results from another source. |

Catalog requires the documented delegated `Catalog.Read.All` permission, as
well as the caller's applicable Fabric access. Relevance discovery has its own
controller scope requirements. Catalog permission is not a substitute for them.
Client credential configuration does not prove that consent or resource access
has been granted. Read-only search does not request write permissions merely
because a service also accepts them.

Relevance workspace scoping uses the native exact workspace-ID restriction,
not a global search or workspace-name lookup. Responses must retain the
requested ownership. If a configured provider cannot represent the exact
requested scope or type filter, validation fails before either hybrid provider is called.
There is no DataHub, WorkIQ, alternate-identity or broad enumeration fallback.

Hosted relevance may use model-backed semantic signals. Selecting it is not
the same operation as deterministic Catalog text matching, and the CLI does
not add query expansion, answer generation or a hidden agent invocation.

## Arguments and bounds

Use `--text` for the query. Catalog additionally supports repeated `--type` and
`--exclude-type` values; these are typed filters, not raw OData expressions.
A filter-only Catalog request is allowed. An unconstrained tenant listing with
no text or filter is rejected.

`--limit` defaults to 50. Catalog accepts 1 through 1,000 as its page bound;
relevance and hybrid accept 1 through 50 as this preview's result bound.
The latter is a client policy, not a promise of service capacity or cost.
Catalog filters allow at most 500 total include/exclude type values, each at
most 50 characters, and 12 workspace UUID values in its SDK request.
Unknown service type names are not evidence of supported item capabilities.

`--timeout` defaults to `30s`. It accepts a positive integer followed by `ms`,
`s`, `m`, or `h`, up to 24 hours. One host deadline covers credential
acquisition and service execution. Local cancellation or timeout does not
assert that server-side work stopped.

`--max-bytes` defaults to 1,048,576. Responses and unprojected/final delivery
are bounded before success output. It is not a server scan, billing or total
tenant-size bound. Only `--format json` is supported.

## Continuation and merge

Catalog-only continuation uses the returned native cursor object from
`meta.sources[].details.continuation`. It retains the original tenant/workspace
scope alongside the unmodified service token:

```text
fab-cli-v2 search <same-scope> --cursor '<returned-cursor-JSON>'
```

Use the same explicit caller/profile. The cursor's tenant must match the
selected identity and its scope must match the addressed path before service
execution. This association is not a signature or a grant of access; the
service remains authoritative. An unscoped raw token is not accepted by this
head because its original scope cannot be safely reconstructed.

Do not combine a continuation with fresh text, type filters or a different
scope. The service token retains its originating criteria; the client neither
rewrites it nor uses it to select a host or credentials. A continuation may
retain a larger original page size: do not lower the delivery bound and assume
that a next-page token can recover discarded rows.

There is no invented relevance or hybrid continuation. A hybrid global cap
can leave fetched candidates undisplayed; it is disclosed as limited rather
than presented as a fully resumable combined result.

Deduplication uses verified, compatible same-tenant artifact identity, never
display name. Type and workspace conflicts are errors, not opportunities to
overwrite identity. Overlapping hits retain source provenance. Results
preserve source-local ordering; Catalog order and relevance scores are not
treated as a common score.

## Output and evidence

`--project` (`-q`) applies JMESPath to `data` only. It can return a scalar or
null without removing status, target identity, source errors or coverage.
Failure receipts that have no data retain their complete error and metadata.
Invalid projection syntax fails before provider setup.

`--output -` selects stdout. An explicit local filename is preflighted before
remote work and created exclusively, without overwrite or directory creation.
Writer, flush and raced-create failures are errors; they do not replay search.
A failed write is not a complete output-file claim.

Partial, failed and unknown outcomes are nonzero. Empty successful results are
distinct from malformed responses, authorization failures or disabled sources.
Machine-readable source evidence must not be hidden behind a debug flag.

`--schema` inspects the SDK-owned operation contract without credentials,
transport, payload files or provider calls. Schema exposure is not a running
MCP server or proof that a provider is enabled for a tenant.

## Evidence boundary

The public [Catalog Search API](https://learn.microsoft.com/rest/api/fabric/core/catalog/search)
is Preview. Source-backed implementation, offline mocks and successful
compilation do not establish deployment, live authorization, indexing
freshness, complete artifact coverage or production identity/platform support.
Those require separate acceptance evidence.

The shared validation graph includes the service integration tests, public SDK
facade, Search wire/parser/binary targets, and the existing native GET binary
regressions. Its combined preview profile composes Search with the separate
Ontology and Workspace previews and existing native, cancellation, copy,
runtime, discovery, identity, REPL, and update features. These remain opt-in
surfaces; Search does not replace the default `fab-rs` grammar or a preview
executable.

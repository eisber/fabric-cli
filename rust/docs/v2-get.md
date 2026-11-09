<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Experimental v2 get

The opt-in `fab-cli-v2` execution head composes explicit retrieval over shared
SDK operations. The existing `fab-rs` executable, GET parser and compatibility
oracles are unchanged. This is a bounded supported-capability slice, not a
claim that every proposed representation or artifact backend is implemented.

Build the separate execution head with the existing explicit headless host:

```text
cargo build --offline --locked -p fabric-cli --features headless --bin fab-cli-v2
fab-cli-v2 get --help
```

Help, version, invalid arguments and existing-output preflight do not acquire
tokens. Actual service execution requires an explicit `FAB_RUST_AUTH_MODE`
and its existing [headless identity configuration](native-read-host.md#headless-credentials).
There is no interactive sign-in, ambient credential chain, static-token option,
private API activation or implicit profile switch. Native transport explicitly
uses platform trust and direct connections; this head does not discover proxies.

## Addressing and capability boundaries

```text
get /Sales.Workspace/Forecast.Notebook
get /Sales.Workspace/Forecast.Notebook/src/definition --output definition.json
get /Sales.Workspace/Model.SemanticModel/src/definition/formats/TMDL
get /Sales.Workspace/Forecast.Notebook/src/definition/parts/notebook-content.ipynb --format raw
get /Sales.Workspace/Bronze.Lakehouse/Files/input.bin --format raw --output input.bin
get /operations/<operation-id> --wait
get /operations/<operation-id>/result --wait --format raw
get /Sales.Workspace/Forecast.Notebook/runs/<job-id> --wait
```

Names resolve against complete bounded authorized workspace/item listings.
Ambiguity is an error, never a first match. Explicit GUID selectors avoid
enumeration; machine identity paths are also accepted:
`/workspaces/<workspace-id>/items/<item-id>`. Names containing reserved separators
or percent-escaped segments are not advertised by this initial grammar. Folder
resolution and non-item service scopes are not silently approximated.

A declared item suffix remains a type constraint even with GUID selectors.
Authoritative item metadata must match that type before metadata is delivered or
any definition, file, run or documentation representation is fetched. This check
uses the same invocation deadline and byte budget. Untyped machine paths remain
valid and do not acquire an implicit owner-type constraint.

| Target | Binding |
| --- | --- |
| Workspace | Primary metadata only |
| Item | Generic metadata only; no implicit definitions/connections/schedules |
| `src/definition` | Raw native multipart definition envelope |
| `src/definition/formats/<format>` | Explicit syntactically validated native service format |
| `src/definition[/formats/<format>]/parts/<path>` | One exact InlineBase64 part |
| `Files/<path>` | Exact OneLake bytes through a storage-scoped client |
| `operations/<id>[/result]` | Existing generic Fabric operation observation/result |
| Item `runs/<id>` | Existing item-job observation; generic result/log aliases unsupported |
| Item `docs[/<view>]` | Only an explicitly injected admitted documentation provider |

Definition retrieval is a semantic read implemented by a service POST and may
require read-and-write item permissions. A 202 response is acceptance, not
completed source: it retains its returned operation handle. An advertised wait
observes that exact handle without replaying the POST. A still-accepted response
cannot be committed as a completed source file. Native parts retain exact paths,
order, encodings and Notebook outputs. Missing, duplicate or unsupported part
encodings fail; there is no extraction, filename normalization or conversion.

Definitions, PBIX/RDL downloads, rendered reports, OWL conversion and packaging
are different capabilities. PBIX/render/bulk/OWL, source-context private routes,
schema YAML, agents.md, notebook cells, secret settings and unbound facets remain
explicitly unsupported in this head.

Explicitly typed `.Report` metadata and definitions use the
[Report-native binding](report-native-reads.md). It preserves binary/native
parts and retains format/version and related-model prerequisite evidence outside
projected data. Unknown native versions or missing report structure are partial,
not complete authoring exports. Untyped item paths retain the generic binding.

Explicitly typed `.Warehouse` metadata uses the native Warehouse metadata GET,
without definition or connection enrichment. Warehouse source, docs, file and
run representations are not bound in this head; they fail before acquisition
rather than falling through to generic item source. The existing explicit-budget
get/list head supplies the separate [Warehouse connection and restore delegates](warehouse-management-reads.md#bounded-cli-consumer).

## Options, data and local delivery

`--format json|markdown|raw` chooses an advertised serialization, not the native
service format or remote operation. Metadata is JSON only. Docs prefer Markdown;
native file content prefers raw. `--project/-q` is local typed JMESPath on
retrieved data: null, boolean and numeric values remain JSON values. Projection
never changes acquisition or hides source budget failure. Raw conflicts with
projection. SDK result envelopes keep observation/coverage metadata outside the
projected `data`.

The isolated preview defaults are **1 MiB** and **30 seconds**, selected by
`--max-bytes` and `--timeout`. These are not global v1 defaults. One invocation
shares its response-byte and 100-dispatch allowance across discovery, source,
status and result reads. Final unprojected data and encoded delivery are also
bounded. The total deadline covers setup, resolution and acquisition; it is not
reset per poll/page. An exact native raw payload at its byte cap is not charged
for an unselected JSON/base64 envelope.

`--output/-o FILE` selects that exact local file; omitted output or `-` means
stdout. Relative paths resolve against the execution head's explicit native
working directory. Existing files, directories and symlinks fail before remote
content reads. Exclusive create enforces no-clobber at delivery, including a
racing creator. There is no overwrite flag, automatic directory, timestamp,
extension, OneLake destination or silent stream fallback.

Remote acquisition is fully buffered and validated before delivery. A failed
read cannot create a valid-looking source prefix. Local write/flush failure may
leave partial effects after exclusive creation; typed evidence retains stage,
creation and byte counts. No rollback, atomic full-file replacement or durable
storage guarantee is claimed. Ancestor namespace stability is caller-owned.

## Observation and docs coverage

Status-only GET can successfully report failed remote work. `--wait` on failed,
cancelled or deduplicated work fails with the last observed status retained.
Unknown states, expired handles, malformed delays and unadvertised results fail
explicitly. Result retrieval requires the exact selected-endpoint result
advertisement. Deadlines/cancellation stop local observation, not remote work:
there is no submit retry, remote cancellation or rollback.

The shared SDK docs API accepts canonical owner/view/stage/revision-selector
keys and already-served provider pages. It never interprets Markdown/URLs as
authorized navigation, calls a model/query or falls back to authored context.
There is no built-in FabricAIHub private API adapter. The hosted provider's
Markdown may be reused through an admitted contract, but native JSON schema,
full coverage, rollout and caller authorization require separate evidence.

Recursive docs are opt-in. Root depth is zero; the head advertises default depth
one when recursive. The graph has finite 100-node/10-page limits and the remaining
invocation byte/deadline allowance. Only advertised structural children expand;
cross-owner expansion requires an explicit `docs/related` subtree. Canonical
owner/view identities terminate cycles and preserve duplicate relation edges.
Repeated cursors, changed revisions/snapshots and mid-acquisition budget failures
never become exhaustive success.

Reaching the requested depth or not expanding a reference is a declared coverage
boundary, not a truncated artifact. Coverage remains in JSON/Markdown and after
projection. A denied/unavailable descendant is labelled partial; root failure,
unrequested viewport truncation, budget exhaustion and drift fail with retained
evidence rather than emitting a clipped source. A bounded graph is not complete
tenant lineage or proof of permissions on related items.

## Validation scope

Offline SDK, CLI-backend, parser/delivery and subprocess contracts exercise
supported positive/error paths, limits, cancellation, observer/source composition,
binary/empty output, races and coverage. Default v1 regression and architecture
contracts remain independent. Native Windows compilation and offline subprocess
execution do not establish live service authorization or Linux/macOS runtime
acceptance. The integration handoff records actual commands and remaining gates.

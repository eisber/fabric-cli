<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Public DataAgent read bindings

`fabric_sdk::dataagents` is an additive public read slice. It runs through the
caller's existing Fabric `OperationExecutor` and immutable `Client` request
context. No transport, runtime, identity provider, HTTP retry/polling engine or
CLI parser is installed. The v1 generic item policy and DataAgent ontology
package are unchanged.

## Explicit CLI composition

The existing experimental `fab-cli-v2` GET head now adapts these SDK reads.
Its shared resolution, invocation budget, cancellation, observation, projection
and exact-output contracts remain authoritative. There is no second HTTP engine,
global command registry version, ontology handler or credential selector.

```text
fab-cli-v2 get /Demo.Workspace/Agent.DataAgent
fab-cli-v2 get /Demo.Workspace/Agent.DataAgent/src/definition
fab-cli-v2 get /Demo.Workspace/Agent.DataAgent/src/definition/formats/JSON
fab-cli-v2 get /Demo.Workspace/Agent.DataAgent/src/draft
fab-cli-v2 get /Demo.Workspace/Agent.DataAgent/src/published
fab-cli-v2 get /Demo.Workspace/Agent.DataAgent/src/draft/stage_config.json --format raw
fab-cli-v2 get /Demo.Workspace/Agent.DataAgent/settings --stage draft
fab-cli-v2 get /Demo.Workspace/Agent.DataAgent/datasources --stage published
fab-cli-v2 artifact data-agent get /Demo.Workspace/Agent.DataAgent/settings --stage draft
```

The facade constrains the owner to `DataAgent` and invokes the same GET parser,
backend and delivery path. Explicit identifiers can retain the typed spelling
`/<workspace-id>.Workspace/<item-id>.DataAgent`; this avoids name enumeration.
New stage/configuration paths reject untyped machine addresses and other owner
types. The SDK's native metadata response must independently verify item,
workspace and DataAgent type before source or configuration acquisition.

`--stage draft|published` is required for settings and datasource-page reads.
`draft` maps to the native `staging/` configuration route; published uses the
native base route. Metadata has no fabricated stage. Source selection belongs in
`src/draft` or `src/published`, not `--stage`; redundant, conflicting, missing,
unknown or inapplicable selectors fail explicitly. Only JSON is an admitted
explicit DataAgent native format. Configuration Preview support and permission
remain service-owned; no denied or unavailable read falls back to another stage.

`src/definition` returns the **full native envelope**, retaining all part paths,
order, duplicates, opaque payloads and unknown fields. It is not stage-filtered.
`src/draft` and `src/published` make **local addressed projections of that same
full definition POST**, never a stage-specific definition endpoint. Their data
contains the unchanged `nativeDefinition` and ordered `selectedPartIndices`.
`meta.dataAgentSource` inventories every original part and separately identifies
draft, published, known shared and unclassified parts. Unknown paths are not
silently treated as shared. A missing selected stage fails instead of producing
an empty success. These views explicitly set `fullSourceRoundtrip: false`;
they are not replacement definitions or historical snapshots.

A child such as `src/draft/stage_config.json` addresses exactly
`Files/Config/draft/stage_config.json`. It uses the shared strict InlineBase64
decoder without interpreting stored instructions, few-shot queries or payload
bytes. Missing/duplicate exact paths, unknown encodings and invalid base64 fail.
JSON delivery retains the complete part inventory and selected native path
outside projected data. Raw delivery is only the exact selected bytes, without
a receipt prefix; use JSON or the stage root when inventory evidence is needed.
The full source is bounded before any stage, part or JMESPath projection.

`get .../datasources --stage ...` retrieves **one native page**, preserving its
opaque token and inert continuation URI. `meta.scope` is `one-native-page` and
`meta.collectionComplete` reports the service continuation state; page delivery
does not claim complete collection traversal. This adapter does not replace or
extend a shared global `list` handler or implement its own traversal/cursor
contract. The existing SDK registry's generic/facade List bindings remain
unchanged. No mutation, publish, reset, unlink, datasource query, hosted answer
or model operation is installed.

Build and run with the existing explicit headless or Windows WAM host described in
[v2 GET](v2-get.md). WAM requires the existing `native-read` feature and explicit
`FAB_RUST_AUTH_MODE=broker`, with the existing selected broker binding or
complete explicit tenant/account/client selectors. No login, new consent or profile change is performed by a
read. For an explicitly approved MSIT environment, select
`FAB_RUST_FABRIC_HOST=msitapi.fabric.microsoft.com` while retaining the Fabric
audience `https://api.fabric.microsoft.com/.default`; the portal hostname is
not the REST endpoint. The host requires its already configured, approved
`FAB_RUST_AUTH_MODE` and matching non-secret identity/provider inputs. WAM
silently uses that selected Windows account; it cannot choose the first desktop
account or manufacture a binding.
Offline success does not establish live identity, endpoint routing, permissions,
stage availability or Preview deployment.

## Targets, requests and results

`DataAgentTarget::new` requires a resolved `DataAgent` owner and nonzero
hyphenated workspace/item GUIDs. Name and legacy AISkill alias resolution belong
to the caller. Invalid targets fail before credentials or HTTP.

| Request | Native Fabric v1 route | Result and permission |
| --- | --- | --- |
| `DataAgentRequests::metadata` | GET `workspaces/{ws}/dataAgents/{id}` | Ordered metadata with verified item/workspace/type; Item read |
| `DataAgentRequests::definition` | POST `workspaces/{ws}/dataAgents/{id}/getDefinition` | Existing `DefinitionRead`: full envelope or real accepted operation ID; Item read/write |
| `DataAgentRequests::settings` | GET `.../{staging/}settings` | Inert stored `aiInstructions`, unknown fields preserved; configuration read |
| `DataAgentRequests::datasources_page` | GET `.../{staging/}datasources?continuationToken=...` | One native datasource page, management IDs and references kept separate; configuration read |

Metadata/definition have no stage selector. The full native definition may
contain both draft and published parts. Settings and datasource requests require
`Stage::Staging` or `Stage::Published`; there is no implicit default, fallback,
historical snapshot or private host variant.

The definition request accepts provider default or explicit `NativeFormat::Json`
(`format=JSON`). The service, not an output serializer, determines that native
format. No TMDL, PBIX, OWL or source conversion is offered. The existing decoder
retains unknown fields, part paths/order, duplicate paths and opaque payloads.
Exact part-byte selection uses its existing strict InlineBase64 decoder and
reports ambiguous/unsupported parts. There is no extraction or file I/O here.

Caller-supplied `RepresentationLimits` bound raw responses/results; no new
product numeric defaults are introduced. Definition decoding also enforces its
existing compact-JSON result bound. The injected executor owns finite deadlines,
streaming limits and redirect/retry policy. Pass cancellation or the caller's
total-deadline future; future drop/cancellation never requests remote cancellation.

Only HTTP 200 completes these reads. Definition HTTP 202 returns the actual
shared observer handle without polling or replaying the POST. Malformed/foreign
operation links and missing handles fail explicitly. Configuration GET acceptance
is unsupported, not a complete empty result. Service/credential errors preserve
the original shared evidence; definition denial never falls back to metadata.

The original operation session and caller cancellation/deadline future remain
alive through native JSON decoding and definition result-size accounting. A
final signal checkpoint runs before returning either decoded content or an
accepted definition handle. No new timer or observation budget is started.
Cancellation at this boundary returns `DataAgentError::Response`, retaining the
original session error and the already-bounded HTTP status, body and headers,
including any real accepted operation ID. Result-byte limit failures also retain
this evidence, including a decoder's compact-JSON accounting limit when numeric
serialization expands beyond an otherwise in-budget raw response. Those
failures retain the shared session's `Limit` classification; a ready original
cancellation signal still takes precedence at the final checkpoint. Neither
the raw nor compact byte limit is relaxed. Response content stays inert and is
excluded from Debug/Display;
it is not a completed read and never authorizes replay or remote cancellation.

Datasource pages expose `page.value`, `next_token` and `is_complete()`. A
remaining token or continuation URI means the selected collection is incomplete.
The URI is inert response metadata: this module never follows it. An explicit
next token is validated and encoded once against the same fixed native route.
Caller-owned traversal must keep the agent/stage/context fixed, detect repeated
tokens, and enforce shared paging bounds. This slice does not claim complete
collection traversal, source schema coverage or snapshot consistency.

Datasource page envelopes must be JSON objects with a native `value` array.
Positional root arrays are rejected even when generic Serde struct decoding
could interpret them as an empty terminal page. Native object-shaped empty
pages remain valid, and datasource fields/opaque continuations retain their
existing contracts.

## Local registration

`register_reads(&mut Registry)` adds six exact bindings atomically: metadata,
definition, staging/published settings and staging/published datasource pages.
The `data-agent` facade restricts owner type and resolves the same operation ID
as a generic get/list. Registration declares only remote-read effects and
conditional caller permission. Configuration adds an explicit public Preview
prerequisite; source inspection is not evidence that a host/tenant supports it.

Registration is opt-in. It does not change the global CLI grammar, default
registry versions, numeric policy or MCP tools. There are no bindings for
`docs`, `src/context`, ontology members, answering, publish, reset or unlink.
The registry schemas describe completed native data, not accepted handles;
definition acceptance uses the existing `DefinitionRead` observer contract.

## Authored, served and execution boundaries

Stored instructions are returned as inert data. No strings are interpreted as
commands or trusted instructions. Datasource metadata keeps its management ID
separate from `itemReference`/`lakehouseReference`; seeing a reference grants
neither access to that item nor permission to execute a query.

Public staging/published settings are not FabricAIHub's authored
`Context`/served `SystemContext`, distilled guidance or hidden runtime prompts.
Native definitions are not generated documents. The separate DataAgent ontology
package owns its public/private ontology projections; these requests do not
navigate, migrate through a private fallback, or perform ontology operations.
Hosted DataAgent MCP answering, tasks, model calls, publish/reset/unlink,
few-shot edits, datasource schema discovery and topic formatting are excluded.

## Source provenance and compatibility

The authorized v2 base is `cc183dd43140350eb1f23af0636332150d1f6d50`.
The explicit-stage CLI adapter is based on approved merged SDK integration
`99ce46c05f7601d3c1556b1e60e62716ae2dd35a` from
`eisber/fabric-cli`, `dev/marcozo/fab-cli-v2-integration-20261104`.
Existing Rust definition/metadata behavior is reused rather than changing the
v1 Python/oracle contract. Public endpoint facts are from:

- [Get Data Agent](https://learn.microsoft.com/en-us/rest/api/fabric/dataagent/items/get-data-agent):
  Item.Read.All or Item.ReadWrite.All, read permission, HTTP 200 metadata.
- [Get Data Agent Definition](https://learn.microsoft.com/en-us/rest/api/fabric/dataagent/items/get-data-agent-definition):
  Item.ReadWrite.All plus read/write rights; sensitivity label excluded;
  encrypted labels require sufficient usage rights; HTTP 200/202.
- [Definition structure](https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/data-agent-definition):
  JSON multipart `Files/Config/...`, draft/published separation.
- [Staging settings](https://learn.microsoft.com/en-us/rest/api/fabric/dataagent/staging/get-settings)
  and [Published settings](https://learn.microsoft.com/en-us/rest/api/fabric/dataagent/published/get-settings):
  public configuration Preview, distinct routes, `aiInstructions`.
- [Staging datasources](https://learn.microsoft.com/en-us/rest/api/fabric/dataagent/staging/list-datasources)
  and [Published datasources](https://learn.microsoft.com/en-us/rest/api/fabric/dataagent/published/list-datasources):
  public configuration Preview, paginated native metadata.

Configuration reads require one of Item.Read.All, Item.ReadWrite.All,
DataAgent.Read.All or DataAgent.ReadWrite.All and agent read rights. These REST
pages document user and service-principal/managed-identity support; actual
authorization stays service-owned. The injected Fabric credential requests
`https://api.fabric.microsoft.com/.default`, with no new scope acquisition or
identity fallback implemented here.

Documentation capture pins are `5df184c567fafea7284357607ebac79d6301a698`
(metadata), `3450951c8dc66a9ffd10b423b8aff5948da0006c` (definition read),
`b4d803a1e7f495553fcb7ef509391016e676d91d` (configuration) and
`4f1cba6a2910d965cc038f9c53475d438a8fc724` (definition structure).
The getDefinition REST example still shows `DataAgentV1.json`; the structure
article lists `Files/Config/...`. This binding preserves the returned parts
instead of inventing a one-file conversion or canonicalizing either example.

## Offline validation

`sdk/client/tests/dataagents.rs` uses only synthetic credentials/responses.
It covers native method/route/format, owner and returned identity, explicit
stage isolation, inert content, partial pages and token encoding, original
permission/service failures, byte boundaries, cancellation, real acceptance
without polling, preserved endpoint binding and generic/facade registry parity.
The shared CLI `get_target`, `get_command`, `get_backend` and headless
`v2_get_binary` targets additionally cover typed paths, explicit selector errors,
canonical/facade parity, identity verification before source reads, distinct
native stage routes, exact full-source preservation, all-part accounting,
binary part bytes, absent stages, acceptance observation without POST replay,
inert datasource continuations, original denial evidence and pre-projection
budgets. The SDK's existing query-absence, object-root datasource,
decode-cancellation, raw/compact-budget and response-evidence guards are retained.
The existing `representations` target exercises the shared decoder/observer
contract; it is not replaced or weakened.

Run from `rust/` under the shared validation mutex on Windows:

```text
cargo +1.99.1 test --offline --locked -p fabric-sdk --test dataagents --test representations -- --test-threads=1
cargo +1.99.1 check --offline --locked -p fabric-sdk --all-targets
cargo +1.99.1 clippy --offline --locked -p fabric-sdk --all-targets -- -D warnings
```

No live Fabric/model/identity acceptance is claimed. Current complete native
three-platform/stable and applicable optional gates are still required before
review-ready integration.

The native CI job has a finite 150-minute Windows budget; Linux and macOS
retain 120 minutes. Run `42465174525` on DataAgent head `4c0dadff` exceeded the
previous two-hour Windows job envelope after 241 successful completed commands
(108 minutes 32 seconds) plus about 11 minutes of preflight. Its final complete-
plan receipt and after-source guard were missing, so that run was not a pass.
The Windows-only headroom accommodates the same full plan and final source
proof. It changes no application deadline, command/profile/test inventory,
parallelism, feature/default graph, locked dependency or oracle/source gate.
The existing workflow guard verifies the three exact finite platform budgets;
current-head CI must still finish every required proof before readiness.

<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Livy JSON SDK: first-wave source surface

This is a native Rust SDK implementation, not a CLI host or a claim of deployed
Fabric/Wildfire qualification. It uses only the coordinator's
`fabric_sdk::runtime::{NativeEndpoint, NativeRequest, NativeMethod, NativeEffect,
NativeError, validate_segment, NativeExecutor}` contracts. No SDK-to-CLI edge,
HTTP client, credential selection, retry engine, runtime, generic LRO or hidden
Spark/session creation is installed.

## Source and integration

The implementation reference is `fabric-livy-sdk`
**`10b9db3a20ca1971f8d8a5fff981ff65260b022d`**, specifically:

- `docs/reference/native-rest.md`: all 49 native method/path pairs.
- `python/src/microsoft/fabric/livy/{client.py,_common.py,models.py,_json.py}`:
  payloads, defaults, handle ownership, output-state checks and byte cursors.
- `contracts/rest/generated/openapi.yaml`: all 20 modeled Fabric operations
  and their JSON schemas/parameters.

The source's empty target commit is not the implementation reference. The
explicit assignment selects the first native SDK wave, not full Notebook/Livy
CLI delivery. Existing Python behavior and Rust command registration are untouched.

The agreed shared runtime interface was cherry-picked first from
`5fe13bd171c3bccf76da519b556173570c786603`, followed by the authorized bounded
execution/media/header enhancement
`2cca2b509c2e0b7c0f7bf0968f9fbe8e427bfb1f`. Livy execution delegates to
`NativeRequest::execute`, and native protocol headers and multipart bytes use
the shared request's validated header/media builders. Integration must export
`pub mod livy;` in `fabric-sdk/src/lib.rs`; this lane deliberately does not edit
that coordinator-owned file. The intended public tests import `fabric_sdk::livy`
and become buildable after that export.

## Service profiles and owners

`LivyService::native(endpoint)` selects an explicit validated native service base.
`LivyService::fabric(endpoint, workspace, lakehouse)` selects:

```text
<base>/v1/workspaces/<workspace>/lakehouses/<lakehouse>/livyApi/versions/2024-04-01
```

`fabric_versioned` accepts explicit source-shaped `YYYY-MM-DD[-preview]` API
versions, including `2023-12-01`; the API does not discover environments or
choose an audience. The caller injects a correctly bound executor. Bases reuse
the shared endpoint HTTPS/loopback validation. IDs additionally follow the
source's ASCII `[A-Za-z0-9._~-]+` grammar and reject empty/dot/escaped/path/
query/fragment/control values. Statement and native job IDs are nonnegative
signed-64-bit wire integers.

All factories are local and immutable:

| API | Meaning |
| --- | --- |
| `service.session(id)` | Classic session owner; never an HC management alias |
| `service.batch(id)` | Batch application owner |
| `service.high_concurrency(acquisition_id)` | Fabric management acquisition owner only |
| `session.repl(repl_id)` | Native classic-session/REPL tuple |
| `hc.repl(backing_session_id, repl_id)` | Fabric acquisition/backing-session/REPL tuple |
| `owner.statements().statement(id)` | Exact classic or REPL statement owner |
| `session.job(id)` | Native serialized-job owner, not a statement/scheduler job |

An HC acquisition ID is retained separately from its backing ID in requests.
The execution route uses `highConcurrencySessions/<backing>/repls/<repl>`,
while release uses `highConcurrencySessions/<acquisition>`. No HC factory
converts its backing session to a classic deletion handle. Fabric HC only models
the four statement operations: native REPL GET/PUT/DELETE/stop/timeout extensions
are rejected, not invented as gateway capabilities. Explicitly supplied backing
IDs are caller evidence, not proof of a server-verified association; no discovery,
first-idle selection or implicit read occurs.

## Complete route surface

`NATIVE_ROUTES`, `FABRIC_ROUTES` and `ServiceProfile::routes()` enumerate exact
methods, path templates, source member/operation IDs, response schemas and request
representations. `request.contract()` identifies the admitted source operation.
The capability check occurs during request building, before the executor runs.

| Native family | Method/path pairs | Rust surface |
| --- | ---: | --- |
| Service version | 1 | `service.version` |
| Session collection | 2 | `list_sessions`, `create_session` |
| Classic session and tooling | 16 | `get`, `delete`, `get_state`, `logs`, `reset_timeout`, `stop`, `interrupt`, `connect`, `get_timeout`, `set_timeout`, `completion`, `inspect`, `debug`, `add_resource` for three kinds |
| Buffered binary uploads | 3 | `upload_descriptor(...).into_multipart_request(...)` for JAR, Python and ordinary files |
| Serialized native jobs | 4 | `submit_job`, `run_job`, `job.get`, `job.cancel` |
| Classic statements | 4 | `list`, `submit`, `statement.get`, `statement.cancel` |
| Native REPL collection/management | 8 | `list_repls`, `repl.create/get/delete/stop/reset_timeout/get_timeout/set_timeout` |
| Native REPL statements | 4 | Same statement API with full REPL owner |
| Batches | 7 | `list_batches`, `create_batch`, `batch.get/cancel/get_state/logs/reset_timeout` |
| **Total** | **49** | **46 JSON/bodyless dispatches; 3 bounded buffered multipart dispatches** |

The 20 Fabric operations are exactly four batch operations, five classic
session operations (list/create/get/delete/reset), four classic statement
operations, three HC acquisition operations and four HC statement operations:
**4 + 5 + 4 + 3 + 4 = 20**.

No Fabric route is inferred from native availability. Native session activity
reset is **PUT**; Fabric classic activity reset is **POST** and returns a session
model. Stop, whole-session interrupt, connect, activity reset, duration update,
DELETE, statement cancel and native job cancel retain distinct effects/routes.
Batch termination is the actual **DELETE** route with `NativeEffect::Cancel`.
Native job cancellation can remove its remote handle; there is no invented
follow-up terminal polling contract.

### Bounded multipart, distinct from JSON and streaming

`UploadDescriptor` describes native POST `upload-jar`, `upload-pyfile` and
`upload-file`, including scoped URL, validated basename, explicit content length,
`application/octet-stream`, and the exact part name (`jar` or `file`). Factories
do not open files or consume streams. `into_multipart_request(content, boundary,
max_request_bytes)` builds the exact multipart framing in the shared request's
explicit byte slot, without dispatching. File length must match the descriptor,
the caller-selected ASCII boundary must be nonempty, safe, at most 70 bytes and
absent from content, and the complete framed body must fit the byte limit before
allocation. Quoted filename characters are encoded as `%22` in the disposition.

`into_json_request()` still returns `NativeError::UnsupportedCapability`:
binary multipart is never disguised as JSON/base64. Dispatch is the same explicit
single `LivyRequest::execute` call as other routes. This is a bounded **buffered**
API, not streamed-file parity; file reading and streaming transport remain
caller/host capabilities. URI loading through `add_resource` is separate and is
not an upload substitute.

## Payloads and parameters

All input bodies are immutable and redact Debug. They expose exact JSON through
an explicit `as_json()` accessor, not automatic logging.

| Input | Exact wire behavior |
| --- | --- |
| `Configuration` | Object preserved unchanged, including unknown/nested deployment fields; defaults to `{}`. Session/batch/REPL creation remains extensible. HC validates known `sessionTag`, `name`, `kind` string and `conf` object fields. |
| `StatementInput` | `{"code": "...", "kind": "sql"}`; explicit kind override; rejects blank values. `from_json` defaults absent kind only and rejects unknown fields/sourceId/parameters rather than promising unsupported statement extensions. |
| `CompletionInput` | `kind=spark`, code and cursor always present; cursor is Unicode-scalar position as in Python, not UTF-8 bytes; explicit optional `sourceId`. |
| `InspectionInput` | `kind=spark`, `vars=[]` by default; optional explicit `sourceId`; variable names are nonempty strings. |
| `DebugInput` | `{"msg_type":"debug_request","content":"<serialized Jupyter object or supplied string>"}`. Native default REPL only, no source ID. JSON whitespace/escaping of an object is serde serialization, not a Python byte-for-byte encoder claim. |
| `TimeoutInput` | Positive millisecond `sessionTimeout` and/or `replTimeout`, at least one; classic setters reject REPL duration. Inherited `-1` is a read value, never a setter. |
| `SerializedJob` | `{"job":"<standard base64>","jobType":"<explicit type>"}` from nonempty opaque bytes. No deserialization or synthesized type. |
| `ResourceUri` | `{"uri":"..."}`; nonblank and control-free, forwarded to server localization without SDK fetching. |
| Statement cancellation | Both native and Fabric send an explicit `{}` for classic and owned REPL statements, matching pinned sync/async `StatementClient.cancel`; acknowledgement is not terminal-state evidence. |

Queries are not generic paging/LRO abstractions:

- Native session/batch lists: optional `from`/`size`, selected with
  `ActivityPage::Native`; Fabric uses `$top`/`$skip`/`$count` with
  `ActivityPage::Fabric`. The default omits parameters. Cross-profile options
  are rejected without dispatch.
- Native statement lists: optional `from`/`size`/`tail`.
  Fabric modeled statement lists only admit the empty default.
- Native REPL list: both `from` and `take` or neither, plus optional literal,
  query-encoded `filter`. Filter content cannot alter route ownership.
- Logs and statement GETs: `BytePage` emits only supplied `from` and `size`.
  Statement cursors refer to **bytes**, never row counts or string lengths.
- Fabric statement submission advertises only `application/json`.
  No arbitrary Accept header, raw URL, unsupported native extension or Arrow
  request parameter is admitted through these APIs.

## Execution and result truthfulness

```rust
use fabric_sdk::{
    livy::{LivyRequest, LivyService, StatementInput, StatementSnapshot},
    runtime::{NativeEndpoint, NativeError},
};

fn prepare(endpoint: NativeEndpoint) -> Result<LivyRequest<StatementSnapshot>, NativeError> {
    let service = LivyService::native(endpoint);
    let session = service.session("0")?; // local handle only
    session.statements().submit(&StatementInput::new("SELECT 42")?)
}
```

The caller explicitly invokes `request.execute(existing_executor, byte_limit)`
inside its own async runtime. Every invocation dispatches once. There is no
internal retry, submit replay, polling, timeout reset, cleanup, remote
cancellation or generic `Location`/operation-LRO assumption.

The host remains responsible for authorization bound to the selected endpoint/
profile/audience/tenant, redirects, deadlines, cancellation, streamed byte limits,
and host-controlled security/transport headers. All Livy requests explicitly
declare `Accept-Encoding: identity`; native requests additionally declare
`X-Requested-By: fabric-sdk-rust-livy`. Fabric never inherits that native marker.
These use the shared validated header builder, without injecting authorization,
Host or Content-Length. Hosts must honor `NativeRequest::bytes()` and
`content_type()` for multipart rather than serializing binary content as JSON.
`NativeResponse` does not expose correlation/retry headers; this SDK does not
manufacture those values.

`LivyResponse<T>` preserves actual HTTP status, original content type, exact
bounded bytes, original request, source-only maturity and an immutable parsed
model. Empty HTTP 202 is `Accepted` with no fabricated model; a returned model
does not override that acceptance classification. Bodyless/text acknowledgements
are allowed only on source acknowledgement routes and retain their original
bytes. Other responses require a JSON object and validate known non-null model field
types. Optional null fields are preserved (for example a starting session's
`appId` or a pending statement's `output`), without fabricating a value.
Unknown nested JSON and opaque serialized results remain unchanged.

`LivyError` separates input/executor/HTTP/decode/bound failures, preserves actual
HTTP status and bounded error bytes, and never prints provider/code content.
An executor/shared-runtime `Limit` exposes no fabricated status or truncated body:
that response was rejected by the common bound before Livy decoding. The error
still identifies a bound failure and conservatively preserves mutation ambiguity.
Runtime HTTP 404 remains actual HTTP 404, not proof that the entire selected
source profile is unavailable. A failed dispatched non-GET is conservatively
`outcome_unknown`, including decode/output failure: reconcile the retained owner,
do not replay. Local cancellation/deadline is not remote cancellation.

Workload errors are different: `SessionSnapshot`, `BatchSnapshot`,
`StatementSnapshot` and `JobSnapshot` expose model-specific `observe()` results
or redacted `WorkloadFailure` with immutable raw evidence. Session idle means
`Ready`, not a successful application. Statement `available` plus
`output.status=error` is failure; absent/unrecognized output status is `Unknown`,
not success. Job application errors remain failures even with a success state.
Cancellation/reset/interrupt acknowledgement never proves terminal cancellation.
List/log/tooling malformed data is explicit error, never a filtered empty success.

## JSON byte cursors

`StatementPages` is a finite pure observer holding the full statement owner. It
builds the next exact GET, accepts only the corresponding response, and returns
either the exact provider next byte offset or retained-output completion:

- Initial size, page budget and cumulative raw-response-byte budget are positive.
- Only one built request can await observation; page and byte bounds are enforced.
- Endpoint binding, acquisition identity, complete route and exact query must match.
- Only a successful HTTP 200 completed statement page with output `status=ok`
  can advance. Missing output/state/cursor is not an unpaged-success fallback.
- Top-level `next` takes precedence over nested `output.data.next`, matching source.
  Optional `from` must equal the requested offset.
- `next=-1` ends traversal; `next=-2` is explicit provider retrieval truncation.
  Negative/boolean/fractional/overflow/nonadvancing cursors are errors.
- Cursors are opaque: fragment byte/character/row lengths never calculate `next`.
  Fragments/schema/MIME/errors are not merged, materialized or deserialized.
- Completion means only traversal of retained output, not completeness of original
  query rows. Errors leave the caller's partial response evidence intact.

The caller owns one absolute observation deadline, cancellation, dispatch byte
budget (bounded by `remaining_bytes()`), output retention and any explicit
reconciliation after a transport failure. There is no durable cross-process
resume/checkpoint, Arrow framing/codec, dataframe conversion or implicit fallback.

## Validation and remaining host gates

`tests/livy_native.rs` contains offline synthetic-executor tests for the full
49/20 route matrix, native/Fabric payload and query defaults, HC acquisition/
backing ownership isolation, native-only rejection on Fabric, bad IDs/bodies,
single-dispatch error paths, immutable raw/error payloads, state/output failures,
nonterminal cancellation acknowledgements, exact native protocol headers,
multipart binary/framing/total-wire bounds and bounded byte-cursor invariants.
They are intended final integration tests, not temporary private-module tests.

**Actually performed in this lane:** pure Python comparison of the public
method/path registries against the pinned native markdown and Fabric OpenAPI;
49 native and 20 Fabric identities match with no duplicates. Source/header/
stub and diff-whitespace checks were also performed. These are static evidence,
not executed Rust tests.

**Not run by this lane:** rustfmt, Cargo check/test/clippy, architecture or
downstream/native host validation. The coordinator reports restored session-local
Rust tools and focused tests queued under the shared heavy-validation mutex;
queued work is not a passed result. Rust tools are still absent from this lane's
PATH, and workers are not authorized to launch compiles. This lane installed no
toolchain/dependency. After the coordinator exports the module and obtains the
build slot, run the focused
`fabric-sdk` Livy tests/all-target checks, pinned formatting/lints/architecture
checks and affected-consumer checks from `rust/`. Compile/registration/type
visibility and independent review remain integration gates.

No live service/auth/settings/credential discovery, pushes, PRs or merges were
performed. Missing host integration, streamed upload transport and deployed
qualification remain explicit gaps; this is not whole-feature completion.

## Existing statement CLI consumer

The opt-in `fab-cli-v2` statement reader consumes this SDK through the shared
`RuntimeHead` and native host, with an explicit GUID Lakehouse owner and the
existing Fabric-bound executor. It uses
`session(id).statements().statement(id).get(BytePage::default()).execute(...)`
for both the statement path and `/result` alias. It preserves the full JSON
envelope and validates the returned individual statement ID; neither spelling
submits a statement, traverses byte pages, extracts tabular output or claims
execution success from read completion. Native `.LivyService` authentication
does not fall back to Fabric. See the [runtime read executable
contract](runtime-v2-reads.md#opt-in-existing-statement-executable) for selection,
deadline, output and remaining qualification limits.

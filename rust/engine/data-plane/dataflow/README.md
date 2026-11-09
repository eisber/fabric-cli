<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Dataflow Execute Query

This SDK-layer client implements the official
[Dataflow Execute Query API](https://learn.microsoft.com/rest/api/fabric/dataflow/query-execution/execute-query)
over the existing caller-injected `OperationSession`. It does not create an
HTTP/authentication implementation, runtime, scheduler, CLI process, identity
fallback or ambient workspace resolver.

The owner-approved CLI v2 binding is **root `execute`**, not `run`. Background
Dataflow execute/applyChanges remains a separate `run` operation. This crate does
not change the Python CLI or claim live service acceptance.

## Public interface

```rust,ignore
use fabric_sdk_dataflow::{
    ArrowVersion, ExecuteQueryCompletion, ExecuteQueryRequest, ExecuteQueryTarget,
    execute_query, wait_for_query,
};

let target = ExecuteQueryTarget::new(workspace_uuid, dataflow_uuid)?;
let request = ExecuteQueryRequest::new("NamedQuery", ArrowVersion::V1)?
    .with_custom_mashup_document("section Section1; shared NamedQuery = ...;")?;

let response = execute_query(
    &mut caller_session, &target, &request, ExecuteQueryCompletion::ReturnAccepted,
).await?;

// An actual 202 is accepted, not completed data. Never resubmit the POST.
let response = if response.status == 202 {
    wait_for_query(&mut fresh_caller_session, response, request.arrow_version()).await?
} else {
    response
};
```

`WaitForResult` can instead be selected on the initial call to use the same
bounded session through state polling and result retrieval. Reconciliation of
an earlier acceptance requires a fresh session; the original request still
counts against its request budget. A used session is never silently reset.

The exact wire operation is:

```http
POST /v1/workspaces/{workspaceId}/dataflows/{dataflowId}/executeQuery
Accept: application/vnd.apache.arrow.stream;pq-arrow-version=1
Content-Type: application/json

{"queryName":"NamedQuery","customMashupDocument":"optional M document"}
```

The optional document is omitted when absent. Arrow version is never serialized
as an extra body property. `ArrowVersion::V2` explicitly requests version `2`;
there is no content-language guessing or retry with version `1`.

## Bounds and result policy

- Both IDs must be explicit dashed hexadecimal UUIDs. Query names must be
  nonblank, contain no control characters, and occupy at most 4,096 UTF-8 bytes.
- Optional custom documents must be nonblank and contain no NUL. The entire
  serialized JSON request is limited to 1 MiB, including JSON escaping.
  These are local safety limits, not advertised service limits or M validation.
- The injected session controls finite response-byte, request and poll-delay
  budgets. Its executor owns the total deadline. No result is silently clipped.
- Only an actual `200` with Arrow streaming Content-Type is a completed byte
  result. Missing, duplicate, non-Arrow or malformed media headers fail
  explicitly. A returned `pq-arrow-version`, including a quoted value, must
  match negotiation. If that parameter is omitted, the documented request
  negotiation determines the version; bytes are not guessed or transcoded.
- `202` preserves native acceptance and validated generic Fabric operation
  evidence. Missing, malformed, foreign-origin and inconsistent operation
  handles fail rather than producing a scheduler/cancellation handle.
- Arrow Accept negotiation is forwarded on the initial POST and result GET.
  State polling retains the existing JSON defaults. Existing shared operation
  constructors and unrelated consumers keep their original defaults.
- Returned bytes are exact, including empty/binary data. This crate does not
  decode Arrow, infer schema, interpret multiple result sets or serialize output.
  Consumers **must decode the stream and inspect `PQ Arrow Metadata` for query
  or streaming failure evidence before reporting success**. A valid HTTP/media
  response alone is not proof of tabular success.

`ExecuteQueryError` exposes a sanitized kind and explicit accessors for the
original shared operation failure or rejected raw response. Service failures,
terminal operation errors, operation IDs and uncertain dispatch are preserved.
Its Display/Debug, request Debug and target Debug redact source-sensitive data.
The raw shared response/evidence accessors remain sensitive; do not log them
indiscriminately.

## Service limitations and gaps

- A query can execute for **at most 90 seconds**. This service duration is
  distinct from the caller's total deadline, transport time and polling time.
- Version 2 is not supported when a Dataflow connects through an on-premises
  data gateway. Version 1 supports those Dataflows. No gateway capability is
  discovered or inferred here; the caller must select an appropriate version.
- The endpoint supports users, service principals and managed identities, but
  still requires execute permission on the Dataflow and the documented
  `Dataflow.Execute.All` or `Item.Execute.All` delegated scope as applicable.
  A caller-injected credential does not establish those permissions.
- Local timeout, cancellation or dropping a future never proves remote
  cancellation or rollback and never authorizes replay. No remote cancel
  endpoint or fabricated background-job handle is provided.
- Full detailed refresh-log ZIP download is a documented separate gap. Execute
  Query is neither a refresh nor a saved-data/log retrieval shortcut.

## Provenance and offline coverage

This is an additive owner-approved native API, not a Python behavior-parity
claim. Repository baseline: `5e1e368e6d977f50c082e3b2f9442d48bd0b2e5b`.
API contract checked against Microsoft Learn on 2026-11-05, source revision
`b4d803a1e7f495553fcb7ef509391016e676d91d`.

`tests/execute_query.rs` uses synthetic credentials, scripted responses and
injected asynchronous time/cancellation only. It covers exact method/route/body,
both Accept versions, pre-credential input rejection, immediate bytes,
accepted/wait/result behavior, service/operation failures, unsafe handles,
media policy, bounds, uncertainty, selected endpoints and debug redaction.
The HTTP adapter also tests actual injected HTTP headers for opt-in and
unchanged-default requests. No service/authentication discovery is exercised.

The coordinator owns workspace dependency registration, combined compiler,
architecture and downstream-host validation, and independent review. Test
source is not a claim that those gates have executed.

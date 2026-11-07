<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# SQLDatabase metadata SDK leaf

This additive `sdk`-layer crate prepares **metadata GET and single-page LIST**
on the native Fabric SQLDatabase routes. It does not replace Python/v1, register
CLI commands or implement the remaining SQLDatabase management operations.

Implementation contract/base: coordinator-accepted metadata-only first wave at
`eca98ab84c4a14d1ee158c2438e76dd77d2b0af0`. No upstream poller, ARM-origin or
authentication changes are adopted.

## Public boundary

- Parse native GUIDs into distinct `WorkspaceId`, `SqlDatabaseId` and `FolderId`
  types. Only 8-4-4-4-12 ASCII hex/hyphen syntax is accepted. Uppercase is accepted
  and rendered lowercase; braces, whitespace, compact/encoded IDs and route
  fragments are rejected. No UUID-version or nonzero restriction is invented.
- Construct `SqlDatabaseTarget::new(workspace, item, "SQLDatabase")` from explicit
  identity metadata. Other item types fail before request construction. This is
  a caller-supplied type check, not proof of remote ownership or authorization.
- `SqlDatabaseRequests::get` prepares
  `GET /v1/workspaces/{workspaceId}/sqlDatabases/{sqlDatabaseId}`.
- `SqlDatabaseRequests::list_page` prepares
  `GET /v1/workspaces/{workspaceId}/sqlDatabases`. Callers explicitly choose
  `recursive`, `FolderScope` and presence/absence of a decoded continuation token.
  Tokens are encoded once by the existing resource query encoder.
- A `SqlRequest<T>` exposes its existing `ResourceOperation`, read-only
  permission/effect facts, a typed decoder and one-call `execute` delegation to
  injected `ResourceOperations`. The caller composes the existing
  `OperationSession` with its injected credential, context, executor,
  cancellation signal and explicit limits.

There are no default recursion/engine/path/output choices, arbitrary URLs,
headers/query parameters, writable metadata maps, new HTTP/paging/polling
engines, identity implementation or runtime. No definition/codec dependency,
mutations, restores, audit, CMK, mirroring or native SQL stubs are installed.
The overall inventory of 13 REST operations / 15 facade dispositions is not
implemented coverage from this first wave.

## Results, permissions and lifecycle

Only documented **HTTP 200** with the correct typed body succeeds. There is no
hidden wait: descriptors use the existing `ReturnAccepted` non-polling policy,
but an unexpected 202 remains an error/incomplete result, never successful
metadata. Shared credential/service/cancellation/execution errors are retained
unchanged in `SqlManagementError::Operation`; no principal/route/SQL fallback or
metadata enrichment occurs.

An injected `ResourceOperations` acceptance handle is retained in
`SqlResponseError`. At this pinned baseline the shared operation engine itself
rejects non-Python GET 202 as a status error and may not expose its original
Location/handle through `ResourceError`. This crate preserves exactly the
available evidence; it does not fabricate a handle, add an HTTP adapter to recover
it, poll, replay or alter shared infrastructure. Full raw acceptance-receipt
retention remains a coordinator-owned shared-interface concern if required.

GET requires item Read and one of SQLDatabase.Read.All,
SQLDatabase.ReadWrite.All, Item.Read.All or Item.ReadWrite.All. LIST requires a
Viewer-or-higher workspace role and Workspace.Read.All or Workspace.ReadWrite.All.
These documented delegated alternatives are metadata facts, not permission
checks or automatically acquired token scopes. Actual credential composition
uses the caller's existing Fabric audience; no SQL authentication is installed.

The read-only response projection requires native `id`, `workspaceId`,
`displayName` and exact `type: "SQLDatabase"`. GET checks both selected IDs; LIST
checks every row's workspace before returning the page. Order and duplicate rows
are preserved. Missing/null optional known fields remain distinct via `Observed`.
Properties never synthesize an empty/default catalog or fall back to displayName.
Connection strings and hosts are inert metadata, not trusted connection targets.
Restore timestamps remain strings, not validated restore inputs; native int32
backup-retention metadata is not subject to a newly invented create range/default.

Unknown response fields are ignored for **forward-compatible projection**, not
retained for a raw metadata round trip. This minimal model does not expose
principal details, tags or sensitivity-label contents. Known projected field
types are checked: pages, rows and properties must be JSON objects, `value` must
be an array of object rows, and `type` must be the exact JSON string
`"SQLDatabase"`. Positional arrays and externally tagged enum objects are not
accepted as equivalent shapes. Missing/null optional properties remain distinct.
No projected field can become a mutation input in this crate.
Debug/Display redact service metadata, cursors, supplied IDs and raw errors;
explicit field/error inspection remains available to the authorized caller.

## Pagination boundary

LIST returns a single `SqlDatabasePage` only. It retains missing/null/empty
continuation fields without fetching the next batch or following a returned URI.
`PageCoverage::Incomplete` means a nonempty token or URI remains unconsumed.
`TerminalPage` describes the service response, not transactional enumeration,
snapshot consistency or CLI command parity.

No full-enumeration adapter is added: the shared `TokenPager` fetch interface
uses raw bounded responses, while this leaf delegates through the existing
`ResourceOperations` typed outcome boundary. A separately reviewed caller adapter
can validate typed pages before shared-pager commit; duplicating a second
traversal/HTTP engine or hiding partial progress is not justified for this wave.

## Offline test coverage and unrun validation

Public-boundary test sources cover strict IDs/type selection, exact native routes,
explicit folder/recursion/token behavior, one-time encoding, response identity
and known types, missing/null properties, projection, pagination coverage and
redaction. Shared-engine tests use synthetic credentials and an injected
`OperationExecutor` through actual `OperationSession`/`ResourceOperations`:
single dispatch, Fabric scope, malformed statuses/JSON, service errors,
cancellation, wrong audience, provider failures and no hidden wait/fallback.
No live service, real credential discovery, filesystem output or SQL is involved.

**Validation has not run.** The coordinator owns toolchain/build allocation,
workspace manifest/lock/default-member/facade integration and independent review.
The crate's local manifest inherits existing workspace dependencies/lints only;
no central file is changed. Future authorized validation from `rust` must include:

1. `cargo fmt --manifest-path capabilities\resources\fabric-sdk-sql-database\Cargo.toml -- --check`.
2. `cargo test -p fabric-sdk-sql-database --all-targets` and
   `cargo test -p fabric-sdk-sql-database --doc`.
3. `cargo check -p fabric-sdk-sql-database --all-targets` and
   `cargo clippy -p fabric-sdk-sql-database --all-targets -- -D warnings`.
4. Architecture/affected-consumer checks selected by the existing impact
   validator, then exact combined-candidate validation.

These are unexecuted requirements, not passing-check evidence or a commit/merge
claim.

## Source references

- [Get SQL Database](https://learn.microsoft.com/en-us/rest/api/fabric/sqldatabase/items/get-sql-database)
- [List SQL Databases](https://learn.microsoft.com/en-us/rest/api/fabric/sqldatabase/items/list-sql-databases)

Both operation pages' documented source pin is
`MicrosoftDocs/fabric-rest-apis-docs@5df184c567fafea7284357607ebac79d6301a698`,
as recorded in the accepted preparation packet. The typed IDs/identity checks,
sanitized diagnostics and bounded single-page semantics are explicit SDK safety
behavior, not a change to the Python compatibility oracle.

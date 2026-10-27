<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# TABLE SDK

`fabric-sdk-tables` is a focused SDK-layer crate. It depends only on SDK/core and
external serialization/interoperability types. It never selects credentials,
creates a runtime/HTTP client, reads configuration, writes temporary files or
launches Python, `az` or `azd`.

`TableClient::new` requires an injected Azure `TokenCredential` and explicit
Fabric/OneLake selections for the same account and tenant. Selection metadata is
not verification of token claims. Each schema/properties request acquires the
storage scope; submission acquires the Fabric scope. The original provider is
available to a shared operation adapter for fresh tokens during reconciliation.
Debug and Display omit tokens, request content and provider diagnostics.

## Public behavior

`TablePath` holds resolved workspace/item IDs and an item-relative path.
`TableClient::schema` requests the Delta-log listing, sorts JSON names in reverse
lexicographic order, and reads until the first `metaData.schemaString` action.
The generic OneLake adapter must supply the complete bounded listing, including
continuation pages. Checkpoints are not inferred, malformed newer JSON is not
skipped, and missing metadata is `InvalidDeltaTable`, not an empty schema.

`TableWithOneLake` explicitly selects the released `OneLakeClient` through its
generic `OneLakeExecutor`. It composes with an existing table-load executor,
preserving earlier raw-read implementers. The same injected provider and storage
selection are used directly, without duplicate preflight tokens. Complete
listing pages count against TABLE's request budget before any log is read.
Source file properties likewise use generic HEAD. Generic OneLake path,
response-shape and success-status safety rules remain explicit: the generic
SDK rejects missing `paths` and unexpected2xx statuses instead of emulating
every Python raw-response quirk.

`source_kind` preserves the source HEAD-before-format-validation order.
`load_with_kind` submits without a duplicate HEAD; the convenience `load`
composes both. The actual endpoint is
`/v1/workspaces/{workspace}/lakehouses/{item}/tables/{final-name}/load`.
Python does not put a schema in that endpoint. A folder sets `recursive: true`;
CSV defaults omit header and delimiter despite the Python help text.
Cross-item sources retain only their local path in the load payload, as in
Python; this does not imply that the service can load a cross-item source.
Load request bytes use the released `fabric-core-json::json_dumps` encoder,
matching the original handler's default `json.dumps`: ordered keys, comma-space
and colon-space separators, ASCII Unicode escaping and no trailing newline.
The literal-byte oracle exercises ordinary CSV, folder append, Unicode paths
and delimiters, supplementary characters, quotes and controls through actual
HEAD/submission/shared operation result retrieval, not parsed-value equality.

`LoadOutcome` distinguishes proven completion, accepted-only work and source
204/no-output behavior. `TableExecutor::load` delegates generic Fabric
reconciliation. A header-less 202 is not completion. Header-bearing 202 follows
the source's conditional operation lifecycle. No implementation here polls jobs
or invents a second HTTP/retry/LRO stack.

`Maintenance::execution_data` produces vacuum/optimize settings plus table and
optional schema names. The generic jobs adapter owns `TableMaintenance`
submission, status, timeout and cancellation. Cancellation is not rollback.
`TableError` retains uncertainty and an observed operation/job ID for
reconciliation rather than inviting a mutation replay.

`JobMaintenance` now implements `MaintenanceExecutor` over the released
`fabric-sdk-jobs::JobClient`. It constructs a `TableMaintenance` target, submits
the exact execution data, and delegates waits/status/cancellation to the generic
job engine. `WaitOptions`, `cancel_on_timeout` and the optional shared
`JobOperationReconciler` are explicit caller inputs. Local cancellation/drop does
not issue a remote cancel. A source timeout only issues cancel when configured;
accepted cancellation is not terminal completion. Missing generic cancellation
LRO support fails explicitly while retaining the accepted run ID.
The result-only `run`/`run_observed` conveniences default to TableMaintenance.
`run_observed_for_job` instead accepts the source-selected generic job type,
including SparkJobDefinition's `sparkjob`, because Python's nested job handler
overwrites the initially assigned TableMaintenance flag. The CLI validates that
owning-item job boundary at the source stage, not as an earlier TABLE-only
resource allowlist.

`run_observed` exposes typed creation, complete service status rows and timeout
events. Observer failures stop work and retain mutation/run evidence. Failed
jobs retain the service row and `JobFailed`; Cancelled/Deduped/unknown terminal
states do not become a successful-completion message. A no-cancel timeout is a
typed no-output outcome, not an error claiming that the remote job stopped.

The SDK depends on SDK/core only, including the generic jobs SDK; it takes no CLI
adapter dependency. The public offline example exercises real shared HTTP with
synthetic replies and no live identity:

```text
cargo run --offline --locked -p fabric-sdk-tables --example offline_maintenance
```

## Bounds and integration state

Default bounds are 100 table read/preflight calls, 100,000 accumulated log rows
and 4 MiB per decoded response. The storage pager and shared operation/job
engines must additionally enforce their own page/request/deadline bounds.
Dropping futures releases local work, not already accepted service work.
For the selected generic OneLake path, the accumulated listing serialization is
also bounded by the configured response byte limit; a large aggregate is a
named safety failure, not an empty/partial schema. `TableErrorKind::OneLake` and
`SharedOperation` retain original generic classifications and raw operation
service details without depending on CLI types. The lossless shared error type
is `PartialEq`; TABLE errors no longer claim `Eq` for arbitrary service JSON.
`TableRequest::max_requests` carries the remaining lifecycle budget into the
shared LRO session, so a source HEAD plus an external POST cannot reset the
command's poll allowance.
`schema_string` is the lossless inner-schema boundary. The current direct SDK
DOM conveniences use the shared native serde_json numeric domain and do not
claim arbitrary Python-width integer fidelity. Consumers needing a stricter
decoder can pass its result to `schema_fields_value`; the CLI already uses its
existing strict finite-integer decoder. A shared SDK/core strict-decoder handoff
is tracked rather than adding a forbidden SDK-to-CLI dependency or duplicating
the generic JSON scanner here.

See the [family matrix](../../docs/table-compatibility.md) for exact source pins,
executed evidence and unresolved integration dependencies. Injected interfaces
alone do not establish full offline or live command parity.

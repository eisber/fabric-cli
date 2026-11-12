<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Additive native link contract

The additive `fabric_cli_mutations::v2_link` module composes native relationship
requests with caller-owned resolution, input, approval, and execution. It does
not replace the Python CLI, Rust `ln`/`mklink`, or their compatibility fixtures.
The experimental `fabric-cli` app now mounts only the Lakehouse root/facade
in the explicitly selected `fab-cli-v2` head with `headless,v2-link-lakehouse`.
Other owners retain their library-only composition in this slice. Central
discovery registry integration remains separate; legacy `fab-rs ln` is unchanged.

## Bounded Lakehouse executable host

Build from `rust/` with
`cargo build --locked -p fabric-cli --bin fab-cli-v2 --features headless,v2-link-lakehouse`.
The additional feature alone does not install native identity or transport.
Neither default, broker-only nor headless-without-link builds mount this leaf.

```text
fab-cli-v2 link /<workspace-guid>.Workspace/<item-guid>.Lakehouse/Files/landing/sales.Shortcut --input shortcut.json
fab-cli-v2 artifact lakehouse link /<workspace-guid>.Workspace/<item-guid>.Lakehouse/Tables/sales.Shortcut --input - --wait
```

GUID-qualified typed selectors deliberately avoid name discovery, implicit
context and ambiguity. The final exact `.Shortcut` marker addresses a
relationship, not file contents; it is removed only from the final name.
Nested Files parents are preserved; relationship-marked parents are rejected
rather than traversed. Tables placement and provider-specific
target validation remain owned by the existing SDK validator; schema locations
are not enabled by this host. A Warehouse source ID remains valid, but a
Warehouse destination fails even if the caller supplied a `.Lakehouse` suffix.
After approval, one bounded item metadata GET must establish the captured
Lakehouse ID/type before the sole shortcut POST.

Input is a regular non-symlink file or explicit stdin, at most 1 MiB, strict
UTF-8 and duplicate-free JSON. Local validation, including body/destination
conflicts, precedes credential selection and all requests. The local host
assumes a trusted parent filesystem namespace, not race-proof ancestor pinning.
`FAB_RUST_AUTH_MODE` must select the existing headless provider explicitly;
the existing `FAB_RUST_FABRIC_HOST` endpoint binding is reused with no fallback.
No live authentication or Fabric mutation is part of offline validation.

The native host asks for explicit yes/no confirmation on the controlling
terminal, independently of JSON stdin. Empty input declines. It displays the
captured destination, remote-write effect and explicit sensitivity-update
effect together; no flag or connection ID grants approval. Missing/failed/
declined approval makes no request. Approval and remote execution share a
five-minute deadline, bounded responses/request count and the existing shared
operation session. Expiry or Ctrl+C is local interruption, not remote
cancellation or rollback. The process never resubmits a write.

Successful stdout is one newline-terminated metadata receipt, distinguishing
`accepted` from `succeeded`, with the exact relationship destination but no
native source body. The caller-injected host retains full `LinkReceipt`/
`LinkFailure`; a delivery error retains its original receipt. Native stderr
projects status, operation ID and mutation uncertainty for reconciliation, not
automatic retries. A partial stdout/flush failure is an error, never a new POST.
This is an experimental host, not live tenant, native-matrix or release proof.

## Operation and command surface

The common operation identity is `fabric.link`. Artifact facades apply an owner
constraint to the same parser, validator, native request, effects, and execution:

```text
fab-cli link <path> --input <file-or-> [--wait]
fab-cli artifact lakehouse link <path> --input <file-or-> [--wait]
fab-cli artifact kql-database link <path> --input <file-or-> [--wait]
fab-cli artifact data-agent link <path> --input <file-or-> [--wait]
fab-cli artifact semantic-model link <path> --input <file-or-> [--wait]
```

The Ontology facade is discoverable but returns `UnsupportedCapability` before
input access or mutation. It does not replace a whole definition as a shortcut
for an unavailable representation-specific binding operation.

There is no implicit current directory, first-match resolution, generic JSON
passthrough, inline credential argument, `--force`, or overwrite option. The
input is one strict UTF-8 JSON document. Unknown native fields and duplicate JSON
object keys are rejected, including duplicates in nested objects. The host
supplies and enforces a positive input byte limit during reading; the adapter
also rejects an oversized result from an incorrectly implemented file host.
`-` selects stdin only through the explicit input adapter.

## Native request boundaries

| Resolved relationship | Native method and relative API path | Supported input |
| --- | --- | --- |
| Lakehouse shortcut | `POST /v1/workspaces/{ws}/items/{item}/shortcuts?shortcutConflictPolicy=Abort` | `{path,name,target}` at a supported Files or Tables location |
| KQL table shortcut | `POST /v1/workspaces/{ws}/kqlDatabases/{db}/shortcuts` | `{name,enableQueryAcceleration,target}`; no destination `path` |
| DataAgent staging datasource | `POST /v1/workspaces/{ws}/dataAgents/{agent}/staging/datasources` | `LakehouseTables` with `lakehouseReference`, or `FabricItem` with `itemReference`; explicit `ById` |
| SemanticModel connection | `POST /v1/workspaces/{ws}/semanticModels/{model}/bindConnection` | `{connectionBinding:{id,connectivityType,connectionDetails:{type,path}}}` |

The SDK implementations live in `fabric_sdk_onelake::link` and
`fabric_sdk_items::links`. They construct typed, fixed-service operations over
the existing `ResourceOperations` abstraction. They do not acquire credentials,
resolve storage endpoints, create transports, install runtimes, or implement
another polling/retry engine.

Core shortcut target kinds are OneLake, ADLS Gen2, Amazon S3, Google Cloud
Storage, S3-compatible storage, Azure Blob Storage, Dataverse, and
OneDrive/SharePoint. KQL admits only the first six. External data-share response
models do not establish direct creation support. Native field validation and
source-specific location rules are independent of HTTP path encoding.

Lakehouse Files supports safe nested parent paths. Schema-aware Tables
placement is deliberately limited to the explicit rules in the SDK validator;
an unsupported schema path fails rather than falling back to Files or changing
the requested owner. Warehouse can be a OneLake source but is not a shortcut
destination. A resolver must distinguish a relationship reference from a file
or directory accessed through that reference.

DataAgent linking affects staging only. It does not publish an agent, select
tables, execute a query, or create the referenced source item. `ByVariable`
references, response-only `fabricItemType`, and metadata fields absent from the
create schema are not inferred as create inputs.

SemanticModel linking requires an explicit existing connection reference and
datasource-identifying details. A missing connectivity type is not defaulted.
It does not update credentials, take ownership, refresh the model, create table
relationships, bind a gateway through the legacy Power BI endpoint, or rebind a
report. The service still enforces model ownership and connection-use permission.

Ontology JSON/TMDL transforms, unguarded full-definition replacement, legacy
Power BI gateway bindings, shortcut bulk operations, rename, and
passthrough/delegated authentication migration are unsupported by this slice.
They need their own native representation, permission, and partial-effect
contracts, not a generic fallback.

## Caller composition

1. Parse arguments with `v2_link::parse_args`, or mount the shared
   `link_command` leaf in a reviewed v2 host.
2. Resolve the target once through `LinkResolver` to a `ResolvedLinkTarget`.
   The captured workspace, item, owner and typed relationship child remain
   stable during execution and observation.
3. Use `prepare_command` with `LinkFiles` and an explicit byte limit, or call
   `prepare` with an already parsed value. Native SDK consumers can use the
   request builders directly.
4. Present `PreparedLink::effects()` and its captured destination to
   `LinkApproval`. Missing, declined or failed approval prevents submission.
   The presence of a connection ID is not a permission grant.
5. Execute once with the caller's existing `ResourceOperations`. The caller
   supplies credential selection, validated Fabric endpoint, deadlines,
   cancellation, request/response bounds and reconciliation policy.
6. Preserve `LinkReceipt` or `LinkFailure` if presentation or file output fails.
   Do not submit the mutation again to reproduce an output receipt.

Effects include the remote reference mutation, KQL query acceleration when
explicitly enabled, and a requested OneDrive/SharePoint sensitivity update.
Acceleration may incur cache, indexing, storage and compute costs. These effects
must remain visible to host approval. Linking is not copying data or granting
ACLs.

## Outcome and failure evidence

Without `--wait`, the operation uses `ReturnAccepted`; a completed response is
still completed, but an accepted response is never promoted to success.
With `--wait`, resource-producing operations use the existing `WaitForResult`
policy and model connection binding uses `WaitForCompletion`. Core single-create
resource `Location` headers do not establish an LRO by themselves.

The original `ResourceOutcome` is retained. A model bind can complete with
HTTP 200 and no response body. Resource-producing creates require usable native
identity metadata. A malformed completed response returns an explicit
`InvalidResponse`, preserves the original observation, and records that a
mutation may have occurred. Errors retain the native status, operation ID,
service details, and mutation uncertainty through `MutationError::operation`.

`execute` returns a boxed `LinkFailure` on error so the result remains small
without dropping target, effect or original-response reconciliation evidence.

The small `LinkReceipt::metadata()` projection does not echo native request
contents, connection details, or returned source data. The original observation
remains available to authorized programmatic consumers.

Local cancellation and timeout are not remote cancellation, rollback, or proof
that a write did not happen. The wrapper never replays mutations or changes
identity after an error. This slice does not promise atomic cross-resource
changes or exactly-once execution.

## Offline validation scope

The additive tests cover root/facade equality, owner/child and body/path
conflicts, exact native routes, strict input, target restrictions, explicit
approval and cost/sensitivity effects, default acceptance, bounded waiting,
empty model-bind results, malformed responses, and retained uncertain-error
evidence. SDK tests validate native request schemas independently of the CLI.

Mocks do not prove actual tenant permissions, deployed feature availability,
source access, native authentication, or platform support. Production-host and
combined integration validation must be recorded separately before advertising
an executable command as implemented.

## Official API references

- [Create a OneLake shortcut](https://learn.microsoft.com/en-us/rest/api/fabric/core/onelake-shortcuts/create-shortcut)
- [Create a KQL table shortcut](https://learn.microsoft.com/en-us/rest/api/fabric/kqldatabase/table-shortcuts/create-shortcut)
- [Create a DataAgent staging datasource](https://learn.microsoft.com/en-us/rest/api/fabric/dataagent/staging/create-datasource)
- [Bind a semantic-model connection](https://learn.microsoft.com/en-us/rest/api/fabric/semanticmodel/items/bind-semantic-model-connection)
- [OneLake shortcut security](https://learn.microsoft.com/en-us/fabric/onelake/onelake-shortcut-security)
- [Long-running operations](https://learn.microsoft.com/en-us/rest/api/fabric/articles/long-running-operation)

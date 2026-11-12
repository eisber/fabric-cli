<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Empty SparkJobDefinition create

The optional `fabric-cli/v2-create-sparkjobdefinition` feature selects one leaf
over the shared `v2-create` host and SDK policy. With `headless`, the actual
`fab-cli-v2` binary mounts:

```text
fab-cli-v2 create /<workspaceGUID>.Workspace/<literal-name>.SparkJobDefinition [OPTIONS]
```

Default `fab-rs` behavior is unchanged. This creates an **empty item**, not a
configured or runnable Spark job. It never submits a job, starts compute or a
session, configures libraries, uploads a script, transfers OneLake data, or
discovers a default lakehouse. No live identity or Fabric acceptance is claimed.

## Input and provider contract

Input may be omitted. An explicit `--input FILE|-` must be a bounded (4 MiB),
unique-key UTF-8 JSON object. The only admitted fields are:

| Field | Contract |
| --- | --- |
| `displayName` | Optional; if present, must equal the literal target name. |
| `type` | Optional; if present, must be exactly `SparkJobDefinition`. |
| `description` | Optional string of at most 256 Unicode characters. |
| `folderId` | Optional JSON null; this leaf does not select a folder. |

Empty bytes, JSON null, duplicate or escaped-duplicate keys, unknown fields,
`definition`, code, main files, libraries, execution parameters, compute,
lakehouse settings and sensitivity-label settings fail locally, including for
`--if-exists skip`. No input format conversion, source-file discovery or
arbitrary definition passthrough is installed.

The advertised deterministic metadata mapping preserves the original explicit
bytes for validation/approval and constructs the native JSON body with
`displayName` from the exact destination, `type: "SparkJobDefinition"` and
`folderId: null`, plus an explicitly supplied `description`. Whitespace and
object-key ordering in the input are not wire-byte preservation promises.
The existing `ItemCollection::SparkJobDefinition` and `ResourceOperation`
construct exactly one `POST /v1/workspaces/{workspaceId}/sparkJobDefinitions`.
The shared operation engine owns acceptance and LRO observation.

The [official Create Spark Job Definition API][api], checked on November 12,
2026 (published source revision
`edc068545c34ad50e4d543d40139cd360a3a2f7d`), requires `displayName`, documents
`definition` as optional, and includes an example without a definition. It
documents contributor permissions, supported capacity requirements, 201/202
responses, and `ItemDisplayNameAlreadyInUse`. Those provider prerequisites and
failures remain real; this command does not provision them.

Historical source evidence is pinned to
`ced983722015e440e989f07d1bf866821d27fe90`: item collection mapping in
`engine/control-plane/resources/items/src/collections.rs`, generic minimal body
in `clients/cli/commands/mutations/src/create.rs` (305-334, 581-583), and
`test_mkdir_item_success[SparkJobDefinition].yaml` (171, 186-187, 223) show the
typed collection and a no-definition 201 recording. A historical recording is
not a universal current API guarantee.

## Policy, approval and evidence

Only `error` and exact-existing `skip` are supported. Unsupported overwrite,
duplicate and `--force` fail through the existing SDK gate before input I/O or
credentials. A bounded workspace item listing resolves the literal name;
ambiguity, wrong type, case-only collision, malformed identity, incomplete
discovery or service failure cannot become a first-match skip. A missing target
under skip still creates. A raced provider conflict is an error, never a skip,
overwrite or replay. The lookup is not atomic with the mutation.

The shared host retains immediate explicit approval and immutable request
context; no approval-bypass switch or credential implementation is added.
Accepted is not completed; an operation ID is not a resource ID. Polling, body,
timeout, cancellation, projection and output failures retain observed evidence
without replay or rollback. The original total deadline, response bounds,
data-only receipt projection and exact no-clobber output sink remain shared.

Offline tests exercise the real parser, `PreparedCreateCommand`, SDK policy and
`HttpExecutor` with scripted HTTP/synthetic credentials, and actual binary help,
local rejection, approval refusal and closed-reader pipe behavior. They do not
perform live Entra authentication, Fabric provisioning, Spark execution, or
tenant reads/writes. Native execution and the automatic full Windows, Linux,
macOS and aggregate CI results must be recorded for the exact published head
separately; source availability is not a claim that those gates passed.

[api]: https://learn.microsoft.com/en-us/rest/api/fabric/sparkjobdefinition/items/create-spark-job-definition

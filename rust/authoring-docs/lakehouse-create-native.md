<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Lakehouse create boundary

The `fabric-sdk-items::lakehouse_create` module supplies a typed native request
for the separate [v2 create contract](create-v2-contract.md). The SDK facade
reexports the same module as `fabric_sdk::items::lakehouse_create`. It does not
change v1 `mkdir`, install a transport, or acquire an identity.
`fabric_cli::v2_create_lakehouse::LakehouseCreateBinding` composes that SDK with
the released `v2-create` host. With `headless,v2-create`, the experimental
`fab-cli-v2` binary routes root `create` to this Lakehouse-only binding through
the shared native runner. Other item kinds are rejected rather than inferred.
The default build and v1 `create` alias remain unchanged.

```text
fab-cli-v2 create /<workspaceGUID>.Workspace/<literal-name>.Lakehouse [--input FILE|-] [--wait] [--if-exists error|skip] [--format json] [--output FILE|-] [--project EXPR] [--timeout DURATION]
```

The root parser still recognizes all original policy spellings, but this binding
rejects unsupported policies. It adds no `--name`, autotype, implicit parent,
collection or artifact facade. An actual write requires explicit selected
identity configuration and immediate approval through a dedicated terminal.

## Native contract and capabilities

The request is exactly `POST /v1/workspaces/{workspaceId}/lakehouses`, using the
existing caller-owned `ResourceOperations` engine. Native HTTP 201 is a completed
Lakehouse response. HTTP 202 is acceptance only; the engine retains the operation
ID and, when requested, polls and retrieves the native result. An operation ID
is never substituted for a Lakehouse ID.

The native API supports users, service principals and managed identities, subject
to service authorization. This is documented provider support, not a claim that
the caller's selected identity is authorized or that live acceptance was tested.

The advertised schema is `fabric.lakehouse.create.metadata/v1`. It admits only
exact named-leaf `error` and `skip` policies. The shared host resolves the
workspace and an exhaustive exact-name item snapshot before planning. Missing
parents, multiple exact matches and wrong-kind matches remain explicit failures.
An exact existing target fails for `error`; `skip` leaves its ID unchanged without
claiming content equality or issuing a POST. Explicit input is validated first.

The native API documents `ItemDisplayNameAlreadyInUse`. This is the execution-time
conflict protection for creation, not a guarantee supplied by a preflight GET.
If a target appears after a missing-target observation, the POST's native failure
is retained; it is not converted into a skip, an update, or an automatic retry.
The released host uses ASCII-case-insensitive discovery and preserves the
requested body spelling. This leaf adds no Unicode normalization or additional
lookup rules. A service rejection for another spelling remains a native failure.

There is no documented complete identity-preserving replacement or repeated-name
fresh-identity guarantee here. `overwrite`, `--force` and `duplicate` must fail
before identity acquisition or service reads, even if a target would be missing.
No delete/recreate, metadata patch, auto-rename or implicit collection selection
is used to emulate those policies.

## Advertised input

Omitted input is legal: the body contains only the exact target `displayName`.
Explicit input must contain that same `displayName`; it is not inferred or
inserted into an incomplete explicit body. Native input can also contain:

| Property | Accepted representation |
| --- | --- |
| `description` | String of at most 256 Unicode scalar values, including empty. |
| `folderId` | Omitted, explicit native null, or a hyphenated UUID; retained unchanged. |
| `creationPayload` | Exactly `{"enableSchemas": true}`; no injected default. |
| `sensitivityLabelSettings` | Native label UUID and optional `ApplyOrFail` or `Ignore` strategy. |
| `definition` | Metadata-only public definition described below. |

Unknown request fields, response-only `id`/`type` fields, unsupported properties,
explicit null outside `folderId`, malformed JSON and duplicate members at any
depth are rejected locally. The strict input wrapper retains raw source bytes
before decoding, including whitespace and CRLF, and bounds input at 4 MiB.
The host still owns bounded file/stdin reading and the total deadline.

### Metadata-only public definition

The native Lakehouse public format is `LakehouseDefinitionV1`; omission of
`format` leaves the native default unchanged. This bounded schema accepts exactly
one `lakehouse.metadata.json` part with `payloadType: "InlineBase64"`. Its decoded
UTF-8 JSON object may contain the documented optional string `defaultSchema`.
Duplicate members, unknown metadata properties, malformed base64 and invalid
part envelopes are rejected. The original base64 and JSON values are sent
unchanged; validation does not reencode the definition.

The broader service definition also supports optional shortcuts, data-access
roles, ALM settings and `.platform` parts. This slice does **not** advertise those
schemas. Such parts are rejected, not silently dropped or partially validated.
It does not upload data or infer tables, files, connections, permissions or
default schemas from a definition or target.

## Effects, approval and evidence

Fabric itself automatically provisions a SQL analytics endpoint alongside a
Lakehouse. The CLI must not send another SQL endpoint creation request. Lakehouse
acceptance or completion does not establish that the SQL endpoint is ready.
Preserve actual `sqlEndpointProperties`, IDs, OneLake paths, native unknown
fields and provisioning states without inventing success. No Spark execution,
table load, data upload, publish or semantic model creation is implied.

The shared host must approve the exact immutable target, body, raw source and
selected account/tenant/audience/endpoint context immediately before dispatch.
`--force` is unsupported overwrite intent, never an approval bypass.
Projection applies only to receipt data, preserving identity, effects and
operation handles. Output failure, timeout and cancellation retain uncertainty;
they never authorize rollback or replay.

## Source evidence and validation boundary

The source executable gap was verified at integration
`ced983722015e440e989f07d1bf866821d27fe90`: `v2_main.rs` did not dispatch create,
and the existing CLI/SDK create modules supplied only pure policy contracts.

| Public source | Pinned documentation revision |
| --- | --- |
| [Create Lakehouse](https://learn.microsoft.com/en-us/rest/api/fabric/lakehouse/items/create-lakehouse) | `edc068545c34ad50e4d543d40139cd360a3a2f7d` |
| [Lakehouse definition](https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/lakehouse-definition) | `cbf487710506ba99809e8c200232edf2a465bdd8` |
| [Manage a Lakehouse with the REST API](https://learn.microsoft.com/en-us/fabric/data-engineering/lakehouse-api) | `3893b03d0246df2a1978058f5542399274d1b243` |

Offline SDK schema, parser-to-host-to-scripted-HTTP and native process tests are
source contracts, not live Fabric proof. Native execution and full gates must
be verified against the exact combined revision before delivery. No live
Fabric authentication, read, create or compute operation is authorized here.

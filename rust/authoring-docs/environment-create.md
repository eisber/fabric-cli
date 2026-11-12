<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native Environment creation

The opt-in `v2-create-environment` feature supplies `EnvironmentCreate` through
the [shared create host](create-host.md). With `headless`, it mounts the existing
`fab-cli-v2 create` parser and native runner. Default builds, `fab-rs`, and Python
`fab mkdir`/`create` are unchanged. This is not Environment publish, settings,
library upload, compute configuration, session creation, or execution.

```text
fab-cli-v2 create /11111111-1111-1111-1111-111111111111.Workspace/Example.Environment
fab-cli-v2 create /11111111-1111-1111-1111-111111111111.Workspace/Example.Environment --input metadata.json --wait
```

The workspace is an explicit existing GUID; the leaf is a literal root-level
display name, even if it looks like a GUID. There is no workspace discovery,
folder creation, parent collection, or artifact facade. The local name limit is
256 UTF-8 bytes, a conservative host bound rather than a universal API limit.

Input may be omitted, selecting `displayName` from the exact destination.
Explicit input is a unique-key UTF-8 JSON object, bounded to 4 MiB. It requires
`displayName` equal to the literal destination and permits only:

| Field | Accepted value |
| --- | --- |
| `displayName` | Exact destination name |
| `description` | String of at most 256 Unicode characters; omission differs from null |
| `type` | Optional exact `Environment` consistency assertion, not a provider request field |
| `folderId` | Optional null only; this slice creates in the workspace root |

Definitions, creation payloads, sensitivity-label settings, library and compute
settings, read-only properties, unknown fields, duplicate keys (including escaped
duplicates), empty input, and JSON null are rejected before identity or HTTP.
The native body is `{"displayName":"Example","folderId":null}`, optionally with
the supplied description. The local `type` assertion is not sent: it is absent
from the current typed `CreateEnvironmentRequest` schema. The original explicit
bytes remain bound to the planner's validated input and immediate approval.
V1 retains its existing `type`-bearing payload without modification.

## Provider contract and conflict behavior

The official [Create Environment API][create] was read on November 12, 2026,
at documentation revision `edc068545c34ad50e4d543d40139cd360a3a2f7d`.
It documents definition-free `POST /v1/workspaces/{workspaceId}/environments`,
required `displayName`, optional `description` (maximum 256 characters), and
optional/null `folderId` meaning the workspace root. It documents 201 completion,
202 acceptance with operation headers, and `ItemDisplayNameAlreadyInUse`.
Contributor permission, `Environment.ReadWrite.All` or `Item.ReadWrite.All`,
and supported Fabric capacity remain service requirements. No live service or
identity acceptance is claimed.

The Python source pin is `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`;
the integration base is `ced983722015e440e989f07d1bf866821d27fe90`.
The existing Rust `commands/mutations/src/create.rs` constructs V1 metadata;
`ItemMutations::create_with_completion` supplies the typed collection request.
The historical Environment mkdir recording is evidence of prior behavior only,
not the current API schema, universal availability, or race-proof uniqueness.
New tests use synthetic payloads exclusively.

Only `error` and exact-existing `skip` are supported. Complete bounded generic
item listing establishes a local observation, not an atomic name reservation.
An exact, unique, correct-kind match may be skipped only after validating all
explicit input. Case-only collisions, ambiguous names, wrong kind, invalid
identity, or incomplete discovery fail closed. Missing-target skip selects one
create request. A provider collision, including any 409, remains a failure;
there is no raced-skip fallback, retry, upsert, or atomic uniqueness claim.
`--force`, overwrite, and duplicate fail at the existing shared policy gate
before opening input or acquiring credentials.

## Approval, receipts, and validation boundary

The existing explicit native session supplies one immutable identity, Fabric
audience and endpoint, and the original whole-command deadline. Immediate
approval is caller-injected; the executable requires an explicit terminal
confirmation and refuses redirected approval before credential/provider access.
The approval inspects the actual prepared POST method, bound URL, generated
metadata bytes and selected create/skip action, not merely the original input.
The exact same operation is then consumed once. The metadata-only binding
declares no additional child-creation or compute-operation effects; that is not
a billing or universal service-side-effects guarantee. The supported-capacity
requirement is not permission to provision or change capacity.
The shared SDK planner remains the sole policy authority. There is no new HTTP,
auth, parser, or long-running-operation engine.

201 and 202 remain distinct. Waiting uses the original operation and deadline;
no mutation is replayed. Actual resource and operation IDs, accepted data, native
errors and uncertainty survive cancellation, failed polling, projection, or
no-clobber/broken-output errors. Local cancellation is not remote cancellation
or rollback. Projection affects receipt `data` only.
Initial acceptance remains separate from the latest observation in protected
`meta.submission.httpStatus`, `meta.submission.resource`, and
`meta.submission.operationHandle`. A malformed final result cannot erase the
accepted resource identity or promote the outcome to completed.

`tests/environment_create.rs` covers parser-to-host-to-SDK-to-scripted-HTTP
behavior and the native process boundary. Explicit impact profiles register
both the library and `headless` process configurations for Windows, Linux, and
macOS. Source and registered tests do not establish executed platform acceptance;
the exact-head validation receipt and ordinary PR CI report actual results.
No live Entra, Fabric provisioning, publish, or compute operation is authorized.

The root-folder skip regression is retained as a fail-closed requirement while
the shared helper's original owner supplies a follow-up. At this source stage,
that gap remains blocking: registered regression source is not a passing check
or completed exact-target implementation.

[create]: https://learn.microsoft.com/en-us/rest/api/fabric/environment/items/create-environment

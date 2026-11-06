<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded v2 definition copy

The opt-in `fabric-cli/v2-copy` feature adds a single-item definition-copy route
to the existing native host. It is an incremental v2 capability, not a complete
implementation of every copy target or a change to the default compatibility
build. Select it alongside `native-read` or `headless` for an executable host.
Libraries can compose it with `native-host` and caller-injected dependencies.
Enabling the feature does not discover credentials or authorize a service call.

## Supported scope

The route accepts an existing Notebook or SemanticModel and a separately
resolved, explicit, non-existing item destination of the same type. The current
`copy` alias and `cp` parser are reused; this does not finalize the v2 path
grammar, introduce a new flag, or create implicit destination names.

Copies create a new item from its raw public definition. They do not copy
physical data, sensitivity labels, access grants, connection credentials,
refreshed model partitions or dependent items. The raw source definition remains
unchanged. The derived creation request omits the optional `.platform` part so
that it does not carry the source item's Git logical identity into a new item.
Every other definition part is preserved unchanged; this is not export cleanup
or dependency remapping. The receipt records whether `.platform` was omitted.
Returned service IDs and actual operation outcomes determine the receipt, not
the requested name or a preflight existence lookup.

This create-new metadata policy matters for Git-backed sources:
[items with the same logical ID represent the same item, and one workspace
cannot contain two items with the same logical ID][logical-identity].
[Get Item Definition always returns `.platform`, and Create Item respects it
when supplied][platform-definition]. Omitting that optional metadata also avoids
assuming that the requested destination name overrides embedded source metadata.
The [Notebook creation definition][notebook-definition] explicitly makes
`.platform` optional; the [SemanticModel definition][model-definition] supports
creation without it. Existing raw definition helpers and compatibility oracles
are not changed by this v2-only policy.

The destination must not already have an item ID. An existing item fails even
with `--force`. There is no update, replacement or upsert fallback. The provider
create operation, rather than a local lookup, determines whether concurrent
creation conflicts. A prospective destination is not an atomic reservation.

Workspace and folder destinations, recursive selection, table cloning, shortcut
dereferencing and other item types are outside this first definition slice.
Unsupported pairs fail explicitly. Physical local/OneLake file routes continue
using their existing adapters, limits, source-handle lifetime and partial-write
policies; enabling definition copy does not make them transactional or recursive.
Moves never use this definition-copy route.

## Execution and evidence

The caller supplies the resolved endpoints, immutable account/tenant/endpoint
context, existing HTTP executor, cancellation, confirmation and output streams.
Without `--force`, the host warns that definitions omit sensitivity labels and
physical data and obtains explicit confirmation through the fallible UI.
Decline performs no definition read or create. Force skips only confirmation,
not target validation, permission checks or destination conflicts.

Source metadata and `getDefinition` use one bounded operation session. Creating
the destination uses a fresh session with the same bound context and latched
cancellation. This separation is required because `getDefinition` is a POST and
the shared session deliberately prohibits a second potentially mutating request.
No new transport, authentication provider, retry engine, runtime or poller is
introduced.

The reusable workflow lives in `fabric_deploy::definitions`. It preserves raw
definition parts instead of passing through export cleanup, SET encoding or
local files. A metadata-only response after definition access denial cannot be
copied. Payload and response bounds remain explicit caller inputs.

Creation returns acceptance without implicit observation. A valid HTTP 201
receipt contains the actual created item ID. HTTP 202 is accepted, not a
completed copy, and preserves available operation identity. Invalid or missing
completion identity, service/transport failure, cancellation and uncertain
dispatch remain errors with their original evidence. Neither cancellation nor
output failure causes automatic retry, deletion or rollback.

The central host retains definition-copy evidence separately from presentation,
including when rendering or stream delivery fails. It does not invent cache
updates for a requested name or accepted operation.

## Offline validation

The dedicated producer and host targets exercise injected credentials and
synthetic service responses, never a live Fabric workspace:

```text
cargo test --offline --locked -p fabric-deploy --test definition_copy_v2
cargo test --offline --locked -p fabric-cli --features native-host,v2-copy --test definition_copy_v2
```

The impact registry records the host target's required features and a `v2-copy`
profile for Windows, Linux and macOS. Integration still requires the existing
dependency-impact plan, all-target checks, linting, unchanged oracles,
independent reviews and exact combined-candidate validation. A registered test,
passing mock or one platform's result does not prove live service, identity
policy or cross-platform acceptance.

[logical-identity]: https://learn.microsoft.com/en-us/fabric/cicd/git-integration/source-code-format
[platform-definition]: https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/item-definition-overview
[notebook-definition]: https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/notebook-definition
[model-definition]: https://learn.microsoft.com/en-us/rest/api/fabric/articles/item-management/definitions/semantic-model-definition

<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Explicit metadata retrieval and exact local output

This additive library slice implements primary workspace and generic item
metadata reads independently of the Python-compatible GET handler. It does not
change `fab-rs get` or claim the full selected v2 get surface is implemented.
The separate optional [v2 get head](v2-get.md) now composes these same primitives
with explicit source/file/observer and provider-injected docs capabilities.

## Shared read operation

`fabric_sdk::metadata::{MetadataHost, MetadataTarget, MetadataLimits}` is a
frontend-neutral read boundary. The CLI's `get_metadata` module reexports the
same types; direct SDK and future execution heads do not depend on CLI types.
Callers supply resolved workspace/item identifiers, an immutable SDK client,
the existing read executors, cancellation and nonzero response/result byte
limits. The operation creates no runtime, credential provider or global state.

| Target | Requests |
| --- | --- |
| `MetadataTarget::Item` | One generic `GET /v1/workspaces/{workspaceId}/items/{itemId}` |
| `MetadataTarget::Workspace` | One primary `GET /v1/workspaces/{workspaceId}` |

These are metadata reads, not definitions, connections, schedules, roles, Spark
settings or related-target enumeration. No query changes acquisition. The
focused workspace SDK and facade expose the additive
`get_workspace_primary_metadata`; existing enriched GET delegates its primary
read to this method and preserves its optional enrichment behavior.

The shared SDK enforces endpoint/audience/identifier checks before token
acquisition, response bounds before decoding, typed service errors and
cancellation. Executors remain responsible for finite deadlines/retries and
disabling redirects. The operation preserves unknown fields, parsed key order,
JSON nulls and number types. Compact JSON encoding is bounded as it writes:
exactly the selected result limit fits; an additional byte fails rather than
returning a truncated artifact.

`MetadataRead` exposes the resolved target, complete parsed `data()` and exact
`json_bytes()`. These are unprojected JSON data bytes without a newline or result
envelope. A caller owns its common envelope and observation time; this slice
does not manufacture freshness, source revision, full-artifact coverage or
authoring guarantees. A failed or accepted-but-incomplete response is not a
successful metadata object.

## Exact local delivery

`fabric_cli::get_metadata::MetadataFileRequest::preflight` validates one resolved
absolute local filename in the caller's blocking context, before remote reads.
It uses `fabric_cli_storage_files::ExactOutputFile`, not the existing
timestamp-directory or truncating transfer path.

Call `acquire` with the shared metadata host to obtain bounded content
asynchronously. Successful acquisition does not create a file. Then call
`MetadataFileOutput::commit` in the caller's blocking context to write the exact
JSON data bytes. The SDK and output use the same result byte cap.

Preflight rejects any existing entry, including directories and dangling
symlinks. `create_new` independently enforces no-clobber at delivery, including a
file created after preflight. There is no implicit parent directory, extension,
timestamp, remote write or overwrite. A failed/cancelled acquisition and
abandoned delivery produce no output file.

Commit does not claim atomic full-file replacement or durable storage. After
exclusive creation, a write/flush failure may leave partial local effects.
Typed errors retain creation status, bytes written, stage and original I/O
cause. The caller must not start commit after observing cancellation; once a
blocking commit begins, this slice does not promise cancellable writes or
rollback. Ancestor namespace stability is caller-owned, not a hostile-process
filesystem sandbox.

## Scope and next integration boundary

The existing v1 parser, native GET host and independent oracles are unchanged.
The optional v2 head adds its own bounded typed-path resolution, result metadata,
typed projection, stdout policy, native source/file bindings, handle observation
and provider-injected recursive docs. These compose shared SDK contracts rather
than infer operations from v1 query text. The existing entry points here retain
their narrower metadata-only semantics.

FabricAIHub docs integration is not activated here. A hosted provider may reuse
its Markdown rendering, but a viewport cannot be called a complete file and a
Markdown envelope cannot be advertised as a native schema DTO. Any private
provider admission, structured coverage/navigation contract or service change
needs separate evidence and authorization.

Offline public consumers cover exact requests and token scopes, focused/facade
equivalence, unknown metadata, invalid IDs/audiences, HTTP and malformed response
failures, response/result bounds, cancellation and exact-file composition.
Storage tests separately cover no-clobber races, binary/empty bytes, platform
path restrictions and write/flush partial evidence. No live authorization or
cross-platform execution is established by these tests.

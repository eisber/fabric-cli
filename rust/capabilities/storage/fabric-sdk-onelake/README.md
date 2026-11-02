<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# OneLake DFS operations

`OneLakeClient` provides directory listing, exact file reads, properties,
directory creation, deletion, rename and bounded file upload.

Construct it with an injected credential and `Audience::OneLake` selection.
Each operation takes an executor, explicit limits and cancellation. The token
scope is `https://storage.azure.com/.default`, not the Fabric API scope.

## Reads

`OneLakePath::new(workspace, item, relative)` validates item-relative resource
paths without accepting arbitrary URLs. The default service host is
`onelake.dfs.fabric.microsoft.com`.

| API | Data |
| --- | --- |
| `list` | Ordered path rows and actual page count |
| `read` | Exact file bytes, including valid empty files |
| `read_file` | File bytes and same-response headers |
| `properties` | HEAD response headers |
| `directory_metadata` | Directory metadata with explicit merge behavior |
| `item_roots` | Item-root metadata |

Continuation tokens are opaque, encoded once and checked for cycles. Active
traversal is bounded by pages, rows and aggregate bytes. Malformed responses
and authorization failures are not treated as missing files or empty lists.

## Writes

`create_directory`, `delete_path`, `rename_path` and `upload_file` reuse the
same credential/executor boundary. Writes are single-attempt.

Upload requires HTTP 201 from creation before append and flush. Byte positions
and received stage statuses are retained. The export-specific upload method
applies its own documented text-position policy without changing ordinary
binary upload.

`OneLakeWriteOutcome` separates acceptance from completion.
`OneLakeMutationError` retains earlier stage responses and possible effects.
Stopping local waiting does not roll back remote work.

Path, payload, media-type and header limits apply before mutation.
Invalid native names and unsupported rename headers fail explicitly; response
headers cannot override HTTP framing.

## Shortcuts and composition

`shortcuts` uses Fabric `ResourceOperations` and the Fabric audience, not a DFS
token. Literal shortcut names are safely encoded as path segments.

The [HTTP adapter](../fabric-sdk-onelake-http/README.md) connects this SDK to the
shared execution engine. No runtime, transport, identity discovery, retries or
table interpretation is installed here.

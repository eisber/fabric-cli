<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# OneLake DFS operations

This SDK-layer crate implements bounded directory listing, exact file-byte reads
and HEAD properties, directory create/delete/rename and bounded file upload.
It has no CLI, identity-backend, deployment or SDK-facade
dependency. It installs no runtime, transport or retry/LRO policy.

Source: Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`client/fab_api_onelake.py`, `client/fab_api_client.py`,
`commands/fs/{ls,get}/fab_fs_*_onelake.py`.

The caller creates `OneLakeClient` from an `Arc<dyn TokenCredential>` and explicit
`CredentialSelection` with `Audience::OneLake`, then supplies `OneLakeExecutor`,
`OneLakeLimits` and a cancellation future to each operation.

| API | Fixed request | Result |
| --- | --- | --- |
| `OneLakePath::new(workspace,item,relative)` | No I/O; validates item-relative path | Explicit resource; no arbitrary URL |
| `client.list(executor,path,recursive,limits,cancelled)` | GET `https://onelake.dfs.fabric.microsoft.com/{workspace}/?recursive=false&resource=filesystem&directory={item}/{relative}&getShortcutMetadata=true` | Raw path rows in page order, actual page count |
| `client.read(executor,path,limits,cancelled)` | GET `https://onelake.dfs.fabric.microsoft.com/{workspace}/{item}/{relative}` | Exact bytes; empty is a valid file |
| `client.read_file(executor,path,limits,cancelled)` | Same single GET | Exact bytes and same-response headers for file copy |
| `client.properties(executor,path,limits,cancelled)` | HEAD at the same file URL | Exact response headers |

The encoded query preserves directory values; query spelling/order and source
`continuationToken` spelling are explicit. Unlike source string concatenation,
reserved characters are encoded as values, not interpreted as query injection.
This is an explicit safety boundary, not byte-for-byte raw-URL parity.
The storage token scope is `https://storage.azure.com/.default`, never the
Fabric API scope. Selection is checked before token acquisition; the injected
provider must already be bound to the selected account and tenant.

`OneLakeRequest` exposes finite GET/HEAD/PUT/PATCH/DELETE methods, validated DFS URL,
exact body/safe headers, injected token and streaming byte limit. The implemented
[`OneLakeHttpExecutor`](../fabric-sdk-onelake-http/README.md) composes the existing
shared HTTP engine through its explicit `PublicOrigin::OneLake` boundary.
The Fabric-only origin guard is unchanged. Private-link/sovereign hosts are not
selected implicitly.

Listing retains unknown row fields, validates path rows, bounds pages/paths/bytes,
detects repeated continuation tokens, and never reports partial data as complete.
Malformed bodies or a 403 are not absence. HTTP status and storage header error
codes are retained as data; display text is sanitized. Cancelled futures drop
local request/credential work without claiming any completed response.

The synthetic consumer exercises `_delta_log` listing/JSON bytes as the public
table dependency seam. Table schema parsing, Delta interpretation, load and
maintenance orchestration remain with the table owner. The special item-root
listing route, shortcut metadata/targets and table mutation semantics remain
unported. No live parity is established.

## Storage writes

`create_directory`, `delete_path`, `rename_path` and `upload_file` reuse the same
credential/cancellation/executor path as reads. PUT/PATCH/DELETE are single-attempt
through the shared HTTP adapter. No retry, cleanup, rollback or LRO stack is added.
`OneLakeWriteOutcome` retains actual status, separating 202 acceptance from
completed responses. `OneLakeMutationError` retains the underlying typed failure,
possible side effects and previously received upload-stage responses.

Directory create preserves trailing slash, `resource=directory` and
`If-None-Match: *`; delete preserves `recursive=true`; rename preserves the
source helper's `workspace/item/path` header without adding a leading slash.
Upload follows Python's exact create-file (201 only) -> append at byte zero ->
flush at content byte length sequence. Other create statuses do not trigger
append/flush, and accepted intermediate responses remain explicitly recorded.

File bodies default to a measured 4 MiB cap, checked before target creation; the
shared transport's 4 MiB maximum remains authoritative even if a caller requests
a larger SDK bound. Media types/header allocation are validated before file
creation. Empty bytes are allowed at the SDK level; the CLI copy branches own
their source-empty checks. Paths selecting an item root and non-ASCII rename
headers fail explicitly as safety boundaries. The shared transport owns actual
HTTP framing/Content-Length; framing headers cannot be overridden by callers.

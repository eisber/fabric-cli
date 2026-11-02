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

## Incremental file I/O

The additive `file_ranges` module supports files larger than 4 MiB without
changing the buffered APIs or allocating the entire file. Its types are public
under `fabric_sdk_onelake::file_ranges`; methods still use the same explicitly
injected `OneLakeClient`, executor, credential selection and cancellation.

| API | Contract |
| --- | --- |
| `file_version(executor, path, limits, cancelled)` | Validated `OneLakeFileVersion` with `len()`, strong `etag()` and optional `content_type()` |
| `read_range(executor, path, version, offset, length, limits, cancelled)` | Exact bounded `OneLakeReadRange { offset, body, eof }` |
| `start_file_upload(executor, path, length, content_type, condition, limits, cancelled)` | Conditionally created `OneLakeFileUpload` session |
| `upload.append_at(executor, position, content, cancelled)` | One bounded, explicitly positioned, ETag-conditioned append |
| `upload.flush(executor, cancelled)` | Final flush only after all declared bytes are acknowledged |

`OneLakeChunkLimits` bounds each request's chunk and service response separately;
`max_chunk_bytes` cannot exceed 4 MiB. A range is clipped only at the declared
EOF. Every nonempty read sends `Range` and the snapshot's `If-Match`; HTTP 206,
ETag, Content-Range, Content-Length and actual bytes must agree exactly. Ignored
ranges, compressed representations, malformed headers, source changes and
truncation fail rather than returning bytes. Exact EOF, including an empty
version, returns an empty range without a GET.

Creation requires `OneLakeFileCreateCondition::IfAbsent` or `IfMatch(etag)`.
Only HTTP 201 with a strong ETag creates a session. Append HTTP 202 is normal
acknowledgement of uncommitted bytes: `position()` advances, but `is_complete()`
stays false until a validated final HTTP 200 flush. Empty files need creation
and flush, not append. Gaps, overlaps, oversized chunks and early flushes fail
before dispatch.

`OneLakeChunkError` preserves the typed SDK failure, acknowledged position,
possible side effects, and received nonconforming response. Service failures
retain their original structured code/body through `OneLakeError::Http`.
Accepted creation/final flush is incomplete, not success. Lost, cancelled or
malformed write responses block further writes on the session. Dropping an
append/flush future after local validation also blocks further writes, even if
dispatch is unknown. A validated append acknowledgement alone restores the
session for the next chunk; purely local invalid input does not poison it.
No replay, automatic cleanup, copy/delete fallback or automatic flush occurs
on drop.

These are SDK prerequisites, not whole-file CLI or tree-engine completion.
Callers own incremental local I/O, traversal, transfer completion and move policy.

## Shortcuts and composition

`shortcuts` uses Fabric `ResourceOperations` and the Fabric audience, not a DFS
token. Literal shortcut names are safely encoded as path segments.

The [HTTP adapter](../fabric-sdk-onelake-http/README.md) connects this SDK to the
shared execution engine. No runtime, transport, identity discovery, retries or
table interpretation is installed here.

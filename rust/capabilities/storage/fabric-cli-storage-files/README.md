<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Local and OneLake file transfers

`StorageFilesHost::download` and `upload` compose a `OneLakeClient`,
`OneLakeExecutor`, `LocalFileSystem` and caller-owned notices. Definition
import/export is separate from these physical file transfers.

`OsFileSystem` uses the caller's existing Tokio blocking executor. It creates no
runtime and performs no blocking filesystem work on the async worker. The caller
supplies credentials, storage audience, transport, timing, cancellation and output.
See the [host contract](HOST-CONTRACT.md) for composition details.

## Download

A directory destination uses the remote basename; a file destination is used as
supplied. Existing files are overwritten. Empty response bytes fail before the
destination is opened. Content length is not used as a substitute for received
bytes.

Downloads preserve raw bytes without decoding or newline conversion. They do
not create missing parents, temporary files or backups. A write failure can
leave a destination truncated or partially written; typed evidence retains
completed byte counts and whether the file was opened.

## Upload

Upload validates the selected writable OneLake root, inspects the local source,
resolves destination naming, emits progress and reads binary contents. The
source handle remains alive through create, append and flush.

Create must return HTTP 201 before later stages proceed. Received append/flush
acceptance remains distinct from confirmed completion. A later fault retains
each received stage and mutation uncertainty; no automatic deletion or replay
is attempted.

## Filesystem and safety policy

The OS adapter requires an explicit local directory boundary. Existing ancestors
and links are resolved before opening; paths escaping the boundary and unsafe
basename joins are rejected. Invalid-Unicode destination names fail rather than
being converted lossily.

Reads and uploads are bounded, including growing local files. Upload uses the
smaller of the caller's request limit and the SDK payload limit. Short writes,
unexpected response statuses, path-policy failures and cancellation are explicit
errors.

These handlers transfer files, not directory trees. Recursive workspace copies,
folder traversal, tables and shortcut traversal require separate orchestration.
Stopping local waiting is not rollback.

## Offline example

From `rust/`:

```text
cargo run --offline --locked -p fabric-cli-storage-files --example offline_copy
```

The example accepts JSON objects on stdin and requires an existing disposable
`root`. It uses synthetic HTTP responses and real local fixture files without
contacting a service or acquiring tokens.

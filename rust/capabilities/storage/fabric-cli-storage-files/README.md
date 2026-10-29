<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Local and OneLake file transfers

`StorageFilesHost::download` and `upload` compose a `OneLakeClient`,
`OneLakeExecutor`, `LocalFileSystem` and caller-owned notices. Definition
import/export is separate from these physical file transfers.

`OsFileSystem` uses the caller's existing Tokio blocking executor. It creates no
runtime and performs no blocking filesystem work on the async worker. The caller
supplies credentials, storage audience, transport, timing, cancellation and output.
See the [host contract](HOST-CONTRACT.md) for composition details.

## Resource command adapter

`ResourceFileHost` composes typed resource `cp`/`mv` commands with the existing
physical handlers and remote `ResourceTransferHost`. It resolves source first
(`raise_error: true`) and destination second (`false`), allowing caller-owned
local fallback on both sides. `execute_resolved` accepts the metadata owner's
already support-checked objects without repeating resolution or notices.

Local-to-OneLake and OneLake-to-local copies do not use force, recursive or collision
flags. This does not enable recursion:
directory/shortcut/table sources retain their source-specific rejection.
Local-to-local copies and **every move involving a local path** are rejected.
No copy-then-delete emulation or local deletion capability exists.

`ResourceFileResult` and `ResourceFileError` retain original remote, upload and
download receipts separately from rendered output. The host must keep that
evidence if presentation or stream delivery fails; output failure never retries
a transfer. A missing file capability, metadata failure or unsupported `--skill`
header path remains an explicit error.

`SelectedFileSystem::defer` binds one absolute native candidate and an explicit
boundary without touching the filesystem. `with_input` binds its exact original
spelling, including relative input, without cwd lookup. Only that alias can be
inspected/read/written; a selected existing directory additionally permits one
direct download basename. The underlying `OsFileSystem` still enforces its
unchanged canonical-ancestor, symlink and basename policy. Boundary selection
does not grant access to sibling files, a drive or recursive descendants.

The native host composes this adapter through explicit asynchronous local
metadata snapshots and `SelectedFileSystem`. Fabric resolution precedes local
classification; authorization, service and cancellation failures never become
permission to inspect a local path. Existing embedded dispatchers do not acquire
filesystem authority unless their caller supplies it.

There is no dependency on definition import/export or its file policy.

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

Run the resource routing comparisons and focused adapter tests:

```text
python capabilities/storage/fabric-cli-storage-files/tests/resource_dispatch_reference.py
cargo test --locked -p fabric-cli-storage-files --test resource_command --test selected_adapter
```

The Rust tests exercise the resource adapter with injected HTTP and private OS
files. Upload/download suites cover status, byte bounds, interruption and partial
effects. Native host tests additionally exercise root argument dispatch, explicit
local fallback, directory destinations, alias binding and stream delivery.

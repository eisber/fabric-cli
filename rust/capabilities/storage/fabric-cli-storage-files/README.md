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

`ResourceFileHost::execute` composes the pinned typed resource `cp`/`mv` commands with the existing
physical handlers and remote `ResourceTransferHost`. It resolves source first
(`raise_error: true`) and destination second (`false`), allowing caller-owned
local fallback on both sides. `execute_resolved` accepts the metadata owner's
already support-checked objects without repeating resolution or notices.

For the retained oracle entry points, local-to-OneLake and OneLake-to-local copies do not use force, recursive or collision
flags. This does not enable recursion:
directory/shortcut/table sources retain their source-specific rejection.
Local-to-local copies and **every move involving a local path** are rejected.
These are retained oracle adapters. The owner-authorized production
`execute_full_resolved` instead provides additive recursive physical transfers
and checked moves, including local-to-local operations. It uses version-pinned,
incremental native leases and SDK ranges/uploads: 4 MiB bounds each request,
not the complete file. See the
[additive operation matrix and safety contract](TREE-OPERATIONS.md).

`ResourceFileResult` and `ResourceFileError` retain original remote, upload and
download receipts separately from rendered output. The host must keep that
evidence if presentation or stream delivery fails; output failure never retries
a transfer. A missing file capability, metadata failure or unsupported `--skill`
header path remains an explicit error.

By default, `SelectedFileSystem::defer` binds one absolute native candidate and an explicit
boundary without touching the filesystem. `with_input` binds its exact original
spelling, including relative input, without cwd lookup. Only that alias can be
inspected/read/written; a selected existing directory additionally permits one
direct download basename. The underlying `OsFileSystem` still enforces its
unchanged canonical-ancestor, symlink and basename policy. Boundary selection
does not grant access to sibling files, a drive or recursive descendants.
Production uses the explicit `with_tree` capability for bounded descendants,
with separate routing for the selected source and destination.

The native host composes this adapter through explicit asynchronous local
metadata snapshots and `SelectedFileSystem`. Fabric resolution precedes local
classification; authorization, service and cancellation failures never become
permission to inspect a local path. Existing embedded dispatchers do not acquire
filesystem authority unless their caller supplies it.

There is no dependency on definition import/export or its file policy.

## Pinned Python operation matrix (not the additive production behavior)

The current Python source and immutable compatibility source
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` have the same physical transfer
branches in `commands/fs/fab_fs_cp.py`, `cp/fab_fs_cp_onelake.py`,
`fab_fs_mv.py` and `mv/fab_fs_mv_onelake.py`. This baseline remains independently
tested; the additive native host intentionally extends its directory/local-move
rejections as described above.

| Source -> destination | File | Directory, including nested/empty directories | Flags and destination selection |
| --- | --- | --- | --- |
| Local -> OneLake | Upload through create/append/flush | Source rejects with `Recursive copy not supported` | Copy force/recursive/collision flags are ignored; directory/shortcut destination appends the local basename, file/prospective destination is exact |
| OneLake -> local | Binary download | Source rejects folder/shortcut/table sources | Copy flags are ignored; an existing local directory appends the remote basename; missing parents are not created |
| OneLake -> OneLake `cp` | Read then upload | Source rejects folder/shortcut/table sources | Copy flags are ignored, including `--block_on_path_collision`; existing targets overwrite, directory/shortcut destinations append the source basename |
| OneLake -> OneLake `mv` | Service rename | Service rename moves the entire subtree without enumeration, retaining empty directories | `--force` bypasses confirmation; `--recursive` is ignored; the destination is exact, **not** an appended basename |
| Local -> local `cp`, any local `mv` | Source rejects | Source rejects | No local deletion or copy-then-delete emulation |
| OneLake `mkdir` | Not a file operation | Conditional directory create | Existing paths fail through the service; an explicitly empty `-P` requests source parameter help |
| OneLake `rm`/`del` | Delete path | Service-side recursive delete | `--force` bypasses confirmation, not storage errors or uncertainty; no separate recursion flag is needed |
| Shortcut `rm`/`del` | Fabric metadata delete | Never DFS recursive deletion | Source confirmation/force semantics; underlying shortcut data is not deleted |

The pinned Python OneLake move is limited to a Lakehouse source and `/Files` paths.
Accepted writes/deletes/renames remain accepted, not completed. A failed or
interrupted rename is never retried, and these commands never delete a source
after a speculative copy. A copy to the same resolved file reads the content
before target creation, as in Python; rename overlap and collision decisions
remain with the single service rename, without client-side delete or retry.

The owner explicitly approved the separate recursive storage-copy and local-move
extension. The table above records Python provenance, not remaining native
directory-copy/local-move refusals.
Recursive Fabric workspace/folder definition-copy orchestration is a different
command branch, outside this physical-file library.

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
For a selected directory download, symlinks are confined to that directory, not
its broader parent boundary. Portable path checks are not a hostile-process
namespace sandbox against concurrent link substitution.

Reads and uploads are bounded, including growing local files. Upload uses the
smaller of the caller's request limit and the SDK payload limit. Short writes,
unexpected response statuses, path-policy failures and cancellation are explicit
errors.

The original `StorageFilesHost` handlers transfer files, not directory trees.
The additive tree host supplies physical directory traversal and safe moves.
Recursive Fabric definition copies, tables and shortcut-source traversal remain
separate operations.
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

The additive tree contracts use:

```text
cargo test --offline --locked -p fabric-cli-storage-files --tests
cargo test --offline --locked -p fabric-cli --features native-read --test native_resource_files
```

The Rust tests exercise the resource adapter with injected HTTP and private OS
files. Upload/download suites cover status, byte bounds, interruption and partial
effects. Native host tests additionally exercise root argument dispatch, explicit
local fallback, directory destinations, alias binding and stream delivery.

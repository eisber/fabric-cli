<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Central-host physical copy contract

This is the reusable contract of `fabric-cli-storage-files`, not a second parser,
Fabric-context resolver, generic resource mutation engine, or production host.
Python behavior is pinned to `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
The lane preserves its original exact Rust base and safely merged exact shared
checkpoint `61d77e330be8a13cbd5033713f1f2badcecfce60`; neither newer Python
ancestry nor a peer's uncommitted changes are required.

## Composition and route selection

The existing central host owns argument parsing, `cp`/`copy` aliases, Fabric-first
context resolution, multi-token path extraction, support checks and output mode.
After those source checks, select only these physical routes:

| Resolved source -> target | Public library operation | Source dispatch |
| --- | --- | --- |
| Local path -> OneLake resource | `StorageFilesHost::upload` | `commands/fs/fab_fs_cp.py:56-57` |
| OneLake resource -> local path | `StorageFilesHost::download` | Same `:60-61` |
| OneLake -> OneLake | Existing resource owner's `OneLakeWriteHost::copy_file` | Same `:52-53`; not this physical-filesystem lane |
| Local -> local | Reject with source `NotSupported`/`WARNING_INVALID_PATHS` | Same `:64-72`; do not call a native local copy fallback |
| Local <-> other Fabric element | Reject with source `InvalidInput`/`WARNING_INVALID_PATHS` after normal support checks | Same `:64-67` |
| Any `mv`/`move` involving local paths | Source has no physical local move route | `commands/fs/fab_fs_mv.py:48-55`; differing types fail `InvalidInput`, local->local fails `NotSupported` |

Workspace/item/folder definition copies and moves, OneLake rename, and definition
export/import belong to their assigned resource/transfer owners. In particular,
do not emulate a local move by copy followed by delete: Python never selects
such a route. A successful library file transfer is not command-dispatch parity.

Construct one host borrowing these explicit dependencies:

- `OneLakeClient`, built from the caller's injected Azure `TokenCredential` and
  explicit account, tenant and OneLake/storage audience selection.
- `OneLakeExecutor`, normally the released `OneLakeHttpExecutor` around the
  existing shared `HttpExecutor::execute_bound`; the caller owns HTTP client,
  timer, deadlines and policy. No library discovers a transport or account.
- `LocalFileSystem`, normally `OsFileSystem::new(root, existing_runtime_handle)`.
  The root is a caller-selected existing directory, not a discovered current
  directory or a sandbox against concurrent hostile namespace replacement.
- `MutationUi`, used only for source notices. These two physical copy handlers
  never call `confirm`, even with force false or an injected denial response.
- Finite `OneLakeLimits`; upload also receives explicit `OneLakeWriteLimits`.
  Pass a caller-owned `Future<Output = Interruption>` for cancellation/deadline.
  The library neither constructs a runtime nor installs a signal handler.

The public future must be polled on the caller's runtime. The host may drop it;
the SDK retains mutation uncertainty where it has an actual failure response,
and dropping running OS work requests cooperative stop, not rollback. A dropped
whole future cannot return an error payload; the host must not report success or
invent remote cleanup. Known cancellation/deadline should be delivered through
the supplied future when the caller needs the returned structured evidence.

`tests/upload.rs::public_physical_copy_futures_can_run_on_the_callers_send_executor`
checks the actual public futures' `Send` contract without polling or dispatching.
The offline example is the complete synthetic HTTP/real-OS composition, not a
production credential sample.

## Inputs, paths and order

`OneLakeWriteTarget` is the resource owner's existing resolved target:
`path` is a validated workspace/item-relative storage resource, `item_type` is
the existing parsed item type, `kind` is resolved metadata rather than filename
inference, and `display_path` is the original host-selected user path. Only use
`Undefined` for a prospective destination whose source context ID is absent.
Do not use it to hide an existing unsupported resource kind.

Local paths preserve the host's native spelling. Relative paths in `OsFileSystem`
are relative to its explicit root; choose that root to reflect the caller's
intended local base. No process-wide `chdir`, tilde expansion, Fabric-first
fallback or token splitting occurs in this crate. An empty native path is
invalid, never shorthand for the configured root or `"."`.

Upload order is exactly: pure writable-root validation; independent native
directory check; independent native file check; released target-name resolution;
progress; bounded binary `read_for_copy`; reject empty bytes; existing upload
helper. Directory/shortcut destinations gain the native source basename; file
and prospective destinations keep their selected name. `LocalReadFile` must
hold a real source disposal guard until create/append/flush returns or fails.
Adapters without this capability explicitly return `SourceLeaseUnavailable`;
substituting an early-closing byte read is not compatible.

Download first branches on source kind. A file checks whether the native target
is a directory, joining only its native basename if so; then progress, bounded
remote GET, nonempty-byte validation, native binary create/truncate/write, Done.
Folder/shortcut/table sources reject recursive copy before all local/remote I/O;
undefined source preserves the original no-op. There is no recursive traversal,
implicit local parent creation, text encoding, JSON conversion or newline change.

Every local method invocation is deferred until the supplied interruption has
been polled: an eager trait implementation must not do I/O before a ready
cancellation wins. Metadata errors Python suppresses are retained separately as
`inspection_errors`; they do not fabricate file existence.

## Rendering and reconciliation

Use `CopyResult.outputs`/`UploadResult.outputs` with the existing output layer.
Use `CopyError::output()` for the source-shaped error envelope and retain the
original `CopyError`, never only its formatted string:

| Evidence | Meaning |
| --- | --- |
| `local_path`, `storage_path` | Exact selected native path and storage resource IDs; sensitive data, not authorization or safe global-log fields |
| `CopyStage` | Validation, local read, storage read, local write or storage write boundary |
| `CopyErrorKind::Upload` | Original `OneLakeWriteError` with unchanged SDK completed steps, possible mutation and caller interruption |
| `local_write_may_have_occurred` | Conservative local-side-effect warning, not proof of a write or permission to erase a path |
| `LocalError.destination_opened`, `bytes_written` | Actual known truncate/open and successful local byte-write evidence |
| `UploadResult.upload` | Real create/append/flush responses; create other than 201 stops without append/flush or Done |
| `CopyResult.bytes_written` | Actual complete local write count; `None` for the source no-op, never an existence-success claim |

The source may print Done after accepted append/flush responses. Preserve that
text while keeping `OneLakeWriteOutcome::Accepted` distinct from completion.
No retry, delete, backup, rename, automatic rollback or later clean-up of a
partially modified target is implied. In particular, never delete unrelated
files or an existing remote destination after an uncertain mutation.

## Explicit compatibility limits

The README and [source-branch matrix](SOURCE-BRANCHES.md) distinguish implemented
physical handlers from host-owned and source-rejected branches. Finite upload
payload bounds (caller limit and released SDK 4 MiB), strict SDK GET 200/201
status handling, validated storage paths, invalid-Unicode source names, explicit
local boundary and native basename guards are recorded safety limits, not
unqualified Python parity. Native OS diagnostic suffixes can differ, but source
codes/prefixes, request order, outputs and actual file contents are compared.
Windows evidence does not establish native Linux/macOS or live service parity.

## Post-checkpoint evidence boundary

The original handler release is `f048914e72b200c63e46b1071a96ecfeaa7893f8`;
exact shared `61d77e3` was safely merged as
`e7f17e64716fd24c0a8ae14484e7afc46af346d5`. Receipt12 is the pre-merge
physical-handler evidence, not proof of the later shared dependency graph.
The post-merge audit identified and fixed the empty-path/root reinterpretation
bug in the native adapter. Private receipt14 completed after the merge: the
native empty-path no-write regression, download GET-then-invalid-write regression,
and actual public-future Send contract passed, as did three original-source/OS
empty-path comparisons, strict targeted Clippy, formatting and the rebuilt public
consumer. No earlier passing test suite was rerun or substituted for that focused
evidence. Attempt13 remains recorded as a refused slot with no validation started;
the old contention and preflight blockers are resolved. This is not a full
post-merge workspace, native-other-platform, central-host or live-service claim.

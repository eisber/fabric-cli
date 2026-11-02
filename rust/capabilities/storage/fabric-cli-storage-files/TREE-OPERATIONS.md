<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Additive recursive physical operations

The owner explicitly authorized recursive physical upload, download, copy and
move beyond the Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` contract.
This is an intentional behavior extension, not a claim that Python supported
these branches. The immutable source fixtures and original `StorageFilesHost`
and `ResourceFileHost::execute` oracle adapters remain unchanged.

The production native `cp`/`copy` and `mv`/`move` dispatcher selects
`ResourceFileHost::execute_full_resolved`. It uses the same context resolution,
injected OneLake client, paginated storage SDK, HTTP executor and runtime as the
other native families. There is no Python file-command fallback.

## Operation matrix

All combinations below accept a single regular file. Directory copies require
`--recursive`/`-r`; directory moves include the whole tree even without `-r`.
Nested and empty directories and zero-byte files are preserved.

| Source | Destination | Copy | Move |
| --- | --- | --- | --- |
| Local file/directory | Local file/directory | Bounded local copy | Copy every entry, verify source snapshot, then unlink copied files and empty directories |
| Local file/directory | OneLake file/directory | Conditional directory creation and binary file upload | Copy every entry through a completed flush, verify local snapshot, then remove copied local entries |
| OneLake file/directory | Local file/directory | Paginated source listing and binary local writes | Commit every local write, verify complete source listing and ETags, then conditionally delete individual remote entries |
| OneLake file/directory | OneLake file/directory | Paginated source listing and binary upload | Same item without a directory merge: checked service rename after full preflight; merges and cross-item/workspace moves: committed copy then checked individual source deletion |

An existing directory destination appends the source basename for both files
and directories. A missing destination or existing file is the exact target.
Native aliases retain the requested basename spelling; canonical paths are used
for identity and containment, not to silently rename case aliases.
Directory-to-file and file-to-directory-leaf collisions fail explicitly, even
with force. Existing directories can merge without force only when no file
would be overwritten; unrelated destination children remain untouched.

Every selected source entry, including implicit/empty directories, must also
have a distinct native destination mapping before any destination effects.
`--force` cannot waive `AmbiguousDestination` or authorize one selected source
to replace another. Existing aliases are resolved through the selected
filesystem's canonical-path capability. Missing tails are appended to their
nearest existing canonical ancestor without creating anything.
On Windows, comparison keys additionally apply Unicode lowercase followed by
uppercase to the complete canonical/prospective path. This catches ordinary
case aliases in files, directory names and missing recursive destinations;
only comparison keys change, never requested output spellings.
This is deliberately conservative, not an emulation of every volume's case
table, case-sensitive-directory setting, normalization or alias behavior.
Expanded Unicode case mappings or case-sensitive Windows directories can
therefore produce an explicit ambiguity rejection rather than a silent rename,
skip or overwrite. Non-Windows checks compare canonical paths exactly; these
checks do not establish complete prospective-alias equivalence on every
case-insensitive or normalization-aware Unix filesystem.

`--block_on_path_collision` rejects any existing planned target, including an
existing directory, and takes precedence over force.
The owner's explicit additive policy requires complete bounded/paginated source
and destination preflight before any target creation or write. Any planned file
overwrite fails with `AlreadyExists` unless `--force` is present; force overwrites
the file, never silently skips it. This applies to single-file and recursive
physical transfers and intentionally differs from the retained Python oracle.
Moves require confirmation unless forced, but only after collision preflight
has succeeded. Confirmation never substitutes for `--force` and cannot authorize
a file overwrite. Decline or failed UI delivery performs no mutation.
New files and directories use exclusive local creation or remote `If-None-Match: *`, preventing
an unnoticed race from overwriting an unapproved target.
Existing destination entries are checked again before use; changed kinds,
links, failed exclusive creates and service preconditions stop explicitly.
Checked renames protect missing targets with `If-None-Match: *`, revalidate the
source snapshot and endpoint kinds, and never replace a directory merge with a
destructive whole-directory rename.

Same-path moves are successful no-ops; same-path copies fail before writing.
Ancestor/descendant overlap is rejected before traversal or mutation. Native
canonical paths, not only alias spelling, are compared. Remote paths are
compared within the selected workspace/item; Unicode rename headers use encoded
path segments. Metadata/service failures never become permission for local
fallback or an empty source tree.
Prospective local destinations use the actual captured writable parent, including
bare relative names. The Python empty-dirname/boolean-file-descriptor existence
quirk remains only in the independently tested baseline resolver contract.

## Roots, traversal and bounds

Local metadata captures two independently selected endpoint capabilities.
`SelectedFileSystem::with_tree` explicitly authorizes descendants of one
selection; `RoutedFileSystems` routes without attempting another capability
after an error. Construction does not enumerate or create a tree. Blocking
filesystem work uses the caller's existing Tokio blocking pool.

Source snapshots include the root and every empty directory. Walks enforce the
configured entry bound and a 256-component depth bound while enumerating.
Local source links, Windows reparse points and special files are rejected.
Destination canonical ancestors and native basenames must remain within the
selected root. Windows device/stream names and trailing-dot/space aliases are
rejected instead of discarding bytes into a device or silently renaming a file.
Remote listings must remain beneath the exact selected
item/directory, contain no duplicate/conflicting paths, and cannot traverse
shortcuts or tables. Missing intermediate directory entries in a valid recursive
listing are materialized as directories, subject to the same entry bound.
Explicit single-file copies into shortcut destinations remain supported; a
shortcut is not treated as a recursively traversable destination.

Remote requests use the existing page/path/response limits.

The production tree adapter uses `LocalFileReader`/`LocalFileWriter` leases,
not the oracle's whole-file `LocalReadFile` API. Each read, write and storage
request is at most 4 MiB (or a smaller caller limit); file length is not capped
at 4 MiB. Native metadata is checked when opening and reading each source range.
Remote HEAD captures a strong ETag and exact length; SDK ranges require HTTP 206,
matching content length/range and `If-Match`. Empty files need HEAD but no GET.
Incremental uploads use `If-None-Match: *` for new files or the preflight
destination ETag with `If-Match` for forced replacement. Positioned appends
are sequential and bounded. Only the final HTTP 200 flush with the declared
total length establishes completion, including zero-byte uploads.

Missing parents outside the selected destination root are not implicitly
created. No temporary file or automatic backup is used.
Windows source handles deny data writes while copying and during final
verification. Approved native overwrites unlink only the selected destination
name and then create a new file exclusively: they never truncate its old file
identity. This protects every source entry from cross-entry hard-link aliases
on Windows and Unix, including aliases to sources not yet opened. A failure
after unlink may leave the destination missing or partially written; it never
implies an atomic replacement or restoration of the previous destination.
Source version metadata must be available before a
copy-then-delete move is permitted.
Portable safe filesystem checks are not a hostile-process namespace sandbox
against concurrent path/hard-link substitution.

## Commit, cancellation and failure

Every file copy must commit before any copy-then-delete move source is removed.
Remote upload commit requires create 201 and a completed flush; append 202 is
the normal storage acknowledgement, but a flush/create/rename 202 remains
incomplete. A move never deletes a source after merely accepted target work.

After copying, the complete source snapshot is compared again. Native files
are checked immediately before unlinking, including a bounded content
fingerprint check using incremental bounded reads so coarse or restored modification times are not the sole
guard. The verification source lease remains alive through unlinking.
Directories are removed nonrecursively,
so a newly added uncopied child is not recursively erased. Remote files require
the HEAD ETag pinned to every actual range GET. Remote directories require checked
directory properties and ETags before deletion begins. Each remote deletion
uses `If-Match` and `recursive=false`; newly added children cause a conflict
instead of being silently erased. Missing ETags or changed source metadata stop
cleanup explicitly.

`TreeResult` retains native writes/directory creation/deletion, actual remote
directory/upload/delete responses and rename acceptance. Upload receipts retain
the last acknowledged append, acknowledged byte position and chunk count; they
do not accumulate a file-sized list of chunks or mistake append acceptance for
completion. `destination_copy_complete` is set only after every file flushes.
`TreeError` retains
the original typed cause, phase, complete earlier receipt and possible local
write/source-delete effects. Active source/destination paths remain structured
reconciliation data, not automatic error/log text. Errors during cleanup can leave partially removed
sources and a complete destination. The actual CLI diagnostic includes the
phase, complete/incomplete destination copy, confirmed partial source-removal
count and active cleanup/write uncertainty, without raw endpoint paths or
service bodies. The underlying typed cause and private reconciliation paths
remain in structured evidence. No rollback is claimed.
An accepted or uncertain destructive operation is never retried, replaced by
copy/delete, or treated as complete. Output failure never replays a transfer.
The native file arm listens for Ctrl+C; library callers inject cancellation.

OneLake `mkdir` and `rm`/`del` remain separate production handlers. Directory
removal is service-side recursive; shortcut removal deletes Fabric metadata,
not the underlying shortcut data. No local `rm` behavior is added by this copy
and move extension.

## Offline validation

`tests/recursive_operations.rs` exercises additive library behavior using actual
local files and injected storage HTTP. Root argv tests in
`hosts/cli/fabric-cli/tests/native_resource_files.rs` exercise the production
family adapter, both explicit local roots and actual SDK request stages.
The production matrix copies and moves files larger than 4 MiB across all four
endpoint pairs; regressions also cover cross-entry hard links, incremental
fingerprints, cancellation, partial native writes, accepted flush/deletion and
rendered cleanup diagnostics.
The source-reference scripts continue to validate the unchanged Python oracle
separately. Tests do not obtain credentials or mutate live storage.

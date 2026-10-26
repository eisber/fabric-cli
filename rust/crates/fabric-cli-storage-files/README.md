<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Physical local/OneLake copies

CLI-layer orchestration pinned to Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, on Rust checkpoint
`e850b32437e0e9171e8f097bfbddb93d57986c3f`. The released OneLake prerequisites
are `05d18f20`, `6a81e4e7`, `53892085`, `80f38f0c` in that order; the shared HTTP
prerequisite `ef17b809` is already an ancestor of the checkpoint.
The lane additionally merged exact shared checkpoint
`61d77e330be8a13cbd5033713f1f2badcecfce60` without changing the pinned Python
source or dropping the released physical-copy commits.
The additional exact released preflight API delta is
`713501b969b71ee8a6d7750a3ce702f92afb2d3d`; it exposes the existing destination
policies without changing or duplicating them.

`StorageFilesHost::{download,upload}` compose an explicit `OneLakeClient`,
`OneLakeExecutor`, `LocalFileSystem`, and caller-owned notices. `OsFileSystem`
uses a supplied existing Tokio runtime handle's blocking pool, not a new runtime
or blocking I/O on an async worker. Credentials/account/tenant/storage audience,
transport, timer, output, and cancellation/deadline are all caller-owned.
There is no CLI parser, ambient identity, logger, process exit, prompt, new HTTP,
retry, paging, or operation engine. Definition export/import is a different lane.
See the [central-host contract](HOST-CONTRACT.md) for exact dependency, dispatch,
lifetime, rendering and reconciliation requirements, and the
[physical source-branch matrix](SOURCE-BRANCHES.md) for implemented,
source-rejected, explicitly bounded and host-owned branches.

The public `offline_copy` example exercises this exact library -> focused SDK ->
`OneLakeHttpExecutor` -> shared `execute_bound` -> synthetic Azure HTTP chain and
real OS fixtures. It consumes one JSON object per stdin line; `root` is a required
existing disposable directory. It never calls a service or acquires real tokens.
Rows default to download; `"direction":"upload"` copies the local `target`
fixture to the remote resource selected by `source`, `kind`, and `item_type`.
The development-only `tests/source_reference.py` executes immutable Python blobs,
including the original dispatch, local handler and local path predicates, against
the same real OS fixture contents. It does not import the installed product.

## Source behavior and explicit bounds

- A destination directory selects the remote file's basename using the native
  platform path rules. A non-directory destination is used exactly as supplied.
  Existing files are binary-truncated/overwritten, without a confirmation.
- Upload first validates the writable OneLake root, then separately tests local
  directory/file predicates, resolves destination naming, emits progress, reads
  binary content and invokes the released upload helper. OneLake directories
  and shortcuts gain the native source basename; existing files/prospective paths
  retain their name. There is no extra existence request or overwrite condition.
  MIME inference remains the helper's case-sensitive destination suffix policy.
- Upload retains a live native source handle through create/append/flush and
  failure. Create must return exactly 201 before append/flush. Source "Done" is
  preserved separately from actual response evidence: accepted append/flush
  statuses remain accepted, never fabricated completion. A later fault retains
  every SDK-received stage and uncertainty; no remote delete or replay occurs.
- `--force`, `--recursive`, `--block_on_path_collision`, interactive denial and
  command-line mode do not change the physical file handlers. The parser's
  recursive option is only for workspace/metadata-folder copies. No filesystem
  traversal is implemented: both local directories and OneLake
  folders/shortcuts/tables are source-rejected combinations.
- GET bytes, not `Content-Length`, determine download emptiness. Empty bytes
  fail before opening/truncating the destination. No encoding/JSON/newline
  transformation, mkdir, temporary file, backup, delete, rename or rollback occurs.
- A missing local parent fails after GET. A write failure can leave the existing
  target truncated or partially written. Completed byte counts and whether a
  destination opened remain typed error evidence. No unrelated path is cleaned up.
- Predicate errors that Python suppresses are retained in `inspection_errors`;
  native error suffixes differ between Python `OSError` and Rust `io::Error`.
  Error codes, phase and `Error writing file:`/`Error reading file:` prefixes are
  separate from these platform-specific diagnostics.
- OneLake paths retain the released SDK's fixed-origin and traversal guards,
  file-response bounds, cancellation, and shared HTTP deadline policy.
  Its file-read contract accepts only 200/201: an unexpected GET 202/204 is an
  explicit SDK `InvalidResponse`, unlike Python's raw-body handler. This existing
  strict SDK safety exception is not reported as source status parity.
  Local reads are bounded during accumulation, including files that grow.
  Upload checks the smaller of the caller's request bound and the released SDK's
  4 MiB payload limit before any remote mutation; injected oversized content is
  also rejected. This allocation/request bound is explicit, not source parity.
  Invalid-Unicode native file names fail explicitly rather than lossy-converting
  a storage destination. The context host must classify `Undefined` destinations
  as prospective (source ID absent), as the shared target contract requires.
  A short successful injected write is an error, never "Done".
- The real adapter requires an explicit local directory boundary. It resolves
  existing ancestors/symlinks before opening and rejects paths escaping it.
  Directory basename joins reject native prefixes/drive/ADS injection. This
  explicit safety bound differs from unrestricted Python file access. It is not
  a sandbox against concurrent symlink/hard-link replacement. Callers must own
  the fixture/target tree and not mutate its namespace concurrently.
  Empty native paths fail instead of being reinterpreted as that local root;
  explicit `"."` retains its native directory meaning.
- Dropping/cancelling a blocking operation requests cooperative stop between
  bounded chunks. A running native system call can finish later; interruption
  does not guarantee rollback, immediate handle disposal or bounded cleanup.
  Error evidence conservatively retains possible local effects. Shared HTTP
  remains responsible for network cancellation/deadlines; 202 is not completion.

## Source-branch ledger

Paths below are relative to `src/fabric_cli/` at the pinned Python commit.
Assigned rows remain distinct from central parser/context and other owners.

| Source / lines | Resource, options, error, mode | Implementation / offline evidence | Gap |
| --- | --- | --- | --- |
| `commands/fs/fab_fs.py:95-99 cp_command`, `:215-255 extract_from_to_paths` | cp/copy context extraction, token splitting, command-line/support checks | Source traced; local_path and OneLake are supported source types | Central host/resolver owner; not duplicated here |
| `core/fab_handle_context.py:43-114 get_command_context` | Fabric-first local fallback, readable existing source, prospective target parent | Source traced; local adapter accepts already-resolved paths | Parent truthiness/descriptor oddity belongs to context owner; not silently corrected |
| `parsers/fab_fs_parser.py:205-239` | cp/copy, force, recursive, path collision | Original file handlers do not consult these flags | Central parser wiring |
| `commands/fs/fab_fs_cp.py:56-61 exec_command` | Local->OneLake and OneLake->local dispatch | Original dispatch used by the independent oracle | Central front-door composition |
| `commands/fs/fab_fs_cp.py:64-72 exec_command` | Local->local and mismatched resource rejection | Source traced; no new local-local route | Host dispatch rejection, not a native file-copy fallback |
| `core/hiearchy/fab_local_path.py:11-40 LocalPath` | Native basename, isfile/isdir, symlink following | Real native predicates/paths; source predicates executed independently | Native platform execution beyond Windows remains a gate |
| `commands/fs/cp/fab_fs_cp_onelake.py:48-64 copy_local_2_onelake` | Writable destination first; local directory recursive rejection; missing/non-file source | Public upload uses released pure preflight; two native predicates; order recorded at real adapter boundary and independently source-compared | Main host only |
| Same function `:66-87`; `utils/fab_cmd_cp_utils.py:40-58 upload_file_onelake,68-103 destination/check,107-116 _infer_content_type` | Target naming, binary read, empty source, read error, upload status/partial stages, MIME case | Public upload retains actual native handle through released SDK/shared HTTP stages; read-fault/empty/naming/status/failure/cancellation source cases | Native diagnostic suffixes and explicit allocation/Unicode/path safety limits |
| `commands/fs/cp/fab_fs_cp_onelake.py:90-106 copy_onelake_2_local` | File->local destination naming/progress/GET | Implemented public download; real OS/shared HTTP consumer and source comparison | Main host only |
| Same function `:107-122` | Empty source, binary overwrite, missing parent, write fault, Done | Implemented; existing/new/directory/collision fixtures, partial-fault bytes and unrelated-file preservation | Native diagnostic prose remains OS-specific |
| Same function `:124-127` | Folder/shortcut/table recursive rejection | Implemented before any local/remote I/O, regardless of options | None in handler |
| Same function `:93-127` match without default | Undefined source silently unhandled | Preserved no-op without GET/write/output | Not an existence-success claim |
| `client/fab_api_onelake.py:get` | Storage GET, audience and errors | Released SDK + bound shared HTTP; offline 401/403/404/byte-bound checks | Live service parity is not claimed |
| `client/fab_api_onelake.py:touch_file,append_file,flush_file` | Storage PUT/PATCH, media type, byte position, permissions, accepted stages | Existing SDK/helper through shared HTTP; actual source comparison covers 401/403/404 at all stages and 200/202/204 append/flush combinations | No live service or mutation-completion claim |

## Focused validation

From `rust`, within an authorized shared-host validation lease:

```text
cargo test --offline --locked -p fabric-cli-storage-files --all-targets
cargo test --offline --locked -p fabric-cli-storage-files --doc
cargo clippy --offline --locked -p fabric-cli-storage-files --all-targets -- -D warnings
cargo test --offline --locked -p fabric-cli --test workspace_contract
cargo build --offline --locked -p fabric-cli-storage-files --example offline_copy
python crates\fabric-cli-storage-files\tests\source_reference.py --probe <target>\debug\examples\offline_copy.exe --fixtures <private-fixture-parent>
```

On other platforms use the native executable name/path. Local lane checks set
`FABRIC_STORAGE_FILES_FIXTURES` to the session's private artifact directory; tests
create and remove only uniquely named child fixtures they created. CI may use its
own OS temporary directory. Existing oracles/expectations are not changed.

First download checkpoint evidence: Rust 1.99.1, `x86_64-pc-windows-msvc`; 12 real
CLI/SDK/shared-HTTP/OS tests and 8 architecture tests passed with strict targeted
Clippy and formatting. The source oracle passed 38 comparisons plus 2 separately
asserted SDK status-safety exceptions (not counted as source parity).
The first Python comparison attempt lacked the standalone
example binary (test-harness builds are not executable consumer builds); it was
corrected by explicitly building that example before running the source oracle.
That download checkpoint run is private mutex receipt `receipt-07.json`, with
`validation-06.log` and `source-06.log`; these accompany the exact-commit handoff.
No native
Linux/macOS, live authentication/service call, central dispatch or full-command
parity is inferred from these results.

The additive `tests/local_adapter.rs` covers exact byte bounds across the 16 KiB
native chunk size, missing/non-file sources, explicit-root relative paths,
unpolled-future disposal, native exclusive-file sharing failures on Windows, and
no-parent-creation/truncation behavior. The source comparison additionally probes
an actual read-only Windows destination; only the native diagnostic suffix is
normalized, not the source error code, stage, requests, output or file contents.

`LocalFileSystem::read_for_copy` returns `LocalReadFile`, retaining its native
source handle until the complete upload finishes or fails, matching the pinned
Python `with open` lifetime. The real adapter shares its bounded binary-read
implementation with `read`; injected adapters that cannot retain a handle fail
explicitly with `SourceLeaseUnavailable` rather than silently closing early.

`tests/upload.rs` exercises actual file reads, both preflight error-order
boundaries, directory/missing-source rejection, naming/MIME behavior, empty/read
fault/bounds, create-only statuses, and every remote lost-response/cancellation
stage. Windows HTTP callbacks prove the source handle is held through mutation
dispatch and released after failure/cancellation. Source comparisons use the
unchanged original local->OneLake dispatch/handler/helpers and real source files;
the released preflight's separate historical 400-case oracle is not counted as
this crate's physical transfer proof.

## Completed physical-transfer checkpoint

Private mutex `receipt-12.json` completed on the same Windows toolchain with
32 own-crate Rust tests (12 download, 8 local adapter, 12 upload), 8 architecture
tests and 9 released preflight/write regression tests. Strict targeted Clippy,
formatting, documentation compilation, and the standalone public example passed.
`source-12.log` records 117 unchanged-source/real-OS comparisons, including
78 physical uploads and 39 downloads, plus 2 separately asserted existing SDK
GET-status safety differences. `validation-12.log` retains native exits/output.
The earlier `receipt-10.json` cancellation regression is retained: injected local
methods must be invoked inside the interrupt-polled async block, not eagerly
constructed before cancellation is checked. Its expectation was not weakened.

The physical handler branches are implemented within the listed explicit bounds.
Central command/parser dispatch, Fabric-first path resolution and multi-token
splitting stay with their assigned host/context owners; there is deliberately no
local->local fallback, directory traversal feature, definition export/import,
production identity/transport host, cross-platform or live-service parity claim.

Post-merge `receipt-14.json` separately completed three targeted Rust cases and
three pinned-source/real-OS empty-path comparisons, plus strict targeted Clippy,
formatting and the rebuilt public consumer. Empty native paths now fail instead
of selecting the configured root; the actual public upload/download futures
also satisfy the caller's `Send` executor contract. The historical full
receipt12 suite was not rerun and is not represented as a new full workspace pass.

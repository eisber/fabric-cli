<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Definition transfer: bounded native library contract

Pinned destination-policy clarification (`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`):
SparkJobDefinition/Resources is **not** a remote export destination. The additive
`definition_export_destination_source.json` independently executes the actual
item-folder enums/map, `Item.get_folders`, `_handle_path_in_item`,
`get_export_path`, `export_single_item`, decoder and storage dispatcher. SJD root
folders are exactly Libs/Main/Snapshots; Resources fails context resolution, then
fails with `InvalidPath` when no same-spelled local directory exists. Recognized
SJD Libs/Main contexts fail with source `NotSupported` ("Only supported for
Lakehouse/Files"). `_write_to_sjd` existing elsewhere does not make it reachable
from this export selector. A same-spelled existing Resources directory is a
genuine **local** fallback, not SJD remote support.

The new root SDK/filesystem consumer mirrors those decisions: one definition
POST, no OneLake storage requests/scopes for rejected SJD outputs, and actual local
bytes for the legitimate fallback. It also retains the existing positive
Lakehouse create/append/flush consumers. These Rust cases remain **NOT RUN**.

Status: the bounded read/export/copy/SET codec library is compiled and tested on
Windows x64. The exact shared Unicode encoder
commit `d72d68b5370ab5ab95c63815ba084513390d68e4` was surgically adopted as
`bbed590e1477af8a79a5dcef6a155026525e0329`; it shares the existing engine and
preserves the default ASCII APIs. Exact cancellation commit
`09c939ba075d6e9fc3142b3ee801344f961208fd` was adopted as
`0e8ec5c896060e9bd7ce189efe5f6f52e025111e`, keeping its four additive tests and
all existing baseline tests. Reviewed gate-v2 `native-receipt-2.json` records
24 definition consumers, 24 unchanged Notebook consumers, 13 shared-operation
tests, five shared-encoder tests and three doctests passing, followed by targeted
Clippy with `-D warnings` and formatting checks. No expected source outputs,
skips or lint waivers were changed. This is native offline library evidence,
not integrated CLI, live Fabric or full command parity.

The additive export command parity work on `parity/export-bulk-20261028` starts
at integration `0e4b0759bd8c3e933250a6f174d3711fcbbdde50`. Its
`decode_export_files_for_storage`, `export_item_for_storage`, and
`bulk_export_source` APIs preserve Python's malformed per-part JSON/base64
fallback, non-string payload storage and local versus Lakehouse serialization.
Existing bounded codec APIs and protected receipts remain unchanged. Actual
pinned-source fallback/remote captures are additive in
`definition_export_fallback_source.json`, captured before Rust expectations.
The central transfer metadata now builds a complete validated folder hierarchy,
uses cached source item order for workspace execution, configuration sorting
only for selection, and folder-first depth-first order for Selective requests.
Unknown future item types are filtered; absent types and invalid/missing item
parents fail rather than producing a partial snapshot. Graph, portable-path,
strict-decoding and containment safety policies are retained.

The first microbatch is `980da06d8eb96bbb5d83ccf424b51beed4d65c0b`.
The follow-up mounts the existing caller-injected OneLake SDK in the central
`FamilySession` transfer branch, without changing native host construction,
dependencies, identity, HTTP policy or the import adapter. Local outputs delegate
to the existing asynchronous filesystem worker. Single-item metadata/definition
reads precede destination resolution; workspace destination validation precedes
selection and its execution retains cached item order. The existing resolver
also preserves relative paths from OneLake current roots.
Metadata's additive `resolve_target`/`complete_snapshot` phases preserve that
warning/precondition order without breaking already-complete injected snapshots.
Folder collection reuses the resolver's validated command-local folder graph;
it does not fetch another hierarchy, install a TTL cache or duplicate paging.
Before returning Export/Import targets, the central resolver applies the existing
source support policy to every target kind. Unsupported workspace/folder imports,
folder export and explicitly unsupported item types fail with the real
`UnsupportedCommand`, not an item-only adapter placeholder. The policy does not
require an item ID: a supported missing import item retains its known workspace/
folder parent and creates through the ordinary source path.
The export wrapper belongs only in `Invocation::Transfer` for Export/BulkExport.
Import and Deploy must receive their original async file capability unchanged;
in particular, deployment's `read_async` and `prepare_repository_async` must not
fall back through an export-only wrapper.

Export/bulk operation sessions borrow `reads.sdk`'s existing request context via
`OperationSession::with_context(self.client.context(), ...)`, not a freshly
constructed public-origin context. The caller must inject a coherent selected
`Client::with_context` and `HttpExecutor::with_endpoint` pair. Initial metadata,
continuation pages, definition POST, polling and result retrieval all remain on
the selected public/daily/MSIT Fabric origin. Foreign poll/result links fail
closed without a public-host fallback or replay. Storage still uses the existing
explicit OneLake adapter/audience, not a second Fabric endpoint or credential.
The new offline consumer covers all three Fabric selections and rejected foreign
poll/result links; these Rust cases remain **NOT RUN** pending ROOT validation.
Native MSIT dispatch restrictions and native endpoint/console construction remain
ROOT-owned and are not changed here.

`export_read_item` separates an already-completed definition read from output
resolution without replaying the POST. Source storage decoding uses the shared
native Python-value encoder for wide integers, nonfinite floats, duplicate keys,
Unicode and Notebook cleanup. The private export parser does not replace service
JSON or add another encoder. Shared native depth/node/string/digit/output limits
remain explicit safety bounds. The original bounded codec and consumers retain
their protected contracts.

Lakehouse export checks actual create/append/flush responses and retains original
SDK mutation errors. The additive SDK `upload_export_file` uses the existing upload engine,
omits empty appends, and preserves Python's character-count flush position for
ordinary Unicode text (even though UTF-8 byte length differs). That source oddity
is captured independently rather than silently repaired. Accepted-only create
or flush is an explicit incomplete failure, never an exported-success fallback.
No upload, failed file, writer or uncertain request is replayed or rolled back.
Bulk result evidence retains exported/skipped IDs in source index/selection
order; workspace failure retains previously exported item IDs and files.
The source's last-by-ID response lookup and original-object missing-item pass
remain distinct for duplicate cached IDs, including type-count insertion order.

Python bulk-export explicitly accepts only an existing local output directory;
this validation is not a missing remote bulk branch. Permissive/noncanonical
base64, traversal/aliases/symlinks/collisions, invalid graphs and shared finite
allocation bounds remain intentional safety exceptions. Unrepresentable local
Unicode and invalid remote scalar payloads fail typed validation rather than
substituting bytes or claiming success. No injected UI is replaced with consent,
a fabricated display capability or an operator/live call.
Python's broad destination-resolution exception handler is narrowed deliberately:
an authentication/service failure cannot become a successful export to a
coincidentally named local directory. Ordinary local path-resolution fallback
is retained, and the original failure still fails closed.

These changes are not a Rust validation receipt: tests/builds are **NOT RUN**
pending the integrator's serialized grant. Eight additive actual-source captures
reproduce independently before Rust expectations; the new offline argv consumer
covers generic/default/explicit formats, workspace selection/execution ordering,
nested folder Selective requests, workspace response-index filtering, malformed
JSON fallback, binary data, later-page errors, accepted/poll/result completion,
partial local/remote writes, cancellation, writer failure, empty remote uploads,
returned IDs and non-replay. All new filesystem roots are caller-confined project
directories; no service, authentication profile or token cache is touched.
The added bulk-precondition receipt independently captures missing recursive/
empty output errors, remote-output rejection, both source decline questions,
explicit confirmation and nonempty-force warnings. Root consumers compare those
exact messages/events and require decline before item discovery, while retaining
existing files. A nested Fabric-folder item also exercises source-ordered JSON
and raw binary parts through a nested Lakehouse folder, including real metadata
HEAD boundaries and create/append/flush requests with no local spill.
These are injected functional stream/workflow cases, not native keypress,
questionary styling or TTY-byte parity claims. ROOT's separately owned native
console checkpoint is not copied into this branch.
The workflow consumers are module-level registered tests; the consumer now denies
`unnameable_test_items` so accidental nesting is a compile error, not silent
coverage. Rust execution is still required; formatting/static registration checks
are not test-pass evidence.

The public metadata-only `families::validate_local_import_input` seam reuses the
selected SDK/resolver and explicit OneLake metadata adapter; it never downloads
bytes, probes native files, prompts or acquires ambient credentials. Its new
actual-source receipt executes `get_import_path`, `import_single_item`, common
error leaves and the real exception normalizer on injected synthetic metadata.
Local regular-file/directory candidates are deferred to the file worker; known
remote/Fabric inputs fail with the captured source message before file I/O/UI.
Direct consumers cover selected MSIT metadata versus public OneLake HEAD,
authentication failure without local fallback and pre-discovery cancellation.

The central `TransferTargets::validate_import_input` override now delegates to
that seam. Only the exact default trait declaration/docs are adopted from import
commit `d9c51de23877c7c047d42f3528c2098c8b308c11`, after SDK peer coordination;
ROOT must deduplicate that identical declaration when unioning both lanes.
The import function itself is untouched here: the actual after-format/before-
filesystem invocation belongs to that import commit, following
`18c1a86e73674f14e6c0c92f370d32e41130ab78`. ROOT must still integrate those
calls and file-aware/environment worker bindings. Until that union is validated,
these direct helper cases are not a front-door import completion claim.

Do not silently reparent a single requested item from a raw `folderId`:
pinned `fab_handle_context.py` constructs workspace-qualified items with the
requested workspace parent (lines 507–508), and explicitly folder-qualified
items with the requested folder parent (lines 560–561). The complete workspace/
folder discovery snapshot already uses the raw service `folderId` and validated
folder graph. Those distinct source paths remain distinct; changing generic
`Snapshot`/read-parent behavior requires separate source evidence and review.

One native-only mount remains assigned to the integrator: the binary currently
constructs its local adapter with CWD and no explicit home. For user-selected
existing local export directories outside CWD (including explicit `~` expansion),
resolve that caller-owned root on the native filesystem worker and use
`TokioTransferFiles::for_export_root(root, original_output, limits, runtime)`.
The original argument spelling remains the progress path while every write is
confined to that selected root. The new argv consumer verifies a selected physical
root outside the current root through real async filesystem writes. Import
construction/preparation and remote Fabric destination resolution are unchanged.
No native host file is edited in this lane.

From `rust/`, serialized validation selectors for the integrator are:

```text
python -B tests/compatibility/run_export_source_parity.py --artifacts <new-directory-inside-checkout>
cargo test --offline --locked -p fabric-deploy --test export_parity --test definitions
cargo test --offline --locked -p fabric-cli-transfer
cargo test --offline --locked -p fabric-cli --features native-read --test export_parity_mount --test family_mounts --test async_transfer_files
cargo clippy --offline --locked -p fabric-deploy -p fabric-cli-transfer -p fabric-sdk-onelake -p fabric-cli --features fabric-cli/native-read --all-targets -- -D warnings
```

Python reference: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Rust integration base: `61d77e330be8a13cbd5033713f1f2badcecfce60`.
The committed `fabric-cli-definitions` candidate at
`b245a05da0a08651369e486accc47aa017f46d56` supplies prior art for portable path
validation, local filesystem preflight, decoding and ordered partial-write
evidence. Its separate HTTP/polling/JSON implementations and competing central
host are not adopted.

The additive API lives in `fabric_deploy::definitions`. It neither replaces the
existing single-Notebook publisher nor installs a CLI parser, command host,
identity provider, HTTP implementation or runtime. Caller-owned SDK operation
sessions provide credentials, bounded shared HTTP execution, operation
reconciliation and cancellation. Caller-owned filesystem implementations provide
local preflight, directory creation and exact byte writes. Schedule synchronous
filesystem work on an appropriate caller worker; the library does not create one.

## API and source mapping

| Native API | Actual Python source boundary | Contract |
| --- | --- | --- |
| `resolve_format` | `fab_item_util.resolve_definition_format`, `fab_types.definition_format_mapping` | Exact selectors/defaults, no arbitrary query interpolation |
| `read_definition` | `fab_api_item.get_item_definition` | Empty-body POST, optional source format, shared synchronous operation/result policy |
| `read_item_definition` | `fab_api_item.get_item_withdefinition` | Metadata GET before definition POST, ordered object update, explicit 401/403 metadata-only fallback |
| `read_copy_definition` | `fab_fs_cp_item._copy_item_with_definition` then `fab_api_item.get_item_withdefinition` | Explicit empty API format, no Notebook default query; raw outputs retained |
| `decode_export_files` | `fab_cmd_export_utils.decode_payload`, `clean_notebook_cells`; `fab_storage._validate_json`, `_write_to_local` | Service part order, JSON-suffix versus ordinary-text distinction, binary/empty payloads, first-part Notebook output cleanup, native text newlines |
| `export_item` | `fab_fs_export_item.export_single_item` | Explicit prior confirmation, context item directory name, no extra overwrite prompt, ordered writes with exact failure progress |
| `copy_payload` | `fab_fs_cp_item._copy_item_with_definition` | Raw base64 definition, ordered type/name/definition/folder fields, null folder retained, truthy description appended |
| `decode_set_definition`, `ensure_notebook_dependency`, `encode_set_definition` | `fab_cmd_set_utils._decode_payload`, `ensure_notebook_dependency`, `_encode_payload` | SET JSON-decodes every part, does not clean Notebook outputs, initializes dependencies before caller replacement, exact default JSON/base64 encoding |
| `load_import_definition` | `fab_cmd_import_utils._build_definition`, `_encode_file_to_base64` | Injected filesystem, sorted root files before recursive sorted directories, raw bytes, full-path cache exclusion after Report validation |
| `prepare_import`, `PreparedImport::execute` | `fab_fs_import_item.import_single_item`, `_import_create_item`, `_import_update_item` | Explicit caller-resolved target/confirmation, ordered create versus definition-only update body, shared operation policy, actual result ID |
| `bulk_export_payload`, `filter_bulk_definition`, `strip_bulk_parent_prefix`, `bulk_export` | `fab_cmd_bulk_export_utils`, workspace/folder bulk handlers | Exact All/Selective bodies, caller-complete cached support snapshot, workspace response-index filtering, parent-folder stripping, actual shared HTTP/LRO and ordered writes |

SET's restricted explicit-path mutation and friendly-name aliases remain the
resource caller's responsibility. They are not JMESPath projection or deployment
JSONPath. Call `read_definition`, decode, initialize the dependency container,
apply the caller's existing replacement, then encode. The GET and mutation use
separate caller operation sessions: the shared engine conservatively treats a
dispatched POST as potentially mutating and must not be reset/replayed.

Copy calls `read_copy_definition` and consumes `DefinitionRead.item`, not
formatted export files. It deliberately omits the format query even for Notebook
and SparkJobDefinition; export and SET use type-specific source defaults.
Discovery, same-workspace `_copy` naming, item-type support, folder collisions,
overwrite confirmation, actual create/update/delete, and cache reconciliation
are not replaced by the payload builder.

Import preparation is synchronous and injected; it must run on a caller worker
appropriate for local file reads before asynchronous mutation. It does not
reinterpret import as CICD Notebook deployment: no `.platform` requirement,
Notebook cell cleanup, newline normalization or text transform is added.
`DefinitionImportFileSystem` supplies bounded entries/read operations.
`LocalDefinitionFileSystem` implements both import and export adapters for
stable caller-owned directories. Input and encoded request bytes are separately
checked against finite bounds. Source `cache.abf` substring exclusions are
reported, including full-root-path matches; `definition.pbir` byPath validation
still runs before exclusion. Empty directories retain an empty parts array.
Create sends type/folder/name/definition in source order; update sends only
the definition wrapper. Prepared execution consumes the input and never replays
an uncertain mutation. Environment import remains explicitly unsupported before
reads or mutation until readiness/publish reconciliation is ported.

Bulk export accepts a complete immutable `BulkExportSnapshot` from the existing
metadata/context owner, not a second cache/discovery engine. Workspace All sends
no selected IDs, matches returned index IDs against known workspace items,
ignores unknown returned items, and counts known missing items as skipped.
Prefix matching requires the item prefix followed by `/`, excluding textual
siblings. Folder Selective uses the caller's source recursive item order and
does not apply workspace response-index filtering. Source false-return versus
exception support outcomes remain distinct: the folder collector silently omits
a false return but counts its Fabric exception as unsupported; the native result
retains that source distinction rather than inventing a universal support rule.
Parent prefix removal preserves the selected folder and rejects mismatched
ancestors. Bulk Notebook parts retain outputs; only single export cleans them.
The same filesystem checkpoint/write helper preserves cancellation and partial
progress for both single and bulk export.

## Failure and safety boundaries

An HTTP 202 is never a completed export. The existing shared source-synchronous
operation policy owns polling/result retrieval; definition consumers reject a
bare accepted response or unusable result rather than reporting empty success.
Operation failures retain the shared operation ID, structured service data and
dispatch uncertainty. No request is automatically replayed.

Definition 401/403 retains the source metadata fallback as an explicit
`definition_access_denied` value for read consumers. Export does not disguise
that fallback as a successful empty definition. Other service failures propagate.

Local errors retain fully written paths in service order plus a possibly partial
`incomplete_path`. Cancellation between filesystem boundaries retains completed
writes; no file deletion/rollback or automatic write replay occurs. An empty
parts array still creates the item directory, as in Python.

The released candidate's portable path policy rejects traversal, device/ADS
aliases, reparse/symlink/network paths, case-aliased paths and file/directory
collisions. Stable caller-owned local directories are required; this is not an
isolation boundary against hostile concurrent filesystem replacement. Explicit
file/path/decoded/formatted byte bounds are separate from SDK response/request/
poll/deadline bounds. These are safety limits, not unqualified source parity.
These checks intentionally differ from Python's leading-slash stripping and
unrestricted `os.path.join`/ordinary local open behavior. They are recorded safety
exceptions, not a claim that the Python path algorithm enforces the same policy.
Leading slashes are stripped as in source before portable validation. Exact
duplicate paths retain source ordered overwrites; case aliases and file/parent
collisions fail. Empty definitions receive the same checked item-directory
creation policy rather than bypassing alias/link checks.

The local adapter requires an already-resolved root. A caller that explicitly
selects and owns a stable directory resolves that root before invoking import
or export; it must not canonicalize individual service-supplied definition
paths to bypass link checks. Rust's
[`temp_dir`](https://doc.rust-lang.org/std/env/fn.temp_dir.html) may contain a
symbolic link, including system temporary-directory aliases on macOS. The
synthetic native tests therefore canonicalize their own newly created root
before passing it to the adapter. The adapter still rejects unresolved root
aliases, linked directories and linked files. Independent source receipts,
request bytes, ordered outcomes and production path validation are unchanged.

Python's permissive base64 decoder, malformed-suffix-JSON per-part error fallback,
nested SET parts, and Python numeric domains outside the shared JSON value range
remain explicit unsupported branches. They are not silently repaired, treated as
empty, or counted as parity. SET's unusual array/scalar rule is preserved:
non-string/non-object edited payloads are not base64-encoded or relabeled by the
source encoder. Fixing that behavior requires a separate product decision.

## Independent new evidence

`fabric-deploy/tests/definition_export_reference.py` extracts and executes actual
immutable Python leaves without importing product code, authentication or mutable
CLI state. Its twelve synthetic cases capture exact local bytes, including
Unicode/float/null, JSON suffix scalars, raw text JSON, binary/empty files,
CRLF translation and Notebook cleanup. `definition_set_reference.py` captures
six exact decode/encode/request-body cases, including the source array/scalar
oddity. These additive expectations were captured before Rust execution; old
fixtures, oracle pins and guards remain unchanged.

The Windows byte receipt is fixed. Native non-Windows comparison removes only
the source Windows text-write newline translation for text cases, never binary
bytes. Capturing another platform is additional evidence, not permission to
overwrite the existing receipt.

The Rust public consumer composes the actual `OperationSession` and
`HttpExecutor` with a scripted Azure `HttpClient`, then exports to its own
synthetic local directory. It also covers confirmation, invalid selectors/IDs,
definition permission fallback, accepted/result handling, file bounds, partial
writes, cancellation and overwrite/path failures. This is offline library
evidence, not proof of front-door CLI registration, live Fabric, Linux or macOS.

The additive `definition_import_source.json` has seven further actual pinned
source cases, captured before Rust import execution. Its reference script
executes `_build_definition` and `_encode_file_to_base64` on synthetic local
directories; no previous expected output was regenerated. The low-level fixture
format is already resolved, while command preparation separately resolves the
user's item-type format selector. Gate-v2 `import-receipt-2.json` completed at
13:13Z on October 26: all 31 definition/import consumers, 24 unchanged Notebook
consumers, one compiled deployment doctest, strict targeted Clippy, ten workspace
architecture checks and formatting passed. `import-receipt-1.json` retains the
earlier pre-validation refusal for unavailable Python process metadata. The
refused process exited; no foreign process was killed or guard bypassed.

`definition_bulk_reference.py` executes actual pinned request-builder, workspace
index-filter and parent-path functions with synthetic injected cache/support
callbacks. Its fixed receipt includes exact All/Selective request bytes,
known/unknown/missing index behavior, textual-sibling filtering, and four parent
path cases. It was captured before Rust bulk execution. Gate-v2
`bulk-native-receipt-2.json` records all 37 registered definition/import/bulk
consumers plus 24 unchanged Notebook consumers and the deployment doctest
passing, followed by strict Clippy and formatting. The prior bulk lint failure
is retained: an incorrectly nested new test block was moved to module scope.
`#![deny(unnameable_test_items)]` now makes accidental nested test registration a
compile failure even before strict linting. No test was removed, waived or skipped.

The exact `c72860db65e821ada2ec3cbac8f34c4bc7458042` integration candidate
adopts the released definition tree through
`230ab09375f4e741a74050a8fd88f21fa85e61dc` without replaying the shared Unicode
or cancellation adoptions. Its fresh gate-v3 transfer checkpoint passed all
37 definition consumers, 24 unchanged Notebook consumers, 18 existing shared
operation tests, five JSON tests, three doctests, ten architecture tests,
strict targeted Clippy and formatting. All four actual pinned Python captures
matched the immutable receipts in insertion order. This is local Windows x64
evidence, not a three-platform or executable-command completion claim.

Affected native CI selections now require those source captures as well as the
public Rust definition consumers. From `rust/`, run:

```text
python -B -m unittest discover -s tests/compatibility -p test_run_definition_parity.py -v
python -B tests/compatibility/run_definition_parity.py --artifacts <private-artifact-directory>
cargo test --offline --locked -p fabric-deploy --test definitions
```

The source runner writes only private temporary receipts and compares all four
captures without sorting arrays or object keys. Missing inputs, observations,
source pins, subprocess failures and timeouts fail closed. On non-Windows hosts
only the fixed export receipt's documented text newline expansion is removed;
binary bytes, actual observations and committed expectations are never changed.

## Remaining branch matrix

| Surface | Implemented bounded boundary | Still required for full command parity |
| --- | --- | --- |
| Single export | Generic item metadata/definition read, ordered local parts, first-part Notebook cleanup | Full command support dispatch, interactive selection/output/cache integration, remote Lakehouse destination, permissive/error-preserving decode branches |
| Workspace export | Reusable single-item export | Complete discovery, config sorting versus execution order, source selection/confirmation/count/error behavior |
| Bulk export | Exact All/Selective shared HTTP/LRO, injected complete snapshots, workspace index/prefix filtering, ancestor stripping, unchanged Notebook outputs, local writes and partial outcomes | Full context/cache discovery and recursive snapshot assembly by its existing owner, command prompts/output integration, complete resource support dispatch and remote storage |
| Import | Injected deterministic local traversal/cache exclusion/Report checks, create versus definition-only update with actual shared HTTP/LRO/result consumers; existing Notebook deployment unchanged | Full command support/context dispatch, prompt/decline/cache outcomes, Environment readiness/publish lifecycle, remote input error presentation |
| SET | Raw read plus exact decoded/base64/request-body codec | Caller query/friendly aliases, complete update orchestration and command output |
| Copy/move | Raw definition payload contract | Resource-owner discovery/confirm/collision/create/update/delete/cache algorithms and uncertain-mutation handling |
| Deploy | Existing native single-Notebook config/transforms/publish path remains unchanged | Full pinned CICD config/YAML/parameters/types/dependencies/reconciliation/bulk/unpublish and CLI wrapper cleanup |

No remaining branch is waived or counted complete because a helper or fixture
exists. See [deployment compatibility](deployment-compatibility.md) for the
unchanged independent 44-case config/transform oracle and Notebook publisher.

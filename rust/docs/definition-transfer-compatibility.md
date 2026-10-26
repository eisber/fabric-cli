<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Definition transfer: bounded native library contract

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

<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Import command source contract and integration handoff

Source: Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Lane base: `0e4b0759bd8c3e933250a6f174d3711fcbbdde50`.

The existing generic definition importer remains the implementation. Import is
not CICD Notebook deployment: no `.platform` requirement, Notebook cleanup,
newline translation, artifact conversion or source-type fallback is introduced.

## Operation matrix

| Source branch | Implementation and evidence |
| --- | --- |
| Generic create | Ordered type/folder/name/definition body; actual completed ID; existing shared HTTP/LRO; original tests retained |
| Generic update | Definition-only body; warning and injected overwrite confirmation; existing item cache effect even on overwrite decline |
| Known missing target | Create only for the caller's genuinely missing item, with an existing workspace/parent; malformed IDs and unknown parents are not forced success |
| Initial decline / force | Explicit injected confirmation before file loading; decline has no mutation/cache effect; force skips only source questions |
| Environment create | Generic raw definition mutation, actual returned ID, one staging-publish POST, then publish-state completion |
| Environment update | Wait for existing publish to become success/failed/cancelled before definition mutation; publish afterward and require success |
| Environment artifacts | Generic deterministic traversal preserves settings, libraries and multiple binary artifacts without separate upload APIs |
| Environment state | Case-insensitive states; exponential 10/20/40/60-second asynchronous backoff; 60 reads maximum per phase; caller request/delay bounds and cancellation |
| Environment failure | Failed/cancelled publish is failure, never success; completed item ID retained in error effects; no mutation replay or rollback |
| Accepted operations | Definition and staging-publish reuse actual shared operation polling/result retrieval; bare acceptance does not complete import |
| Local directory | Sorted files before sorted depth-first directories, raw bytes, InlineBase64, format mapping and aggregate bounds |
| Existing local file | Actual Python `os.walk(file)` yields empty parts, not the file bytes; low-level local importer now preserves this |
| Report byConnection / null byPath | Raw original bytes, unchanged connection, no implicit rewriting |
| Report non-null byPath | Source `InvalidDefinitionPayload`, before `cache.abf` exclusion; legacy low-level classification retained for existing contracts |
| Invalid scalar/null Report | Typed malformed-definition error; no scalar fallback or successful empty result |
| Cache exclusions | Full source-root/path substring check, with Report validation first and original traversal-order exclusions |
| Folder destination | Import consumes the real caller-resolved parent folder ID; shared metadata owner must provide complete folder ancestry |
| Remote OneLake input | Source explicitly raises `NotSupported` before questions/file loading; it does not download remote input |
| Bulk-directory repository import | No such source branch: the directory is one item's definition, not an item repository |

Python's Environment import does **not** call staging library-upload, settings
PATCH or commit APIs. `_import_update_item` and `_import_create_item` use generic
definition endpoints followed by `environment_publish` and
`environment_get_publish_details`. Likewise `_build_definition` explicitly
rejects Report `byPath` instead of rebinding it. Inventing either algorithm
would change this source contract.

The regular-file empty-definition behavior is a source oddity, not a suggested
new interface: accidentally choosing one file can produce an empty definition
mutation. This lane preserves it with a regression test. A future explicit
single-file format or directory-only rejection needs a separate owner decision.
For nested targets, `_handle_path_in_folder` still resolves an existing item's
name workspace-wide and constructs the caller-requested folder parent; correcting
that source name-lookup quirk is also a separate change, not part of this port.

## Independent source evidence

The additive `definition_import_lifecycle_reference.py` executes actual immutable
Python payload/API/readiness/publish leaves together. Its six receipts capture
exact requests, raw multi-artifact parts, update ordering, terminal failures,
cancelled readiness, retry messages/delays and exhaustion. Expectations were
captured before the Rust lifecycle/tests were added.

`definition_import_inputs_reference.py` executes the actual import front door,
storage classifier, API functions and payload builder. Its five receipts prove
regular-file empty definitions, initial/overwrite decline/cache differences,
folder IDs and explicit remote rejection. An initially incomplete synthetic
`ItemType` namespace was corrected before adding input expectations; no existing
protected source fixture was modified.

Re-execute both captures from `rust/`:

```text
python -B crates/fabric-deploy/tests/definition_import_reference_checks.py
```

The checker uses a newly created caller-workspace directory, removes only that
owned directory, and compares object insertion order as well as arrays, bytes,
requests and effects. On non-Windows it normalizes only the captured Windows
local-input path separator in the progress diagnostic, not payloads or parts.

## Native file bindings and shared-owner seams

The owner subsequently authorized import-specific async file bindings.
`TokioTransferFiles::prepare_environment_import_async` now calls
`PreparedImport::prepare_environment` on the existing whole-plan worker,
preserving stop checks, minimum limits, owned strings and the same permit.
Generic preparation uses the unchanged legacy low-level contract.

`resolve_import_async` now resolves a confined existing directory **or regular
file** on that worker. `LocalTransferFiles` checks parent containment and links
before inspecting the selected file, then reuses definition preflight for
device/stream/alias checks. Root-file enumeration retains the source's empty
parts; export directory resolution remains directory-only. Constructors, runtime,
manifests, transport and credential composition remain unchanged.

`TokioTransferFiles::for_import_root(source, original_input, limits, runtime)` is
the additive constructor for a caller-selected existing directory or regular
file outside the initial CWD root, including an exact explicitly expanded tilde
argument. It validates on the worker, reuses `Self::new`, and binds an import-only
exact argument alias in `LocalTransferFiles`; it adds no Tokio worker fields or
competing changes to that worker's original constructor. File sources use a
validated parent but cannot import another source under that parent. Alias
binding still rejects traversal, network/device/stream aliases and URI paths.
Export directory resolution does not use this alias.

ROOT must mount that constructor only after target support/format validation
and Fabric-versus-local input classification, so local preflight does not
supplant earlier source errors. `original_input` must be the same normalized
`process_tokens` value passed to import. Resolve only the selected directory or
file parent; do not canonicalize an individual selected file past link/alias
checks. No ambient home/profile lookup belongs in the constructor.

The integration/shared-discovery owners still need:

1. `TransferMetadata::validate_import_input`: the existing metadata owner should
   classify actual Fabric input paths and preserve source remote rejection
   before local resolution/confirmation. It must not introduce remote download.
2. Existing shared transfer metadata must enforce single-target source command
   support and retain actual nested destination-folder ancestry/IDs. The export
   lane owns that discovery implementation.
3. ROOT's real injected console adapter must provide import confirmation,
   decline and prompt-error behavior. The library never supplies an implicit yes.
   The native non-read MSIT global gate remains exclusively ROOT-owned.

The metadata default retains compatibility with local-only injected adapters;
it is not evidence that native Fabric input classification is mounted.

### Exact metadata and context requirements

Import consumes only `TransferContext.target`: an item with its exact type and
short name, an ID for an existing item or `None` only for a genuinely missing
item, a real workspace GUID in the parent chain, and the immediate requested
folder GUID when applicable. It does not consume `items` or `selection_order`;
no new snapshot fields or public target types are needed.

The existing SDK accessor is **`Client::context()`**, returning its immutable
`RequestContext`. Import now constructs **both** operation sessions with
`OperationSession::with_context(client.context(), ...)`, not the fixed-public
`OperationSession::new`. The first session performs readiness reads and one
definition mutation; the fresh second session performs one staging publish and
completion reads. Both borrow the original credential, endpoint/environment
binding and caller's shared executor/header policy. No session is reset or POST
replayed, and a completed definition ID survives subsequent publish failure.

ROOT should continue injecting the already caller-bound SDK client from
`ReadSession`; it must not reconstruct it with `Client::new` as a public-cloud
fallback. No additional read-context constructor, provider or runtime is needed.

## Validation status and required serialized checks

Executed in this lane: new pinned Python captures, exact source receipt
re-execution, source formatting, locked metadata and whitespace checks.
Heavy compilation/tests require the integration owner's serialized slot and
were not run locally. No dependency installation or graph changes were made.

After binding the shared seams, run from `rust/`:

```text
cargo test --offline --locked -p fabric-deploy --lib definitions::import::tests
cargo test --offline --locked -p fabric-deploy --test definitions
cargo test --offline --locked -p fabric-cli-transfer
cargo test --offline --locked -p fabric-cli --features native-read --test family_mounts import_parity
cargo clippy --offline --locked -p fabric-deploy -p fabric-cli-transfer --all-targets -- -D warnings
cargo clippy --offline --locked -p fabric-cli --features native-read --test family_mounts -- -D warnings
```

The new root tests exercise the production parser, `FamilySession`, shared
`HttpExecutor`, actual file worker and real local files. Their expectations come
from the pinned receipts, not an injected fake import engine. They additionally
exercise create/publish LROs, cancellation during backoff, permission failures,
malformed states and Report cases. Planned checks are not passing-check evidence;
live Fabric and non-Windows native runs remain unexecuted.

## Changed files

- `rust/crates/fabric-deploy/src/definitions/import.rs`
- `rust/crates/fabric-deploy/src/definitions/import_tests.rs`
- `rust/crates/fabric-deploy/tests/definition_import_lifecycle_reference.py`
- `rust/crates/fabric-deploy/tests/definition_import_lifecycle_source.json`
- `rust/crates/fabric-deploy/tests/definition_import_inputs_reference.py`
- `rust/crates/fabric-deploy/tests/definition_import_inputs_source.json`
- `rust/crates/fabric-deploy/tests/definition_import_reference_checks.py`
- `rust/crates/fabric-cli-transfer/src/lib.rs`
- `rust/crates/fabric-cli-transfer/src/import_environment.rs`
- `rust/crates/fabric-cli-transfer/src/local.rs` (import resolution and checks)
- `rust/crates/fabric-cli/src/families/transfer_files.rs` (import worker methods)
- `rust/crates/fabric-cli/tests/family_mounts.rs` (additive test registration only)
- `rust/crates/fabric-cli/tests/family_support/import_parity.rs`
- `rust/crates/fabric-cli/tests/family_support/import_roots.rs`
- `rust/docs/import-command-parity.md`

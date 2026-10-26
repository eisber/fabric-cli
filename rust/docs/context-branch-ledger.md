<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Context resolver source branch ledger

Pinned Python: `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
This additive follow-up preserves the original `Resolver`, `Element`, `NodeKind`,
81-case oracle and six historical unsupported classifications unchanged. New
`PathResolver`/`PathElement` consumers opt into extended hierarchy behavior.
The historical six classifications are not newly claimed exact comparisons.

Line ranges below refer to the pinned `src/fabric_cli/core/fab_handle_context.py`
unless another source file is named. State "implemented" means code exists;
execution evidence is recorded separately, not implied by this ledger.

| Source/function/lines | Branch and observable contract | Implementation and caller | State / evidence |
| --- | --- | --- | --- |
| `get_command_context` 44-114 | Scalar vs list join/quote stripping; absolute/relative/home, trailing slashes, root shortcuts | `PathResolver::resolve/remote`, shared original `split_parts` | Implemented; original oracle preserved; extended fixtures separate |
| `process_path_part` 117-146 | Empty/dot; parent clamp; invalid leading three dots; context dispatch | `PathResolver::remote` uses immutable local traversal stack | Implemented; partial failure never changes current |
| `process_context_path` 149-186 | Tenant/workspace/folder/item; virtual workspace/container; OneLake; untraversable leaf | `PathResolver::child` dispatches `PathKind` without changing peer `NodeKind` matches | Implemented; no global Unsupported switch |
| `process_absolute_path/process_relative_path` 189-230 | Escaped slash splitting, all-backslash removal, Python whitespace and parent chains | Shared tokenizer; explicit remote current and tenant | Implemented; no OS virtual-path normalization |
| `get_personal_workspace_name` 233-258 | User-only personal workspace, first ordered match, missing/auth errors | `select_home` over injected identity flag/workspace snapshot; `HomeContext` for errors | Implemented pure selection; acquiring identity/catalog remains caller-owned |
| `_get_*_id` 261-432 | Suppress only `NotFound` when `raise_error=False`; preserve other lookup failures | Original core `Lookup`; additive `HierarchyLookup::virtual_id` | Implemented; service/cache/EDS preprocessing remains caller-resolved |
| `_handle_path_in_tenant` 435-478 | Four hidden virtual workspaces, canonical case, invalid types vs invalid path | `VirtualWorkspaceKind`, `PathKind::VirtualWorkspace`; core delegated to original child logic | Implemented; standalone example reads Domain |
| `_handle_path_in_ws` 481-539 | Real item/folder, four virtual containers; missing workspace before type dispatch | Original child + additive virtual container dispatch after ID check | Implemented; preserves folder-aware existing callers |
| `_handle_path_in_folder` 542-576 | Workspace-wide item IDs, nested-folder parent equality, no virtual container traversal | Original child implementation reused | Unchanged; original expectations retained |
| `_handle_path_in_item` 579-611 | Source FS_LS support check precedes ID/folder validation; ordered case-insensitive item folder map | `CommandPolicy::source_list/check`, `item_folders`, explicit `ls`/`dir` diagnostic alias | Implemented; command authorization/other command effects remain consumers' responsibility |
| `_handle_path_in_onelake` 614-655 | Remove all exact `.Shortcut`/`.shortcut` occurrences, then original-name retry even if identical; missing-final-only creation target | Injected `properties`, `PathResolver::onelake_child` | Implemented; service responses supplied, never HTTP |
| `_get_onelake_details` 658-669 | Suppress properties `NotFound`, preserve other failures | `PathResolver::properties` | Implemented; raw non-200 status produces typed source `AssertionError`, not success or panic |
| `_handle_path_in_onelake` 637-655 | Directory/shortcut/table/file/undefined; table only when parent local path is exactly `Tables` | `OneLakeKind` and immutable name-based OneLake ID paths | Implemented; no file-type traversal restriction invented |
| `_handle_path_in_vws` 672-730 | Capacity/Domain/Connection/Gateway suffix validation and collection mismatch; tenant lookup scope | `VirtualLookupRequest` + typed workspace leaves | Implemented; duplicate/service/cache behavior belongs at supplied snapshot boundary |
| `_handle_path_in_vic` 733-811 | SparkPool/ManagedIdentity/ManagedPrivateEndpoint/ExternalDataShare; container scope and mismatch | `VirtualItemKind`, `VirtualLookupRequest` | Implemented; no managed-resource SDK duplication |
| `get_command_context` 85-93 | External share warning only when final result is external container/leaf | `PathResolution.notices`, `PathNotice::ExternalDataSharing` | Implemented; no global logger or UI |
| `get_command_context` 95-114 | Local fallback only for three source status codes; read probe; `raise_error=False` writable-parent expression | `LocalLookup`, `LocalProbe`, conditional fallback after remote failure | Implemented; caller-classified filesystem, no ambient reads |
| `get_command_context` 109-113 | Source `exists(dirname and access)` receives bool when dirname nonempty, and calls dirname twice | Typed `LocalProbe::Boolean`, explicit ordered local boundary calls | Preserved oddity, not silently fixed; never stat real file descriptors; discussion `tk:post/12/4/105` |
| `hiearchy/fab_local_path.py` 10-43 | Original path, OS basename, process hash ID; file/directory predicates; no tenant | `LocalMetadata {path,name,id,kind}`, `PathElement::local` | Explicit caller OS/identity snapshot; no invented stable Python hash or actual samefile |
| `hiearchy/fab_onelake_element.py` 15-113 | Shortcut display suffix, short-name ID path, local/root path and ancestor shortcut detection | `PathElement::{name,path_id,local_path,root_folder,is_shortcut_path}` | Implemented |
| `fab_commands.py` 146-202 / command support YAML | Group/subcommand union; explicit unsupported item overrides supported item and element | `CommandPolicy` accepts combined canonical caller snapshot; pinned `source_list` ships internal traversal rule | Implemented policy application; no runtime YAML loading/command namespace/persistence |

The source's final `case _` arms in the virtual handlers cannot be reached
after the current closed four-variant type parsers and collection-kind equality
checks. Typed enums preserve that invariant instead of inventing a fifth
success-shaped resource. Unknown suffixes and mismatches retain separate source
errors. They require new source/API behavior before adding a resource kind.

## Boundaries, not silent parity claims

All lookups are already-resolved metadata, not synchronous network calls.
`HierarchyCatalog` is a caller-owned ordered graph snapshot. Generic Fabric
queries reuse `Catalog`; it does not implement retries, TTL or duplicated SDK
operations. Non-200 OneLake responses preserve the source assertion branch as a
typed `AssertionError` with empty message; Python's raw assertion is not
translated into a Rust panic.
Local basename/dirname/hash/classification and exists/access results are supplied
explicitly by a filesystem consumer, including OS-mode semantics. `samefile`,
disk reads/writes, persisted current context and authentication are not resolver
effects and remain outside this pure layer.

Modes and options are explicit inputs. Resolution has no ambient interactive vs
command-line branch; Python's context persistence and command decorations belong
to the session/main owner. `PathInput::Local` is an explicit caller classification
entry point, not Python's implicit fallback and not permission to perform I/O.
`supports_local_path=True` still tries Fabric first. Failed fallback retains the
original remote error and grey diagnostic. Local adapter failure is explicit.
Home selection failure occurs before local fallback, as in Python.

`change_context` is atomic and remote-only. Consumer `cd` support checks and
persistence must occur in their owning command layer, not this library.

## Validation commands

New checks must use the parent-provided common `Local\FabricRustParityValidation`
gate, one build job and one test thread, private short target, at most 120 seconds
gate wait and 10 minutes per command. No tests claimed until actual completion.
Target the context crate, its standalone examples, actual source oracles and the
existing CLI architecture/consumer tests affected by this additive public API.

## Executed comparison and final checks

On October 26, 2026, common-gate attempt C executed the context crate contracts
and both real Python source oracles. The original 81 exact cases, six historical
unsupported classifications and 14 guard-negative checks stayed unchanged.
The new 93 hand-authored exact expectations passed independently in Python and
the Rust extended harness, including ordered virtual/OneLake/filesystem calls,
warnings/notices, current snapshots and parent identities. The public extension
has 19 additional consumer tests; the original 13 passed. One compiled doctest
and the extended standalone hidden/OneLake/local example also passed.

The new oracle verifies 22 source hashes, executes actual source registry and
hierarchy methods, and explicitly selects Windows/POSIX string semantics.
This does not certify real filesystem/permission/descriptor behavior on either
platform. Gate receipts and remaining check outcomes are recorded in the final
validation handoff, not inferred from the passing corpus.

The final authorized warm gate attempt E completed at
`2026-10-26T01:15:31.7353771Z`: strict context all-target Clippy (`-D warnings`),
formatting, and a rerun of the affected 93-case Rust harness all passed.
Existing architecture tests passed 8/8 in attempt C against the exact adopted
`e850b32437e0e9171e8f097bfbddb93d57986c3f` base plus this extension. No
architecture file was changed or later peer edge implicitly certified.

All heavy commands used the existing parent gate
`Local\FabricRustParityValidation`, same-process MSVC setup, private target,
one build job/test thread, 120-second gate wait and 600-second command bounds.
Attempt A exposed an artifact-runner working-directory omission; B exposed
accidentally nested test functions; C exposed a single test-boundary Clippy
lint. Each was corrected without editing Python sources, old oracle files,
source pins or expected values. Attempt D was busy after 120 seconds and ran
no validation. The separate parent-authorized E attempt passed and released
normally, with no owned heavy processes left running.

Evidence artifacts are named `context-gate-20261026-{a,b,c,d,e}.json` and
`context-extended-handoff-20261026.md` in this task's session artifact directory.
These results are native Windows x64/Rust 1.99.1 and Python 3.13 offline evidence,
not new Linux/macOS, live service, real local filesystem or full command parity.

<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Physical local/OneLake source-branch matrix

Source: Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, immutable Git
objects, not current Python files. Paths below start at `src/fabric_cli/`.
This matrix enumerates the physical-handler branch categories and their host
boundary, not a claimed denominator for all commands or every possible input.
`Implemented` means a reusable public library branch, not integrated executable.
`Host-owned` is an explicit remaining composition gate; `source-rejected` is
completed rejection behavior, never an invented recursive/local-move feature.

Evidence abbreviations: `U` = `tests/upload.rs`; `D` = `tests/download.rs`;
`L` = `tests/local_adapter.rs`; `P` = `tests/source_reference.py`, which executes
the original dispatch/handlers/LocalPath/API helpers over actual local fixtures
and synthetic HTTP/UI. Existing earlier expectations are not regenerated.

## Dispatch, mode and path ownership

| Source symbol / lines | Resource/options/error/mode branch | Status / consumer evidence | Remaining gate |
| --- | --- | --- | --- |
| `commands/fs/fab_fs.py::cp_command:95-99` | Resolve paths, mode check, source command-support before dispatch | Host-owned; complete original sequence traced | Central host/parser |
| Same `extract_from_to_paths:215-227` | One token per path, source strict and target prospective | Host-owned | Context/host adapter |
| Same `:228-255` | Multi-token whitespace split, first resolving combination, final invalid-path error | Host-owned | Do not reinterpret tokens here |
| `core/fab_handle_context.py::get_command_context:43-114` | Fabric-first resolution, allowed fallback errors, existing readable local path, prospective parent descriptor oddity | Host-owned; oddity retained in private relay artifact | Pure/async context owner, no silent source fix |
| `parsers/fab_fs_parser.py::register_cp_parser:205-239` | cp/copy, force, recursive, block-on-path-collision options | Host-owned; P passes both modes/options to original physical handler; Ui always denies if called | Central registration/dispatch |
| `commands/fs/fab_fs_cp.py::exec_command:56-57` | Local -> OneLake | Implemented `StorageFilesHost::upload`; U/P | Central select this API |
| Same `:60-61` | OneLake -> local | Implemented `StorageFilesHost::download`; D/P | Central select this API |
| Same `:64-72` | Local -> local; mismatched local/Fabric types | Source-rejected by generic dispatcher; source traced, no native-copy API | Host must preserve rejection |
| `commands/fs/fab_fs_mv.py::exec_command:48-55` | Local source/target moves, equal/different types | Source has no physical local route; no copy-then-delete implementation | Resource/central dispatch rejection, not a missing physical move engine |

## Local -> OneLake: `commands/fs/cp/fab_fs_cp_onelake.py::copy_local_2_onelake`

| Lines / helper | Resource/options/error/mode branch | Implementation and evidence | Remaining gate / explicit distinction |
| --- | --- | --- | --- |
| `:51`; `utils/fab_cmd_cp_utils.py::check_onelake_destination:94-103` | Invalid writable root precedes any local predicate/read | Released pure validator, U precedence/P invalid target cross-product | Existing item-type policy reused, never duplicated |
| `:54-57` | Local directory, with/without recursive | Source-rejected NotSupported before file predicate/target naming | U/P; directory contents untouched, no traversal feature |
| `:60-64` | Missing/non-file source | Two independent native predicates then InvalidPath | U/P/L; Unix socket native case requires Unix execution |
| `:54,60` | Native predicate I/O failure | Same validation outcome; actual cause retained in inspection evidence | U/L; this extra typed evidence is not a silent default |
| `:66`; helper `get_onelake_file_destination:68-91` | Target directory/shortcut appends source basename | Existing released resolver; U/P Unicode/spaces/shortcut | SDK traversal/basename safety guards explicit |
| Same | Target existing file/prospective path retains name | U/P, no additional existence or conditional create | Host must classify prospective kind correctly |
| Same | Unsupported target kind after valid file classification, before open | U/P error order, no read or remote mutation | Undefined with an existing source ID must be rejected by host context |
| `:70` | Progress after all validation, before open | U/P exact message/order | Host renders notices; neither library nor adapter prompts |
| `:74-75` | Actual binary local read, exact bytes | Real OS adapter; U/P/L zero/high bytes and newline/MIME content | Finite read/request bounds explicit |
| Same | Source handle lifetime encloses remote stages and exceptions | LocalReadFile retained; real Windows exclusive-open checks in HTTP callbacks | Cooperative cancellation is not bounded immediate handle disposal |
| `:76-79` | Empty source | InvalidInput before storage PUT, after progress/read | U/P; source bytes unchanged |
| `:82-87` | Local open/read/permission error | InvalidPath and reading-file prefix; original OS cause retained | U/P/L; native suffix may differ by runtime/platform |
| `:81`; helper `upload_file_onelake:40-58` | Existing destination overwrite/create | Existing SDK PUT with no invented condition/extra request | U/P; no auto backup or remote cleanup |
| Helper `:43-44` | Create exactly 201 | Append then flush once | U/P actual public SDK/shared HTTP |
| Same | Create 200/202/204 | Stop; no append/flush/Done | U/P actual response evidence retained |
| Helper `:47-57`, `_infer_content_type:107-116` | Case-sensitive .json/.csv/.txt/default, byte lengths/positions | U/P destination naming and exact HTTP bytes/headers | No JSON interpretation/encoding change |
| Helper `:56-58` | Append/flush 200/202/204 | Source Done and real statuses retained independently | U/P all nine status pairs; accepted never completed |
| Shared request errors at each helper stage | 401/403/404, lost response, cancellation/deadline | U/P error order and stage evidence; no replay or cleanup | Service auth/network are synthetic; no live proof |
| Entire function | Force/recursive/collision/mode/denied UI | Flags do not alter physical branch; no confirmation called | U/P both modes/flag combinations; actual parser registration host-owned |
| Entire function | Empty native source path | Original invalid-source result, no mutation or root reinterpretation | Adapter fix and P/L post-merge regressions passed in receipt14; not equivalent to explicit `"."` |

## OneLake -> local: same module `copy_onelake_2_local`

| Lines | Resource/options/error/mode branch | Implementation and evidence | Remaining gate / explicit distinction |
| --- | --- | --- | --- |
| `:93-94` | Source file | Continue to native target predicate | D/P |
| `:124-127` | Folder/shortcut/table, recursive set or unset | Source-rejected NotSupported before local/remote I/O | D/P; no recursive download feature |
| Match without default | Undefined source | Original no-op, no GET/output/write | D/P; not a success/existence/completion proof |
| `:96-99` | Target native directory | Native basename join | D/P Unicode/spaces; Windows drive/ADS guard explicit |
| Same | Target not a directory/missing | Use selected native path unchanged | D/P |
| Same | Predicate permission/error | Preserve source false-predicate behavior, typed cause retained | D/L; open/write can still fail after GET |
| `:101-106` | Progress then actual remote GET | Existing SDK and shared bound HTTP | D/P no extra HEAD, bytes not Content-Length |
| `:107-110` | Empty GET body | InvalidInput, existing target not opened/truncated | D/P |
| `:105-106` | GET 401/403/404, oversized body/cancellation | Retain SDK/caller error, no local write | D/P; finite response safety bound |
| Same | GET unexpected 202/204 with body | Strict released SDK InvalidResponse | P separately asserts source difference, never counted as parity |
| `:115-116` | New/existing local file | Binary create/truncate/write, exact bytes | D/P, no prompt, no text/JSON/newline conversion |
| Same | Missing parent or directory collision | Fail after GET; no parent mkdir or successful output | D/P/L |
| `:117-120` | Permission/native write/partial-write fault | Writing-file prefix, partial bytes/open evidence, no Done | D/P/L; no partial or unrelated-file deletion |
| `:115-120` | Injected short successful write | Explicit error, not Done | D; safety validation rather than Python bug emulation |
| `:122` | Complete local write | Source Done and actual bytes_written | D/P |
| Entire function | Empty native target path | GET/empty-body checks retain order, nonempty source fails local write | Adapter root-reinterpretation bug fixed; D/P/L no-write regressions passed in receipt14 |
| Entire function | Ready/in-flight cancellation/deadline | Deferred injected calls, original failure stage and possible effects | D/U; cancellation is not rollback |

## Validation and completeness state

The original physical checkpoint has actual Windows x64 evidence in receipt12:
32 own Rust tests, 117 source/OS comparisons (78 uploads, 39 downloads), two
explicit SDK-status differences, eight architecture cases and nine released
preflight/write regressions. That evidence is retained independently of the
later empty-native-path regression and shared-checkpoint adoption.
Attempt13 acquired no validation slot: the common gate reported an existing
uncoordinated heavy process and ran no compiler, formatter, source probe or test.
Receipt14 then completed the focused post-merge proof: three exact Rust cases
(native empty-path/no-write, download GET-before-write-failure, actual public
upload/download future Send), three original-source/OS empty-path comparisons,
strict targeted Clippy, formatting and the rebuilt public consumer. This adds
three source cases to the historical 117, without rerunning or relabeling the
earlier full suite as current checkpoint evidence.

Implementation is reusable at the physical library boundary. Integration into
the central executable, actual context-resolution fidelity, native Linux/macOS,
production identity/transport and live service validation are not hidden behind
test counts. The exact [host contract](HOST-CONTRACT.md) defines these handoffs.

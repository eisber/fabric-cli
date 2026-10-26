<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Extended context reference fixtures

This additive development oracle covers hidden resources, OneLake traversal, and
local-path fallback in Python source commit
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. It does not modify or replace
`reference.py`, `cases.json`, their pins, or their 81 exact and six unsupported
cases. The Rust resolver and its consumer harness are separately owned.

## Status and execution gate

The **93 exact expectations were hand-authored from source before execution**.
There are no generated expectations, Rust-derived expectations, or unsupported
waivers in this new fixture. Preparation checked JSON structure, source hashes,
and current-snapshot references with read-only PowerShell operations.

**Executed October 26, 2026:** all 93 exact cases passed against the real pinned
Python source and independently in the Rust extended harness, through the
integration owner's existing common parity gate. The oracle also passed the
14 reused guard-negative probes and its added cached-import denial probe.
No Python, Cargo, test, formatter, package installation, or parity command was
run during preparation. The shared gate supplied the execution coordinates and
build slot before these results were obtained. For authorized reruns, invoke
`python tests\compatibility\context\extended_reference.py` from `rust\` through
that gate, not as an independent concurrent test job. This document is not an
execution grant.

Do not infer parity, platform execution, or oracle success from the fixture count.
If execution finds a mismatch, investigate the pinned source and boundary setup;
do not regenerate or silently weaken expectations to match Rust.

## Executed product definitions

`extended_reference.py` loads its unchanged sibling oracle as a local module
before entering the guard. It reuses the existing AST compiler, error/type/base
hierarchy definitions, remote lookup wrappers, and denial guard. It additionally
selects the actual source definitions for:

- All hidden and OneLake handlers in `fab_handle_context.py`, including
  `_get_onelake_details` and the eight virtual lookup error wrappers.
- `VirtualWorkspaceItem`, `VirtualItemContainer`, `VirtualItem`, `OneLakeItem`,
  and `LocalPath`, including their actual constructors and path properties.
- Every enum used by `ItemFoldersMap`, the actual map, and `VICMap`.
- The real `Command` enum and command-registry matching/conversion functions.
  The complete `FS_LS` YAML block is independently source-verified against a
  bounded projection before the real registry helpers run. No YAML library,
  registry file open, or always-supported command stub executes under the guard.
- Actual virtual memstore ID matching/retry/error functions, plus the external
  share parser and its nested item lookup.
- The actual `remove_dot_suffix`, including its replacement-anywhere behavior.

The fixture pins 22 complete source files by SHA-256. Reads, local module loading,
source verification, and compilation of the main namespace happen before
product execution. The guard blocks real imports (including cached imports),
file opens, metadata/environment discovery, authentication, network, process
creation, and sleep. The source's lazy `Context` import resolves only to the
injected caller object; it never imports a product module.

Only caller lookup collections, the caller's current context/tenant/home, warning
and notice sinks, OneLake properties responses, and local filesystem metadata are
injected. The source performs path splitting, canonicalization, recursion,
matching, command support, hierarchy construction, error handling, and fallback.
No Rust implementation is loaded or consulted.

## Fixture schema

`extended_cases.json` uses independent schema version `1`.

| Field | Meaning |
| --- | --- |
| `source_commit`, `source_sha256` | Exact reviewed source baseline and full-file hashes |
| `defaults` | Values merged into each case before its explicit fields |
| `catalog` | Ordered synthetic hierarchy records; parents precede children |
| `current_snapshots` | Hand-authored full snapshots keyed by caller context name |
| `cases` | Named exact inputs, scripted boundaries, and independent expectations |

Cases specify `input` (string or source-supported token list), `current`, `home`,
`raise_error`, `supports_local_path`, and `local_mode` directly or through
`defaults`. Modes are exactly `windows` and `posix`, never the runner's ambient
platform. `home` is either an injected workspace name or
`{"error":{"code":"...","message":"..."}}`; authentication is not consulted.

A result snapshot always includes `path`, `path_id`, `kind`, `name`, and `id`.
Additional fields are:

| Kind | Fields |
| --- | --- |
| Workspace | `subtype`: workspace type, such as `Personal` |
| VirtualWorkspace | `subtype`: hidden container name, such as `.capacities` |
| VirtualItemContainer | `subtype`: hidden container name, such as `.sparkpools` |
| Item / VirtualItem / VirtualWorkspaceItem | `subtype`: source item type string |
| Onelake | `subtype`: enum member name; `local_path`; `root_folder` |
| Tenant / LocalPath | No additional fields |

The spelling `Onelake`, bare OneLake names such as `Files`, virtual workspace
`/None` ID prefixes, and local `path_id: "/"` are deliberate source results.
IDs remain JSON null when the actual source constructs a missing resource.

Each `expected` object contains either a complete `result` or exact
`error: {code,message}`, plus:

- `calls`: one ordered cross-boundary trace.
- `warnings`: exact ordered `fab_logger.log_warning` messages.
- `notices`: exact ordered grey local-fallback notices.
- `current_before` and `current_after`: keys into `current_snapshots`, **not
  generated snapshots**. The harness verifies the real current snapshot against
  that key, checks the entire catalog's snapshots before/after, checks parent
  identities, and requires the caller's current object to remain identical.

The source's non-200 properties assertion is represented as
`{"code":"AssertionError","message":""}` in the same error shape. Only an
argument-free assertion whose deepest traceback frame is the actual pinned
`_handle_path_in_onelake` function is normalized; guard and harness assertions
still fail the oracle. This is not a `FabricCLIError`, so the source does not
suppress it or attempt local fallback. Injected service failures are constructed
through the real `FabricCLIError`, preserving its trailing-dot message stripping.

Call shapes are:

```json
{"op":"capacity","parent":"/","name":"Compute.Capacity"}
{"op":"external_data_share","parent":"/Alpha.Workspace/.externaldatashares","name":"Lake.Lakehouse_Invite.ExternalDataShare"}
{"op":"properties","path":"w-alpha/i-lake/Files/data.csv"}
{"op":"exists","arg":true}
{"op":"dirname","arg":"/data/new.txt"}
{"op":"access","arg":"/data","mode":"W_OK"}
```

Lookup operations use the old remote names (`workspace`, `item`, `folder`,
`nested_folder`) and the additive names `capacity`, `domain`, `connection`,
`gateway`, `spark_pool`, `managed_identity`, `managed_private_endpoint`, and
`external_data_share`. Collection/cache operations are intentionally inside the
lookup boundary; the actual source matching and retry branches still execute.
The external-share lookup's nested `item` call is included in the same trace.
`failures` injects exact caller-lookup errors by `op,parent,name`.

`properties` is a strict ordered script. Each entry has a `path` and either
`status_code,headers` or `error: {code,message}`. Duplicate requests must be
explicitly repeated in both the script and expectations. Unscripted requests and
unused entries fail, including after a source error.

For a Rust `OneLakeProperties` adapter, copy `status_code` unchanged, set
`resource_type` to the optional exact `x-ms-resource-type` header string, and set
`is_shortcut` only when `x-ms-onelake-shortcut-path` equals the literal `"true"`.
Do not lowercase either header value: `"Directory"` is undefined, and `"True"`
does not indicate a shortcut.

`local_fs.operations` is similarly strict, with `exists` or `access` call fields
and a Boolean `result`. The `arg` type is significant: `true`, `false`, strings,
and empty strings must not be coerced. `local_fs.hash` injects the deterministic
integer used by the actual `LocalPath` constructor; the source still stringifies
it. Basename/dirname use preloaded pure `ntpath` or `posixpath` functions selected
by `local_mode`, never cwd, home expansion, actual stat/access, or ambient hashing.
Those pure string operations do not touch the filesystem.
`dirname` invocations are recorded in `calls` to preserve
the source's evaluation order and one-versus-two-call short-circuit behavior.
It has no entry in `local_fs.operations`; its return value comes from the
explicitly selected pure path module. Basename and hash remain untraced.

## Bounded behavior inventory

- All four tenant-level hidden collections and all four workspace-level virtual
  item containers resolve real typed children; canonicalization, context type
  mismatches, missing IDs, suppression, malformed names, leaf rejection,
  parent/sibling traversal, and external-sharing warnings are exercised.
- The external-share malformed-name case deliberately produces a lookup of the
  empty item name, rather than a friendlier redesigned validation error.
- OneLake root folders include every distinct folder name in `ItemFoldersMap`:
  `Files`, `Tables`, `TableMaintenance`, `Libs`, `Main`, `Snapshots`, `Shortcut`,
  `Code`, and `Audit`. The real registry rejects `MirroredWarehouse` despite its
  folder map, permits `GraphQLApi` whose map is empty, and rejects `Notebook`.
- Metadata produces directory, table, file, shortcut, and undefined kinds.
  Cases preserve exact header casing, shortcut precedence, literal `.Shortcut`
  fallback, replacement-anywhere semantics, missing-final versus nonfinal
  handling, duplicate metadata requests, and service-error propagation.
  Raw status 201, 404, and 500 responses preserve the source's empty
  `AssertionError` without retry, suppression, or local fallback; a returned 404
  is deliberately different from a thrown `FabricCLIError` with code `NotFound`.
  The source permits a properties request below a OneLake file; the oracle does
  not impose a new leaf restriction.
- Local fallback covers readable, read-denied, missing, write-parent, Windows,
  POSIX, token-list, option-disabled, error eligibility, successful-Fabric
  short-circuit, suppressed-final-missing, and OneLake-to-local branches.
- In the source's write-parent expression, `exists(dirname(path) and
  access(dirname(path), W_OK))` receives a **Boolean**, not the parent directory.
  Separate scripts preserve true/false descriptor acceptance and rejection, plus
  empty dirname short-circuiting. No real descriptor or filesystem is touched.
- Home is explicit caller input; a home error happens before the source fallback
  `try` block. Service/auth errors never trigger filesystem fallback.

## Remaining limits

These are synthetic offline boundaries, not API response decoding, cache
configuration/invalidation parity, authentication, authorization, live OneLake,
real permissions, native descriptors, Windows drive semantics end to end,
shortcut endpoint behavior, REPL state persistence, or general command parity.
Local `is_file`, `is_directory`, and `samefile` are denied and are not part of this
resolution slice. The real API call sites, disk behavior, and both native
platforms remain separate validation work. The passing offline comparison does
not remove those limits or expand the original oracle's bounded parity claim.

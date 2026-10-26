<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded remote context compatibility oracle

`reference.py` is an independent, standard-library-only development oracle for
the Python source at `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. It does not
implement a second path resolver and is not a Rust production dependency.
All expectations in `cases.json` were hand-authored from source, not generated
from either implementation. Do not regenerate them to make a port pass.
The catalog contains 26 ordered nodes, 81 exact cases, and six separately
classified unsupported cases.

## Execution and provenance

After obtaining the shared-host validation grant, run from the repository root:

```powershell
python rust\tests\compatibility\context\reference.py
```

No Python, Cargo, test, install, or build command was executed while initially
authoring these files. Source equality against the pinned commit was checked
with a read-only Git diff; SHA-256 values were collected with `Get-FileHash`.
Under the parent's named `context-parity-20261024-a` validation grant on October
24, 2026, Python 3.13 executed all **81 exact cases and 14 guard-negative checks
successfully**. The six unsupported cases remain classifications, not executed
Python parity. The Rust harness independently matched all 81 exact expectations
and returned the expected typed unsupported category for all six exclusions.

Before algorithm execution the oracle reads fixtures and the real source files,
checks every raw-file SHA-256, and selects actual AST definitions. The definitions
include `get_command_context`, absolute/relative processing, recursive traversal,
tenant/workspace/folder handlers, exception and error-message definitions,
the actual enums, and the actual element/tenant/workspace/folder/base-item/item
hierarchy. Imports and unrelated top-level initialization are not executed.
The selected source functions and class method bodies are not rewritten.

The only substituted product boundaries are:

- `Context()` supplies the explicitly chosen synthetic current node and tenant.
- `get_personal_workspace_name()` returns the explicit fixture `home` name.
  This intentionally replaces personal-workspace authentication/discovery;
  it does not claim that ambient discovery was ported.
- The memory-store caller boundary records ordered lookups and can inject a
  `FabricCLIError`. Its collection providers return synthetic ordered nodes.
  The real source `get_*_id` wrappers and `_get_*` matching functions execute,
  including exact-name matching, parent equality, first-match selection, and
  missing-resource messages. Cache clearing is an in-memory no-op.

The source import statements inside the handlers can resolve only the injected
`fabric_cli.core.fab_context.Context`; no product package is imported.
The source module namespace replaces filesystem/auth/network/logging/command
dependencies with denial objects. During construction and resolution a guard
also denies filesystem operations (including stat/access), environment discovery,
network, process creation, sleeping, real imports, and relevant audit events.
Fixtures and source are read outside this guard. Unsupported branches are
classified without execution; entering one from an exact case fails loudly.
Each exact case verifies current-object identity, every catalog snapshot, and
all parent-object links after success or failure.

Immediately after installing the audit hook, and before any cases, 14 negative
checks enter the same guard and require its exact denial exception for
`open`/`io.open`/`stat`/`access`, environment indexing/`get`/`getenv`, socket
creation, subprocess execution, sleep, controlled product and authentication
imports, the injected OneLake dependency, and a synthetic filesystem audit
event. The patched boundaries reject the probes before real operations occur.
These checks do not create a probe file, launch an executable, import product
or authentication modules, open a socket, or wait. They are not fixture parity
cases and do not alter the 81-exact/six-unsupported counts. Their execution is
also subject to the named parent validation grant.

## JSON schema (version 1)

The Rust harness consumes the same immutable `cases.json`.

- `schema_version`: `1`.
- `source_commit`: pinned upstream SHA.
- `source_sha256`: repository-relative source names to raw-byte SHA-256 values.
  These strings use `/` as portable JSON identifiers, not native path commands.
- `catalog`: ordered records with `key`, `kind`, `name` (display name), `id`,
  `parent` (catalog key or null), and `type` for Workspace/Item records.
  Parents precede children. Order is semantically significant. Duplicate names
  and IDs are synthetic, never live tenant data.
- `cases`: independently named inputs. Every case explicitly provides `input`
  (scalar string or token array), `current` (catalog key), `home` (full workspace
  name, including suffix), `raise_error`, and `supports_local_path`.
- Optional `failures` is an ordered array of `{op,parent,name,error}` matching
  the complete lookup tuple. `error` is `{code,message}`. Injection occurs
  before the actual matching function at that caller boundary.
- Exact `expected` contains either `result: {path,path_id,kind,name,id}` or
  `error: {code,message}`, plus `lookups`, `current_before`, `current_after`.
  `kind` is the source enum string (`Tenant`, `Workspace`, `Folder`, `Item`).
  `name` is the source full name, not the display name. Null IDs remain JSON
  null, but an absent-ID element's `path_id` uses the literal string `None`.
  Error `message` is the actual exception `.message` (trailing periods removed),
  not `str(exception)` with a bracketed status prefix.
- `lookups` is an ordered array of `{op,parent,name}`. `op` is `workspace`,
  `folder`, `nested_folder`, or `item`; `parent` is the lookup container's
  canonical display path. Item lookups always use the containing workspace,
  even while resolving through folders. This traces caller lookups, not
  collection-provider calls/cache retries.
- `current_before/current_after` are explicit catalog **keys**, denoting the
  full current-node snapshot and ancestor links in `catalog`, not selected
  return values. Resolution is non-mutating even after partially resolved
  errors. The oracle checks full snapshots and identity independently.
- Unsupported `expected` is only `{unsupported: <stable category>}` with a
  sibling `source_behavior` description. These cases are separately counted,
  never executed by the Python oracle, and never counted as exact parity.

## Intentional source nuances

Suffix matching uses Unicode `lower` and is canonicalized, but display names
match exactly (including Unicode and case). Kelvin sign U+212A lowercases to `k`
in suffixes; the item regex's ASCII-looking range also accepts it under Python
`re.IGNORECASE`. The strict item-stem regex has no ignore-case flag, so a Kelvin
sign in a Lakehouse stem is rejected. Names with dots retain their earlier dots.
The initial workspace folder search scans **all** workspace folders, including
nested ones; its returned object is reconstructed under the requested workspace.
Subsequent nested-folder searches compare complete parent objects. Item searches
are workspace-wide, not folder-scoped. First duplicates win in catalog order.
Folder `path_id` is its parent's ID path, never its own folder ID.

Only slashes not immediately preceded by a backslash split the input; then
**all** backslashes are removed and each component uses Python Unicode `strip`.
This includes U+001C through U+001F, not just Unicode `White_Space`. Internal
newlines remain: workspace/item stem regexes reject them, whereas the folder
validator falls back to the entire supplied name and appends another `.Folder`.
Trailing slashes are removed before splitting, including an escaped trailing
slash. Empty components and `.` do nothing; `..` clamps at tenant. Three leading
dots reject even `...suffix`. Token arrays join with one space and strip both
quote characters at their extreme ends; scalar strings do not strip quotes.
Home replacement substitutes only the first `~` and retains the suffix path.

Empty Item/Folder display names are accepted by their real validators and
constructors. Leading-dot names at workspace level nevertheless fail the
handler's full-token type dispatch; the folder handler uses the final suffix
and accepts `.Notebook`/`.Folder`. An empty Workspace display name is invalid.
Lakehouse, MLExperiment, MLModel, and Eventstream names reject anything outside
ASCII letters/digits/underscore; Notebook/Report names do not share that rule.
`raise_error=false` suppresses only `NotFound` from a memory-store lookup, not
invalid names, other lookup failures, or traversal into an unresolved parent.

## Excluded from exact parity

Local fallback, virtual administrative workspaces/items, external sharing,
OneLake traversal, and ambient personal-workspace discovery are explicitly
unsupported in the bounded Rust slice. The classified cases describe the source
branches without running filesystem, auth, or service behavior. These files do
not validate live service cache refresh, hierarchy API ingestion, commands,
persistence, logging, item traversal, or any authentication backend.

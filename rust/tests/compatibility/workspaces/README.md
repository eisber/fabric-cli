<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Offline Python workspace-list reference

This is a development-only independent oracle, not a Rust implementation or a
Python dependency of the shipped Rust product. `cases.json` contains exactly
eight hand-authored synthetic inputs and expected results. No live recordings,
credentials, tenants, existing Python tests, or Rust-produced snapshots are used.
There is deliberately no record, accept, update, or regenerate mode.

## Source and provenance

The behavior source is `microsoft/fabric-cli` commit
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. The lane adopts Rust foundation
`eccfca1d3b1cb4df71d0fa65a76221a588095290`, whose Python source and existing tests
are unchanged from that source commit.

Before every observation, the harness verifies the manifest's aggregate SHA-256
over all 259 `.py` and `.yaml` files under `src`. Entries are sorted by relative
path and encoded as UTF-8 `path + NUL + sha256(contents) + LF`; paths use `/` and
only checkout CRLF is normalized to LF in contents. The aggregate is SHA-256 of
the concatenated entries. Source drift fails closed and requires independent
owner review; do not update the digest or expectations to make Rust pass.

All fixture data is synthetic. Microsoft source and these fixtures are under
the repository's MIT license. Dependency versions are inherited from the
development interpreter, not a newly invented Python lock or a Rust dependency.

## Actual helper boundary

Each observation starts a fresh `python -I` interpreter and composes the same
workspace-only calls made by `commands/fs/fab_fs.py::ls_command`:

1. Real `handle_exceptions` and `set_command_context` decorators.
2. Real `fab_handle_context.get_command_context` and command-support check.
3. Real `fab_fs_ls_workspace.exec`, memory-store workspace construction, API
   `list_workspaces`, `do_request`, pagination, and `ApiResponse.append_text`.
4. Real sorting, JMESPath, output models, and headless fallback JSON printing.
5. For the missing-`value` `KeyError`, real
   `main._handle_unexpected_error` and its `SystemExit`.

The command's broad module is not imported: it eagerly imports unrelated
deployment/mutation commands. The workspace-only composition avoids importing
those commands without substituting workspace behavior. This is **not** a full
CLI/parser subprocess test: command name `ls`, JSON output, no `--long`, disabled
cache/debug/context persistence, and in-memory context are controlled inputs.
Success `None` is converted to exit 0 exactly as `main` does. Text/table output,
argument aliases, authentication, persisted context loading, capacities, and
other command dispatch are not claimed.

Auth, transport, environment, import-time directory creation, clock, and the
terminal backend are controlled. `socket.gethostname` is fixed to a synthetic
value so Windows platform detection does not trip the network guard during
imports; actual DNS/network operations remain denied.

Real JSON serialization and the product's `_safe_print`/`_print_fallback` remain
in the path. Questionary is required explicitly, and its printer raises one
controlled `RuntimeError` to select the real headless fallback on every host.
The harness asserts exactly one call to that backend. This avoids depending on
whether the host has a console or its renderer emits CRLF. It does **not** test
the successful questionary/prompt-toolkit renderer or establish terminal-byte
parity. That renderer needs separate expectations and native tests; its output
must not be silently normalized to make this oracle pass.
The output clock is fixed to `2026-01-01T00:00:00Z`, not deleted from comparisons.
Exact headless stdout text (including indent/key order/LF), empty stderr, actual
helper exit code, exception mapping, service-order IDs, pagination metadata,
HTTP method/full URI sequence, credential scopes, and unchanged current context
are checked. Structured data is checked as part of the exact JSON envelope.

## Eight cases

| Case | Why it belongs to this slice |
| --- | --- |
| `empty` | Empty `value` becomes successful `result.data: []`. |
| `names_types_order` | Default name-only projection, `.Workspace`/`.Personal` suffixes, stable case-insensitive sorting, literal slash/Unicode names, unknown workspace types filtered by the real memory-store helper. |
| `token_pagination` | Three token pages including an empty middle page; opaque token URL encoding; service-order aggregation before sorting; `continuationUri` ignored; Python's observed `total_pages: 1` is preserved rather than corrected. |
| `query_projection` | Real JMESPath filter/object projection operates on sorted name/id rows, not raw service fields. |
| `missing_value` | Missing `value` is a raw `KeyError`; the main error helper maps it to `UnexpectedError`, exit 1, without a command key. |
| `forbidden_mapping` | 403 becomes `FabricCLIError`; public error header overrides its code and standard forbidden text overrides its body. |
| `fabric_error` | 400 becomes `FabricAPIError`; code/message come from the body, final period is stripped, request ID is retained on the exception but not JSON output. |
| `root_from_workspace` | Explicit `/` uses the tenant despite current workspace context, issues only the list request, includes sorted hidden collections with `-a`, and leaves context untouched. |

The quota is not filled with invented server-side filters, unsupported output
flags, or new command namespaces. Filtering is the existing CLI JMESPath layer.

## Offline safeguards

The parent runner launches only its own interpreter with `-I` and a scrubbed
environment. Before product imports, the worker redirects HOME, USERPROFILE,
APPDATA, LOCALAPPDATA, XDG_CONFIG_HOME, TEMP, and TMP to a fresh temporary area.
The harness creates only empty sandbox directories. It never writes config,
token, cache, golden, or product files. Import-time config-directory creation
is replaced with validation of the existing empty sandbox directory.
The controlled environment disables urllib3's import-time IPv6 bind probe;
the network guard still denies all actual network operations.

A process-local audit hook denies network operations, file writes, directory
mutation, and subprocess execution during product import/execution. Identity
dependencies (`msal`, `msal_extensions`, `azure`) cannot load. The auth module is
an in-memory test double exposing only `get_access_token`; requests receive only
a synthetic sentinel. Real requests sessions are denied, and the single scripted
session consumes an exact ordered response list. Extra requests, unused
responses, sleeping, any recorded guard violation, files in the sandbox, or
current-context mutation fail the observation. Temporary directories are removed
after guards are deactivated for harness cleanup.

These are regression safeguards, not an operating-system sandbox for hostile
code or an assurance about arbitrary interpreter startup hooks/native code.
Use a trusted isolated development interpreter.

## Running

From the repository root, with the existing Python development dependencies
available in an isolated interpreter:

```powershell
& <python.exe> -I rust\tests\compatibility\workspaces\reference.py --check
& <python.exe> -I rust\tests\compatibility\workspaces\test_workspace_reference.py -v
```

The first command checks all eight goldens. The second adds repeat-run
determinism, ten independently mutated observation fields (each must fail), and
five adversarial offline-boundary probes. Neither command runs the existing
Python suites, Cargo, coverage, a live endpoint, or credential discovery.
On a coordinated host, obtain a named bounded validation grant first. A missing
interpreter or module is a blocker, not permission for global installation,
TLS bypass, broad repeated restores, a test skip, or a baseline change.

## Rust comparison seam

The requested SDK injection seam is an ordered synthetic status/headers/body
transport, observable GET URI/query sequence, injected credential scopes/count,
and a returned service-order workspace collection or typed error. No SDK/core/
manifest changes or Rust adapter are included here; attach the adapter only
after the parent confirms the SDK interface.

Compare the SDK's raw collection separately from CLI suffix/filter/order/query/
output behavior. A bootstrap Rust JSON error is not Python parity. Python's
recursive unbounded token traversal and ignored `continuationUri` are observed
behavior, **not** permission to remove Rust's deliberately required bounds,
origin validation, or explicit incomplete-result errors. Those safety differences
need separate labeled assertions and owner-reviewed compatibility exceptions.

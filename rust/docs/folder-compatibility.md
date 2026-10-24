<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Bounded folder read compatibility

Source: Python `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`,
`fab_api_folders.py::{list_folders,get_folder}` plus actual
`fab_api_client.py::{do_request,_handle_successful_response}` and
`fab_api_types.py::ApiResponse`. The [focused crate](../crates/fabric-sdk-folders/README.md),
additive facade and thin shared HTTP adapter are SDK-layer consumers, not a CLI
folder implementation or a generator/bridge.

| Surface | Implemented contract | Boundary |
| --- | --- | --- |
| LIST | GET `/v1/workspaces/{ws}/folders`, default string `recursive=True`, exact query/root/token merge order | Explicit options model attribute presence; scalar subset only |
| GET | GET `/v1/workspaces/{ws}/folders/{folder}`, no LIST defaults | Single typed folder, no object-list aggregation |
| Models | Opaque string id/displayName; raw parent, null/unknown metadata, order and duplicates | No inferred UUID schema, tree reconstruction or filtering |
| Credentials | Injected Azure credential, explicit account/tenant/Fabric selection, scope per page | No discovery, fallback, backend or runtime |
| HTTP | Existing private shared execution for both methods | No new transport/retry/TLS policy |
| Failures | Python status/header/body-code precedence; structured diagnostics retained as data | Sanitized Display; malformed data never becomes empty success |
| Progress | Committed complete pages, rows and next unconsumed token | Not Python's recursive `total_pages` metadata |
| Bounds | Default exact/+1 100 pages, 100,000 rows, 4 MiB per response | Explicit Rust safety exceptions, not Python bounds |
| Continuation | Header presence/empty precedence, query token replacement, cycles rejected | Active exact-origin/path hints validated but never followed; terminal hints ignored |
| Cancellation | Before auth, in-flight auth/HTTP, between pages and ready-result priority | Drop releases local futures; no claim about remote rollback |

The [new independent oracle](../tests/compatibility/folders/README.md) distinguishes
26 comparable raw request/row/error cases from 13 raw-Python quirks (12 matching
typed Rust safety rejections, one Python-only invalid namespace state). Additional
Rust boundary/consumer tests are not counted as Python parity cases. Existing
workspace, item, capacity and HTTP fixtures remain unchanged.

Actual memstore/path/list consumers are source-inspected and hash-pinned. Their
cache, tree assembly, `.Folder` paths, configuration, sorting, query/export and
CLI invocation remain unported here. No create/delete/move/patch, full folder
or Python CLI parity, new namespace, live authentication or Fabric execution is
claimed. Final integrated native CI remains separate from local Windows results.

## Executed local validation

Final serialized gate: October 24, 2026, **20:12:13-20:12:55 UTC**,
Rust 1.99.1 (`58cf8f9c3`), `x86_64-pc-windows-msvc`, Python 3.13.
One process tree, private Cargo target, build jobs/test threads both one.

| Command from `rust/` | Result |
| --- | --- |
| `python -m black tests/compatibility/folders/reference.py tests/compatibility/folders/test_reference.py` | Formatted the two new oracle files |
| `python tests/compatibility/folders/reference.py --check` | 26 comparable + 13 raw-Python quirk cases passed |
| `python -m unittest discover -s tests/compatibility/folders -p test_reference.py` | 6 tests passed, including comparison mutations, actual pure memstore helpers and no-I/O/source-drift guards |
| `cargo test --offline --locked -p fabric-sdk-folders -p fabric-sdk -p fabric-sdk-http --all-targets` | 64 tests passed, including 19 folder standalone, 1 folder facade and 5 folder HTTP consumers |
| `cargo test --offline --locked -p fabric-sdk-folders -p fabric-sdk -p fabric-sdk-http --doc` | 2 compiled examples passed, including the new folder caller |
| `cargo clippy --offline --locked -p fabric-sdk-folders -p fabric-sdk -p fabric-sdk-http --all-targets -- -D warnings` | Passed |
| `cargo test --offline --locked -p fabric-cli --test workspace_contract` | 8 architecture tests passed |
| `cargo fmt --all -- --check` | Passed |

Two initial validation failures were repaired without changing source
expectations: CPython's cached JSON-decoder error import needed a no-loader
guard allowance, and the new standalone fixture include path needed the
integration-test directory depth. Only the new local package/dependency edges
were added to the lockfile offline; external versions/features are unchanged.
All heavy invocations used the assigned shared OS mutex runner. Total command
time across all five attempts was 459.261 seconds; no individual command
exceeded ten minutes. The final receipt recorded normal completion and mutex
release. A fresh 20:13:25 UTC snapshot found no cargo/rustc/rustfmt/clippy/Python/
Java/link process. No background job, live call, install, push or PR was made.

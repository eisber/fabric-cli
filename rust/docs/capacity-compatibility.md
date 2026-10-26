<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Capacity-list compatibility matrix

Scope: `fab_api_capacity.py::list_capacities`, shared request/aggregation helpers,
and caller context at Python commit
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. Handwritten D10 implementation
starts from Rust checkpoint `4e822a3b760214f37e789d8a016e8cd7635f2f7f`.
No existing workspace, item, or shared-HTTP golden/expected value is changed.

| Behavior | Source observation | Rust public gate |
| --- | --- | --- |
| Method/route/scope | Factory sets GET `capacities`; shared helper uses Fabric `/v1` and `.default` scope | Standalone, facade and shared HTTP consumer tests check fixed route, token, content type and scope. |
| Empty and multiple pages | Raw empty arrays accepted; recursive aggregate preserves order/duplicates, including empty intermediate pages | Actual pages tracked, no sorting or deduplication. |
| Case and future fields | Raw names/state/SKU/region are not normalized by the factory | Round-trip unknown fields, case, null vs missing, future enum/value shapes. |
| Required rows | Mem-store hierarchy directly indexes `id` and `displayName`; raw factory does not validate | String fields required; malformed rows fail typed, retaining prior committed pages. |
| Header continuation precedence | Header presence overrides body, including empty header | Mixed-case header and malformed overridden body token tests. |
| Opaque query tokens | Requests uses Python `urlencode`, including space, `+`, `/`, `=`, `%`, Unicode, `~`, `*` | Exact encoded fixed-route requests; never concatenate/follow an opaque URL. |
| URI handling | Python ignores URI, even active foreign URI | Validate active URI origin/path/port/userinfo/fragment; ignore terminal URI. Explicit existing safety difference. |
| Error precedence | 401/403 headers before fallback; 404 fixed; generic Fabric body code | Public typed errors retain structured request/details data; static Display does not leak diagnostics. |
| 429/retries | Outer four-attempt 429 loop with virtual sleeps; HTTPS adapter separate | Existing private shared HTTP policy reused; adapter tests check recovery, exhaustion and partial failures. |
| Bounds | No Python pagination or body/row cap | Test exact and +1 for actual default 100 pages, 100000 rows, 4MiB, plus small-limit partial-progress tests. |
| Cancellation | No matching async cancellation contract | Pre-call, pending credential, pending executor, ready response, between pages, future drop, tie priority. |
| Partial failure | Python raises rather than returning explicit committed progress | Retain prior pages on decode/execution/service/row-limit failures; decoded current page retained on continuation/page-limit failures. |
| Page metadata | Recursive `total_pages` remains 1 | Accurate `pages_received`; documented SDK metadata difference. |
| Follow-up 201 | Python only appends recursive responses with final status 200, potentially omitting a 201 page | Existing SDK 200/201 page contract accumulates both; no known incomplete traversal becomes success. |
| Construction/facade | Python uses ambient auth/context | Injected provider and explicit selection; additive facade preserves same provider and typed models. |

The independent [source oracle](../tests/compatibility/capacities/README.md)
uses pinned real Python factory/helper definitions, fake HTTP/auth and virtual
time against hand-authored expectations. It is development-only, not a product
bridge. Source digests fail closed on drift; source behavior and Rust safety
exceptions are recorded separately, not rewritten to make the port pass.

There are 35 frozen Python cases. Of the 20 cases labelled for compatible typed
rows, the public shared-HTTP consumer checks 17 exact collection/error-code,
request, scope and virtual-wait observations. Three use the same immutable input
to assert mandatory Rust rejection instead: active foreign continuation URI,
empty Retry-After and noninteger Retry-After. The latter two are already explicit
shared-HTTP compatibility limits; Python's polling helper substitutes a default.
These are not new waiver labels or edited expected values. The remaining 15
raw-factory quirks continue to execute in Python, including invalid rows and
silently omitted 201 continuation data.

## Executed evidence: October 24, 2026

The isolated capacity lane executed the commands below under coordinator slot
`capacity-parity-20261024-a` on Windows x64 with Rust/Cargo 1.99.1,
host `x86_64-pc-windows-msvc`, LLVM 23.1.1, and Python 3.13.15. Execution used
`CARGO_BUILD_JOBS=1`, a session-private target directory, offline locked Cargo,
and one test thread. The base remained exact `4e822a3`; later item/CLI lane
commits were not merged or validated by this lane.

| Command from `rust` | Actual result |
| --- | --- |
| `cargo test --offline --locked -p fabric-sdk-capacities -p fabric-sdk -p fabric-sdk-http -- --test-threads=1` | 47 non-doc tests and one compiled doctest passed; rerun after the test-only Clippy repair passed. |
| `cargo test --offline --locked -p fabric-cli --test workspace_contract -- --test-threads=1` | Eight architecture tests passed, including actual discovered-crate/layer/edge checks. |
| `cargo clippy --offline --locked -p fabric-sdk-capacities -p fabric-sdk -p fabric-sdk-http --all-targets -- -D warnings` | Passed after collapsing one nested test `if`; no suppression or expectation changes. |
| `python -I -B tests\compatibility\capacities\reference.py --check` | All 35 immutable actual-source observations passed. |
| `python -I -B tests\compatibility\capacities\test_capacity_reference.py -v` | Nine tests passed: every golden repeated twice, 20 observation mutations rejected, source/fixture drift rejected, offline boundary/import/I/O/script guards passed. |
| `cargo fmt --all -- --check` and `git diff --check` | Passed. |

The capacity-specific Rust tests are 11 standalone public-consumer tests, one
facade test, and five shared-HTTP consumer tests; the command also retains and
passes existing facade/HTTP tests. Default-boundary tests execute actual exact
and +1 values for 100 pages, 100,000 rows, and 4 MiB, not only small proxies.
No existing Python source, workspace/Notebook/deployment/identity source, or
preexisting workspace/HTTP expected values changed.

All owned validation processes had ended by 17:31:09 UTC, verified by a fresh
process check, and the coordinator slot was explicitly released. No further
heavy work is implied by the recorded reservation expiry.

These are targeted local results, not full integrated-workspace evidence.
Windows execution does not imply native Linux/macOS, real Fabric, a selected
HTTP/TLS backend, live authentication, enterprise policy, CLI output/collection,
ARM or full Python parity.

<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Generic item LIST/GET compatibility matrix

This bounded SDK slice follows the handwritten REST decision, not a generator
or full Python/CLI parity claim. Reference source is Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`; implementation starts from exact
shared-HTTP checkpoint `4e822a3b760214f37e789d8a016e8cd7635f2f7f`.
See the [item SDK](../crates/fabric-sdk-items/README.md) for API semantics and
the [independent source oracle](../tests/compatibility/items/README.md) for
source digests, case inventory and observed quirks. Existing workspace eight-case
goldens, their source hashes and expectations are untouched.

| Contract | Independent evidence | Executed Rust gate |
| --- | --- | --- |
| Exact method/path/query/token/scope | 30 source-executed core cases | 28 exact comparisons, two explicit active-origin exceptions |
| Python query punctuation, not WHATWG encoding | Additive `query-encoding.json`, actual `do_request` | Exact query key/value URL comparison |
| Empty/order/duplicates/Unicode/future/missing/null type/extra JSON | Hand-authored core rows, no output regeneration | Exact row serialization comparison |
| Header token precedence/empty suppression, query replacement | Core paging cases | Exact URL sequence and token encoding |
| Three safe pages with empty middle/duplicate row | Source core records Python behavior separately | Native safe-origin traversal and query retention |
| 401/403/404/header/body/auth error precedence | Source errors, raw Python exception/code/message/request ID | Exact status-specific code, retained structured error and headers |
| Malformed pages and service failure after committed page | Source quirks separate from typed policy | Explicit typed failure and committed partial rows/token |
| Wrong origin/path/port/userinfo/fragment/dot segments/token cycles | Explicit Rust safety exceptions, not Python parity | Failure before next request; no successful incomplete result |
| Nonzero 100-page/100,000-row/4-MiB defaults | Explicit Rust safety exceptions | Actual exact-default and +1 traversal/item/byte cases |
| Cancellation before auth, in-flight credential/request, between ready pages | Injected futures and drop guards | Cancellation priority, fair yield, no extra request, partial progress |
| Shared HTTP execution | Existing HTTP source traces remain unchanged | Item adapter: GET retry/recovery, headers/query, byte bound, deadline and retry exhaustion |
| Standalone and stable facade | Same downstream consumer scenario compiled twice | LIST and GET with same injected provider; no identity/deploy/CLI edge |
| Broader CLI/types/definitions/live deployment | Inspected callers only | Not implemented or claimed |

## Measured local execution

Windows `x86_64-pc-windows-msvc`, Rust 1.99.1
(`58cf8f9c35b355199727627d07ae34a4e2846d67`), Python 3.13.15.
The named assigned host slot uses one sequential heavyweight process tree,
`CARGO_BUILD_JOBS=1`, private `CARGO_TARGET_DIR`, and `--test-threads=1`.
Only existing offline registry packages were used; lockfile changes are the
local `fabric-sdk-items` package and exercised internal edges, not external
versions/features. No full workspace build or full Python coverage is claimed.

Commands from `rust/` (Python commands from repository root):

```text
cargo test --offline --locked -p fabric-sdk-items -p fabric-sdk-http -p fabric-sdk -- --test-threads=1
cargo clippy --offline --locked -p fabric-sdk-items -p fabric-sdk-http -p fabric-sdk --all-targets -- -D warnings
cargo test --offline --locked -p fabric-cli --test workspace_contract -- --test-threads=1
cargo fmt --all -- --check
python -I -B rust\tests\compatibility\items\reference.py --check --include-python-only
python -I -B rust\tests\compatibility\items\test_reference.py -v
```

Measured first complete native gate: **44 non-documentation tests + one existing
doctest passed**, including ten grouped item unit tests, standalone and facade
consumers, two new HTTP adapter tests and all existing selected tests.
Eight architecture contracts, strict targeted all-target Clippy, and formatting
passed. The Python runner passed all 56 original cases; seven guard tests also
passed, including the additive punctuation source golden. An initial compile
failure from an unqualified private helper was corrected before these results;
no ignored/deleted/rewritten pre-existing tests were used to obtain a pass.
Final commit identity and any subsequent validation are recorded in the
integration handoff rather than a self-referential commit hash here.

No provider CI, Linux/macOS execution, production transport, live schema,
real authentication, broker/proxy behavior, CLI output/exit parity or release
readiness is established. The original independent Python cases retain quirks
such as `total_pages: 1`, silently discarded followed 201 pages, raw unsafe path
interpolation and generic GET object pagination. Rust safety/typed exceptions
are explicit; neither side's expectations are rewritten to conceal them.

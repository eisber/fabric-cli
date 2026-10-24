<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Fabric CLI query projection

`fabric-cli-query` is a pure CLI-layer library translating
`utils/fab_jmespath.py::search` at Python commit
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. It implements actual JMESPath
evaluation, not deployment JSONPath and not a parser-only proof.

```rust
use fabric_cli_query::{project, search};
use serde_json::json;

let rows = json!([{"id":"a","name":"Alpha"}, {"id":"skip","name":"Ignored"}]);
assert_eq!(
    search(&rows, "[?id != 'skip'].{workspace: name, key: id}")?,
    json!([{"workspace":"Alpha","key":"a"}]),
);
assert_eq!(project(&rows, None)?, rows);
# Ok::<(), fabric_cli_query::QueryError>(())
```

## Public contract

| API/input | Result |
| --- | --- |
| `search(&Value, &str) -> Result<Value, QueryError>` | Source-compatible search wrapper |
| `search(data, ".")` | Original JSON value, including scalar types |
| `search(data, "")` | Ordered JSON path inventory; source maximum depth four |
| `search_with_options(data, "", SearchOptions { deep_traversal: true })` | Complete path inventory |
| Ordinary object/list result | JSON tree with original/expression insertion order |
| Ordinary scalar result | JSON **string** containing Python `str`: `None`, `False`, `0`, `1.0`, raw Unicode, etc. |
| `project(data, None)` | Original JSON, distinct from `Some("")` |
| `join_query_args(&[impl AsRef<str>])` | Join tokens with spaces, then strip edge single/double quotes exactly like the Python list branch |
| `QueryError::code/message/kind` | Source `InvalidInput` and public message; typed syntax/type/arity/function/evaluation/unsupported/resource-limit diagnostics |

No global CLI state, I/O, credentials, timers, async runtime, SDK, HTTP client,
or process launching is introduced. Errors never return the original input,
`null`, or an empty list as a successful fallback. Error diagnostics do not
retain query/data text. Mutation/`replace` and simple-path mutation validation
are not part of this crate.

The standalone **public consumer**
[`examples/projection_output.rs`](examples/projection_output.rs) applies queries,
feeds the result into the existing `fabric-cli-output::OutputEnvelope`, writes
its `RenderPlan` into caller-owned streams, and reports actual bytes and exit
codes. It exercises no-query, projected scalar/list/object, and error branches.
There is no second CLI envelope implementation here. Main/parser/command
composition remains separately owned.

Callers must retain command-specific policies. Python `ls` skips search for an
empty query and selects its default columns; `get` calls search to list paths;
`api` retains its original payload when a projected list is empty. `project`
does not silently choose one of those policies.

## Dependency choice and adapters

The exact dependency is **`jpx-core = 0.3.1`, default features disabled**.
It implements JMESPath directly over `serde_json::Value`; the existing workspace
`preserve_order` and `float_roundtrip` features remain intact. No `extensions`
or `let-expr` feature is enabled. All 26 Python-standard JMESPath functions are
registered; unknown/extension functions fail.

Published August 13, 2026, the crate has nine published versions, declares Rust
1.90, and is licensed **MIT OR Apache-2.0**. The acquired source records upstream
commit `b25f7dc27392d9e18479d9954583a73c5624f937`, path `crates/jpx-core`;
the lock checksum is
`d7932defff2fb2ccc4a5c4ef9e5108f880333d1cf3dbd82b47b4452d85b80705`.
This is a younger, less widely used implementation than `jmespath`, not a claim
of equivalent adoption or blanket Python compatibility.

Bounded selection evidence:

| Candidate | Source finding | Decision |
| --- | --- | --- |
| `jmespath 0.5.0`, MIT | `Variable::Object` uses `BTreeMap`; key/value/wildcard/hash order differs from Python | Not selected |
| `jmespath_community 0.1.2`, Apache-2.0 | `Value::map_from_json` converts every number to `f64`, losing integer type and precision | Not selected |
| `jpx-core 0.3.1`, MIT OR Apache-2.0 | Direct ordered `Value`; native PoC passed filters/pipe/expref/hash/order and exact `1.0`/`9007199254740993` preservation | Selected with counted compatibility adapters |

Eight standard functions have narrow adapters: `abs`, `ceil`, `floor`, `sum`,
`avg`, `to_number`, `to_string`, `contains`. They reuse the library's typed
signature/arity validation and evaluator. Private comparison callbacks retain
Python's distinction between outer bool/number equality and nested Python
container equality and guard numeric operands before lossy library evaluation.
They are installed by rewriting the library's parsed AST,
not by parsing/evaluating a new query language, and cannot be invoked by users.

The scalar formatter follows the existing output crate's private Python float
encoder policy; its public API does not expose that helper, and this task does
not change its separately owned surface. Compact `to_string` uses ASCII escapes
and Python exponent spelling, not Rust `Display`/ordinary JSON defaults.

The lock adds eight external packages: `jpx-core`, `serde_spanned`, `toml`,
`toml_datetime`, `toml_parser`, `toml_writer`, and two versions of `winnow`.
The seven transitive additions are **build-time** dependencies of the upstream
crate. Its packaged `build.rs` generates registry/documentation/compliance-test
metadata into Cargo `OUT_DIR`. No generator toolchain, REST/schema generator,
generated product endpoint, FFI, or Python runtime bridge is added. Optional
extension dependencies (time, random functions, network-related helpers) stay
disabled. Existing locked packages are not updated.

Sources: [release metadata](https://crates.io/crates/jpx-core/0.3.1),
[public API](https://docs.rs/jpx-core/0.3.1/jpx_core/),
[official value source](https://docs.rs/jmespath/0.5.0/src/jmespath/variable.rs.html),
[community value source](https://docs.rs/jmespath_community/0.1.2/src/jmespath_community/value.rs.html).

## Evidence and limits

See the [compatibility matrix](../../docs/query-compatibility.md) and
[independent oracle](../../tests/compatibility/query/README.md). The fixture
inventory contains **118 cases: 108 native-exact and 10 explicitly unsupported**.
These are bounded coverage labels, not a claim of complete Python parity.
Every unsupported case retains its actual Python expectation and must produce
a typed native error plus the existing failure envelope, never empty success.

Native evaluation still has documented numeric, legacy-literal, expression-
reference, and extreme-index/depth limits. Input is the finite, valid-Unicode
`serde_json::Value` domain, not arbitrary Python objects or unbounded integers.
No production executable command, auth, live request, or mutation is provided.

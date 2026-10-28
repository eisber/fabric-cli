<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# CLI query projection

`fabric-cli-query` evaluates JMESPath expressions over ordered JSON values.
Deployment JSONPath mutation is a separate API.

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

## Query behavior

| Input | Result |
| --- | --- |
| `search(data, ".")` | Original value, preserving scalar types |
| `search(data, "")` | Ordered property-path inventory, limited to four levels |
| Empty search with `SearchOptions::deep_traversal` | Complete property-path inventory |
| Object or list expression result | Ordered JSON tree |
| Scalar expression result | Display string encoded as a JSON string |
| `project(data, None)` | Original value without evaluation |

`join_query_args` joins tokens with spaces and strips surrounding quote
characters. An absent query remains distinct from an explicitly empty query.
Command handlers choose whether empty input means default columns or path
discovery; the query library does not choose that policy for them.

## Errors and limits

`QueryError` exposes a public code, message and typed classification for syntax,
type, arity, function, evaluation, unsupported-domain and resource failures.
Diagnostics do not retain input data or query text. Errors never return the
original data, null or an empty list as successful fallback.

The evaluator uses `jpx-core` with extensions disabled and preserves the
workspace's JSON ordering and numeric precision policies. Mutation, arbitrary
runtime objects and values outside the supported JSON domain are not supported.

The `projection_output` example composes query results with the shared output
envelope and caller-owned streams. The crate performs no I/O, authentication,
SDK requests, process launching or runtime initialization itself.

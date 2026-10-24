<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Independent CLI query reference

`reference.py` executes the actual Python `search`, `_get_json_paths` and
`process_nargs` definitions from source commit
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` with installed
`jmespath==1.0.1`. Complete original definitions are selected by AST only to
remove product imports and their ambient initialization; their algorithms are
not reimplemented. Git blob hashes reject source drift before execution.
The existing output reference supplies real error/constants/output classes.

`cases.json` is a hand-authored, frozen synthetic inventory derived from CLI
queries/documentation and explicit edges. Expected values are never generated
from Rust or rewritten by the harness. Dictionary order and integer-vs-float
types are part of comparisons, not only Python `==`/JSON-tree equality.
The old workspace `query_projection` fixture is independently consumed unchanged
by the Rust public-consumer test.

## Running

On a shared host, run these commands only through the owner's granted common
validation gate. Use an existing read-only oracle interpreter, or an isolated
artifact-directory venv after a missing-dependency failure. Never install into
a peer environment or make Python a product dependency.

From `rust`, with single-threaded Cargo and a private target:

```powershell
cargo test --locked --offline -p fabric-cli-query -p fabric-cli-output
cargo run --locked --offline --quiet -p fabric-cli-query --example projection_output |
    Set-Content -Encoding utf8NoBOM $observations
& $python -B tests\compatibility\query\reference.py --observations $observations
& $python -B -m unittest discover -s tests\compatibility\query -p test_reference.py
cargo clippy --locked --offline -p fabric-cli-query --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test --locked --offline -p fabric-cli --test workspace_contract
```

The Rust example is a standalone public consumer, not a hidden test-only adapter:
query result -> existing output envelope -> render plan -> caller-owned streams.
The reference reads its observation file; it never launches Rust/subprocesses
or writes a baseline. UTF-8 output/observation handling must be configured by
the caller on Windows. `-B` prevents Python cache writes.

## Coverage and safeguards

The inventory contains **118 cases**: **108 native exact** and **10 unsupported**.
All 26 standard functions have comparisons. Each case independently checks
the original Python output against its frozen expectation. Supported native
cases must match projected values plus full envelope bytes/exit/streams.
Unsupported cases must retain their original Python results, but native
execution must produce a typed unsupported error and the source-shaped failure
envelope. They never count as native exact. The [matrix](../../../docs/query-compatibility.md)
lists precise limits.

`test_reference.py` additionally executes complete real `ls`, `get` and `api`
query-call-site branches with in-memory collaborators. These preserve the
different absent/empty/query-result-truthiness policies; they do not simulate
network, auth, file export or full command execution.

An audit guard is active during actual comparison and test evaluation. Negative
controls prove that network sockets, credential/registry access, subprocesses,
disk writes, sleeps and source imports fail. Read-only source/fixture access is
permitted. Controlled source imports never import the product package. Frozen
timestamps and repeated observations protect determinism; source-hash, native
tree/byte corruption, incomplete inventory, input mutation and unsupported-
relabelling controls protect the oracle. The audit guard is a development
safeguard, not a sandbox for arbitrary untrusted Python.

## Initial measurement and provenance

The first real wrapper/output comparison found ten mismatches in the selected
library: integer `abs`/`ceil`/`floor`/`sum`, empty sum, relaxed Python
`to_number`, ASCII/float `to_string`, and bool/numeric or invalid-operand
`contains`. Narrow native adapters address those without weakening expectations.
See the [crate README](../../../crates/fabric-cli-query/README.md) for exact
dependency/version/license/checksum, selection evidence and build-time notices.

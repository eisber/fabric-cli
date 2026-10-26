<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Independent job-family expectations

`cases.json` is hand-authored from Python source
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`, not generated from Rust.
`reference.py` uses only the Python standard library. Before executing selected
functions, it checks their source AST against `git show` at the exact pin.
It imports no product initialization and uses synthetic HTTP, time, UI and
configuration boundaries. No credential, cache or service is accessed.

Run from the repository root:

```text
python rust/tests/compatibility/jobs/reference.py
```

The cases cover execution-data construction, schedule configuration, every
Python job API function, actual command-handler outputs and the polling
algorithm. Failed/unknown status tables, no-envelope terminal states,
cancel-on-timeout and schedule-removal confirmation are independent contracts.
Existing command inventory, HTTP and other-family fixtures remain unchanged.

Rust public integration tests consume the same expectations through the real
job SDK, shared HTTP executor, injected synthetic HTTP and real CLI handlers.
`support.rs` supplies only offline credentials, HTTP and a deterministic
caller-owned timer; it is not a production backend or successful service stub.
An unexpected request, including mutation replay, fails the test.

Resumed branch work adds `branches.json` and `branches_reference.py`; the
released `cases.json` and `reference.py` are unchanged. The additive source
suite inventories all 44 pinned item types, executes the unchanged hierarchy
support functions for every type and job leaf, and covers additional parameter,
schedule, retry-header and wrapper-progress cases. Native tests exercise the
same values through the actual handlers and shared executor, rejecting
unsupported resources before HTTP.

`branch_ledger.py --output <absolute-json-path>` runs both unchanged-source suites
and records exact pinned file/function/line decisions, raise expressions,
measured source lines and observed line arcs beside the immutable full argument
inventory. Decision-line observation is not full branch coverage, byte-for-byte
parity or proof that shared host/numeric/header integration is complete.

`operation_output.json` adds 25 independent generic-operation error contracts.
`operation_output_reference.py` executes the released, unchanged Python
HTTP/exception/decorator oracle over those values rather than implementing
another Python representation formatter. The native `operation_output` tests
exercise actual job cancellation and timeout cancellation through the shared
HTTP/operation engine, then the borrowed shared CLI formatter. They cover
Failed/Cancelled structured, list, scalar, Unicode and null errors, HTTP
authorization/code precedence and a period-only message that must stay empty
after its first normalization. Original SDK reconciliation evidence remains
inspectable after repeated output projections.

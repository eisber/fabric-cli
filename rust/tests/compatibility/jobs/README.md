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

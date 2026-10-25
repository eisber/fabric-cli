<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Python read-retry reference

`retry-cases.json` contains seven hand-authored synthetic traces. The Rust
`fabric-sdk-http` tests and `reference.py` both check these expectations.
There is no record/update mode. Existing workspace goldens remain unchanged.

The reference extracts only `_get_session` from Python source checkpoint
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4` and checks its normalized AST hash
before execution. It instantiates the real Requests HTTPS adapter and uses
urllib3's retry classification, increment, Retry-After and backoff methods
against synthetic responses. It never sends HTTP, requests credentials, imports
the product, sleeps, or changes configuration. Requires an existing development
interpreter with Requests/urllib3:

```text
python -B rust/tests/compatibility/http/reference.py
```

This validates the HTTPS status-retry policy, not Requests socket behavior or
the outer `do_request` loop. Rust separately tests that loop's four headerless
429 responses and four ten-second waits. The reference does not establish
HTTP-date Retry-After, I/O retry, redirect, TLS/proxy, or CLI parity.

The initial execution used Python 3.13.15, Requests 2.34.2 and urllib3 2.8.0.
Dependency differences must be reported, not hidden by changing expectations.

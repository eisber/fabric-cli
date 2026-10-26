<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Independent raw API source oracle

`reference.py` executes the pinned Python `fab_api` key/value builders,
`fab_api_request.exec_command` and `fab_api_client.do_request` with synthetic
credential/context/session boundaries. It imports no product/auth module.
The actual source origin/audience branches, request URL/body, scope calls,
status/header/text/query output and fixed-time JSON output classes supply the
17 expectations in `cases.json`. No Rust result is used to produce expectations.

The initial capture refuses overwrite. Normal execution only compares. The source
HTTP session/user-agent boundary is injected, so these fixtures do not establish
Python dynamic host/user-agent/transport behavior.

Rust's actual shared `execute_bound` executor now exercises all four fixed origins,
source audience bindings and safe headers. Additional tests cover every source
method for each origin and reject wrong-scope/auth/host/cookie/proxy overrides
before credentials. Existing source expectations are unchanged.
Multipart/local-file and injection/bounds tests are additional Rust safety cases.
They are not an independent requests-library multipart serialization oracle.

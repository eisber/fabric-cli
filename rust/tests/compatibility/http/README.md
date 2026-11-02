<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# HTTP retry contracts

`retry-cases.json` defines fixed synthetic response and delay traces.
The reference and native HTTP tests check the same status-retry, retry-count,
Retry-After and backoff expectations without sending requests.

The development reference requires the pinned Requests/urllib3 environment:

```text
python -B rust\tests\compatibility\http\reference.py
```

Execution does not acquire credentials, sleep or change configuration.
Normal runs compare expectations without rewriting them.

Socket behavior, TLS, proxies, redirects, I/O retries and HTTP-date parsing
require separate transport tests. The shared executor additionally checks
headerless throttling, finite deadlines, streaming bounds and no mutation replay.

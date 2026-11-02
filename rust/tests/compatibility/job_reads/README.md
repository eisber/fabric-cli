<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Job read contracts

The job-read suite verifies status and list responses through metadata
discovery, resource routing, projection, JSON/text output and skill headers.
Fixtures are synthetic, fixed and independent of native observations.

From `rust/`:

```text
python -B tests\compatibility\job_reads\compare.py
```

Add `--probe <job_read_session_probe executable>` to compare a built native
parser/session. The runner does not build the probe; retain its exact source
and feature receipt separately.

Reference execution uses injected credentials, HTTP, UI and time.
Native consumers use the actual parser, session, SDK and shared HTTP adapter.
Assertions check scope and request order as well as output and exits.

Resource safety, raw response domains and operation reconciliation have
separate tests. These observations do not establish persistent cache behavior,
live-service authorization or complete interactive terminal support.

<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Read orchestration observations

This harness records LS/GET discovery, metadata enrichment, request ordering,
query execution and output independently of formatting-only tests.
The outer GET handler includes connections and schedules; the unprojected result
is retained so a matching scalar cannot conceal omitted requests.

Credentials, HTTP, current context, time and terminal capabilities are injected.
Unscripted I/O is fatal, including attempts caught by optional-read handlers.
No configuration store, real account or service is accessed.

From the repository root:

```powershell
python -B rust\tests\compatibility\orchestration\capture.py
python -B -m unittest discover -s rust\tests\compatibility\orchestration -p test_capture.py
```

Optional native comparison accepts `--rust-probe <read_session_probe>` and
`--rust-pin <full-build-commit>`. The probe uses the actual parser, session, SDK
and shared HTTP executor. The supplied pin is a label, not a binary hash or
build receipt; retain those separately.

Use repeated `--case` selectors for a focused observation. Baseline differences
remain findings, not automatically approved expected values. Request counts
alone do not establish timing or performance improvements.

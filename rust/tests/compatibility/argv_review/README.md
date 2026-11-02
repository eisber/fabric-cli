<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Argument capture harness

`capture.py` observes parser registration, validation, help, diagnostics and
dispatch selection across jobs, local commands, reads, resources and transfers.
Accepted handlers are identified but not executed.

The harness uses synthetic UI and logging boundaries. It does not open a
browser, authenticate, read application configuration or call services.
Group-help events and headless leaf-help output are distinct from full
interactive terminal rendering.

From `rust/`, select a new absolute output path:

```text
python -B tests\compatibility\argv_review\capture.py --output <new-absolute-json-path>
```

`--rust-binary <exact-build-fab-rs>` additionally records executable streams
and exits with inherited identity/configuration inputs cleared.
The runner does not build the binary or prove its provenance; retain a separate
build receipt. Output files refuse overwrite.

The optional source archive must match the harness's recorded identities.
Digests alone do not authorize replacing expected behavior.

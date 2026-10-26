<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native job read boundary

`cases.json` contains new, hand-authored synthetic expectations from Python
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. The original job, output, context
and read oracles are unchanged. There is no native recording/update mode.

From `rust/`, use `python -B tests\compatibility\job_reads\compare.py` for
source-only validation. Add `--probe <job_read_session_probe executable>` to
compare the production native parser/session against source observations.
Build the development executable using the normal `fabric-cli --examples`
target. No production Python dependency or fake CLI flag is added.

The runner executes actual unchanged job API/handler/projection/UI definitions
using the existing protected AST/blob-pinned source loaders. It asserts
hand-authored output trees, then freezes source stdout/stderr/exit before
starting the native process. Sixty exact cases cover three response branches,
five resource types, two route selections and JSON/text. Four API-gap cases
retain their distinct source expectations and require explicit native failure,
not successful partial parity.

Requests include independently specified native metadata discovery followed by
the actual source job route. Scope checks require Fabric only. This is not
Python cache/request-count parity, a full source context invocation, source
HTTP-operation reconciliation, service validation or whole-command acceptance.
See [native job read limits](../../../docs/native-job-reads.md).

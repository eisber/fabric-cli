<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Integration validation baselines

These are dated measurements, not target thresholds or parity claims. The
integration checkpoint brings together submitted build, identity, extension,
workspace-reference, and deployment-fixture handoffs from the
`eccfca1d3b1cb4df71d0fa65a76221a588095290` foundation. Python production source
and existing tests are unchanged. In-progress SDK code and coverage-improvement
work remain in their owning branches, not copied from peer worktrees.

## Existing Python unit coverage

The independent coverage session's `validation-report.json` was read during
integration. Its trace covers Python source
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`; the source and test Git tree IDs match
the Rust foundation. This is the October 22 baseline, not a measurement of any
later coverage improvements.

| Measure | Observed baseline |
| --- | --- |
| Host/runtime | Windows x64, Python 3.13.15; pytest 9.1.1, pytest-cov 7.1.0, coverage 7.16.2 |
| Suites | `tests/test_core`, `tests/test_utils`, `tests/test_parsers`, `tests/test_main.py` |
| Result | 677 collected: 658 passed, 1 failed, 18 existing skips; exit 1 |
| Pytest duration | 24.57 seconds, one reported final invocation; not a benchmark distribution |
| All-source denominator | 257 tracked Python files |
| Statement coverage | 5,436 / 12,607 = 43.1189% |
| Branch coverage | 970 / 3,536 = 27.4321% |
| Combined coverage.py measure | 39.6828%; distinct from statement and branch percentages |
| Packaging smoke | Wheel build, non-editable install, help and version passed |

The failed test is
`tests/test_utils/test_fab_ui.py::test_print_error_format_text_success`.
The report diagnoses a headless-Windows `NoConsoleScreenBufferError` path:
the real fallback omits the marker/command prefix expected by that test.
Neither the test nor production behavior was altered to make it pass.
POSIX permission and no-console exclusions account for the 18 existing skips.
This is usable coverage data but **not an all-green test baseline**.

Default reporting discovered only 205 files, omitting 52 unimported files in
namespace directories. Reporting the unchanged trace against an explicit tracked
source inventory includes all 257 files. Do not compare a future percentage
using a smaller denominator and call that an improvement. Preserve raw trace,
command/config, source inventory, per-file data, and numerator/denominator
alongside the percentage. Coverage is not correctness or Rust parity.

Raw measurement evidence remains in the owning coverage session's artifacts:
`validation-report.json`, `coverage-per-file.csv`, `attempt-03/.coverage`,
`attempt-03/junit.xml`, and `all-source/coverage.json` and HTML report.
Do not commit private machine/session paths or copy raw logs into public source.

## Independent workspace reference

The eight immutable cases target the pinned Python source above. Integration
validation uses Windows x64 and an existing isolated Python 3.13.15 environment;
the golden data and source digest are unchanged.

```text
python -I -B rust\tests\compatibility\workspaces\test_workspace_reference.py -v
python -I -B rust\tests\compatibility\workspaces\reference.py --check
```

The harness controls auth, transport, environment, import-time config-directory
creation, clock, local hostname, and headless terminal backend. It executes real
Python workspace, pagination, context, projection, error and JSON helpers. The
product's real headless fallback prints exact LF JSON; successful terminal
renderer output, including CRLF, is **not** an equivalent tested boundary.
Line-ending drift is rejected explicitly rather than normalized.

On October 23, 2026, all **13 unit tests passed**. The unit runner covers the
eight cases plus repeatability, five side-effect
probes, ten contract mutations, rejected source drift, and rejected line-ending
drift. The standalone check also exercises all eight cases. This is a sampled
Python helper oracle, not a full CLI/parser test, Rust adapter, or cross-platform
terminal proof.

## Deployment reference leaves

The 44 synthetic cases execute selected definitions from
`microsoft/fabric-cicd@5ca096c00a9a878c646c59fe537b883eba951329` without importing
the service package or constructing a real workspace. Integration validated all
44 on Windows x64, Python 3.13.15, PyYAML 6.0.3, and jsonpath-ng 1.9.1.

```text
python -I -B rust\tests\fixtures\deploy\reference_oracle.py --reference-root <pinned-checkout>
```

The initial environment had jsonpath-ng 1.7.0 and was rejected as expected. A
separate integration-owned virtual environment restored the exact declared
oracle versions; no shared coverage environment, fixture expectation, source,
or dependency baseline was changed. The successful JSON report includes source
and fixture hashes. Preserve it as a session artifact. This proves sampled
Python leaf behavior only, not a native deployment implementation.

## Native Rust and remaining gates

The [native build evidence](build-validation.md) records the prior three-host
foundation run. Integration locally passed metadata, formatting, all-target
check, Clippy with warnings denied, 12 Rust tests, the documentation command
(zero doctests), the offline credential example, and CLI help/version on
Windows x64 with the pinned Rust 1.99.1 toolchain.

The new workflow resource guards need their own exact-head
provider result after the integration push; a green earlier head does not prove
this checkpoint. Local Cargo checks run from `rust/` with the pinned toolchain.

Production interactive authentication, live service behavior, native deployment,
successful terminal rendering, release packaging, signing/notarization, and
unexecuted architectures remain separate gates. Identity evidence is not a
backend selection. First-party extension consumer priority does not select a
plugin runtime. Observers assess goal/evidence gaps; they do not accept work.

The Tachikoma wiki index `tk:wiki/12/871` and forum `tk:forum/12/4` hold reference
learnings and lane baseline tables. Their contents and saved message receipts
are not execution authority or proof another agent has processed a handoff.

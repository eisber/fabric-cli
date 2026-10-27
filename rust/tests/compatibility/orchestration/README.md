<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# LS/GET orchestration audit

This additive inventory compares orchestration, not formatting, parsing policy,
identity selection or live performance. Python is independently pinned to
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`; the reviewed Rust source baseline is
`03fab20577a1016178de227eac0ee178921bb8d0`. Old fixtures, source pins, dependencies
and expectations remain unchanged.

`capture.py` checks the complete existing 259-file Python source digest, executes
the original public argument registration, path resolver, cache/memstore,
handlers, API/HTTP, synchronous operation polling, query and JSON UI definitions.
Only current context, credentials, HTTP, time and terminal capabilities are
synthetic. GET executes the **outer item handler**, including connections and
schedules, not just `get_item_with_definition`. Every item query in this inventory
is a metadata property, so no definition export or confirmation is needed.
The capture also retains the unprojected return object: matching `description`
output does not conceal omitted enrichment requests.

The small offline capture requires the already established `cachetools` and
`jmespath` source-reference dependencies. It does not import the product, touch
configuration or credential stores, use a real account, sleep, build or install.
Unscripted I/O is fatal even if source optional-read handling catches it.
It does not execute Python main, persisted-context restoration or command-support
configuration. The 429 UI uses actual source `print_info` with a synthetic
non-color terminal; logger progress is recorded separately as events.

From the repository root, using an existing approved Python environment:

```powershell
python -B rust\tests\compatibility\orchestration\capture.py
python -B -m unittest discover -s rust\tests\compatibility\orchestration -p test_capture.py
```

With an explicitly granted CI/build resource, from `rust`, the smallest native
consumer build is:

```powershell
cargo build --locked -p fabric-cli --example read_session_probe
```

Then from the repository root:

```powershell
python -B rust\tests\compatibility\orchestration\capture.py --rust-probe <private-target>\debug\examples\read_session_probe.exe --rust-pin <exact-built-40-character-commit>
```

The existing probe uses the real public Rust parser, `ReadSession`,
`with_source_operations`, SDK endpoints and shared `HttpExecutor`. There is no
replacement request engine or expected-output callback. `--rust-pin` records the
caller's claimed build provenance; **it does not verify a binary hash or establish
a build receipt**. CI must retain those separately.

Inputs declare Python and baseline-native response sequences separately because
the native baseline does not send every source request. Observations are
**evidence, not automatically approved expectations**. In particular, baseline
sequences for REQ-007/008/015/016 intentionally expose an error or missing call.
A repair must add source-exact regression inputs/assertions rather than trim
the source expectations or count the baseline divergence as a pass.
Use repeated `--case REQ-015` selectors for a bounded capture.

The workstream documents `rust/reviews/20261027/orchestration-findings.json` and
`orchestration-table.md` distinguish actual source observations, static Rust
proof and unexecuted native verification. No timing/speedup claim follows from
request-count differences. Approved origin, no-mutation-replay, bounds and
explicit identity-selection policies are not repair targets.

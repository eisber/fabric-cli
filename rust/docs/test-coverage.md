<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Stable Rust test coverage

Coverage is measured by executing instrumented code, not by counting tests or
registered commands. The pinned Rust **1.99.1** toolchain and
**cargo-llvm-cov 0.9.1** measure lines, functions and regions. Branch coverage and
instrumented doctests are **unmeasured**: they require nightly instrumentation,
which is not enabled. Ordinary documentation tests remain a separate gate.

## Reproduce a full native measurement

Run under the shared host's exclusive build-slot wrapper, with the native
compiler environment already configured. Keep the checkout unchanged until
the command finishes. Install the pinned tool only when missing:

```text
cargo install cargo-llvm-cov --version =0.9.1 --locked
rustup component add llvm-tools-preview --toolchain 1.99.1
node rust/scripts/rust-coverage.cjs --output <new-absolute-external-artifact-directory> --scope full --timeout-seconds 9000 --execute
```

Invoke Node from the repository root; the runner uses `rust/` for Cargo. On
Windows, the Visual Studio environment setup and this command must run in the
same process. `--tool <absolute-cargo-llvm-cov-path>` supports an isolated tool
installation. Omitting `--execute` prints a plan and measures nothing.

The full run executes `--workspace --all-features --all-targets` tests, keeps
individual target failures, and writes JSON, LCOV and HTML reports. Its
`receipt.json` records actual commands, exit codes, hashes, native rustc host,
toolchain, Git base, dirty state and the complete source inventory before and
after execution. `summary.json` and `summary.md` expose weighted covered/count
totals and package-level results. A changed source inventory invalidates the run.

The manual/weekly [coverage workflow](../../.github/workflows/rust-coverage.yml)
uses the same runner on Windows, Linux and macOS. A configured workflow is not
evidence of a successful run. Results from one OS do not certify another.

## Denominator and failure policy

Only compiled workspace files under production paths are counted. Standalone
`tests`, `examples`, generated `target` files and external dependencies are
excluded. **Inline unit-test code in production source is included** and is
labelled in every report. Target-cfg-eliminated code and uncompiled source are
unmeasured, not silently counted as zero-covered lines.

Percentages are `sum(covered) / sum(count)`, never averages of package
percentages. A narrowed `--scope packages --packages name,...` run is labelled
package coverage, not full-workspace coverage. Empty measurements, malformed
counts, unknown packages, duplicate files and unexpected source paths fail.

Failing tests may produce a **partial diagnostic report**, but the command
still fails. Retain the receipt and failure logs; do not promote that report to
a passing validation gate. Do not combine overlapping test totals into a
coverage percentage or reuse historical results for a changed candidate.

The [command acceptance inventory](../validation/command-acceptance.json)
separately maps source commands, resource/option/failure branches and executable
test targets. Its validator does not execute tests or certify behavioral parity.

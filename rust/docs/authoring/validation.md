<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Native validation and availability evidence

## Preserve independent Rust builds

For every Rust change, identify the affected Cargo packages and keep each one
buildable by name from `rust/` with `cargo build --offline --locked -p <package>`.
Keep dependencies explicit in Cargo manifests. A package build may compile its
declared dependencies, but it must not rely on first building an unrelated
workspace package or the Python CLI.

When a change affects compilation or dependencies, validate each directly
affected package with a targeted build before running the workspace checks.
Follow the [contributor guide](../../CONTRIBUTING.md#build-an-individual-package)
for setup and shared-host build lease requirements. Package builds complement
workspace and native validation; they do not require separate CI jobs or
published artifacts.

## CI selection and native checks

Automatic PR and `main` push runs of `rust-ci.yml` are filtered to `rust/**`.
The Python `fab:build` workflow is filtered to `src/**` and `tests/**`.
Workflow-only or dependency files outside those directories do not trigger
either build. Filters consider the event's full diff, not only the latest commit.

GitHub does not support path filters on `merge_group`, so merge-queue runs retain
a lightweight diff selector and **Rust validation** aggregate, skipping native
jobs when no `rust/` files changed. Failed selection, an unknown base,
failed/cancelled native jobs, or a missing selected result cannot pass the gate.
Manual CI and unsigned-preview dispatch remain available for explicit checks.
Do not require the path-filtered Rust check unconditionally on unrelated PRs:
GitHub can leave skipped workflow checks pending. Where applicable, use the
aggregate check rather than individual matrix labels.

`rust-native.yml` is shared by CI and manual preview. Each native job runs
fmt/check/clippy/workspace tests/doc tests/public docs, executable metadata,
release build, packaging, and exact-bundle offline smoke. Its cache is partitioned
by runner OS/architecture, pinned toolchain, lockfile, and source/manifests.
Restoration may use registries; Cargo checks/tests/builds then run offline.
Behavioral tests must not use a live Azure/Fabric session. Source and script
checks do **not** substitute for these native gates.

The workflows have read-only contents permission, no secrets inheritance,
OIDC/signing/publication permission, or `pull_request_target`. Third-party actions
are pinned to full revisions. The manual preview workflow uploads unsigned
artifacts only; it has no release/push operation.

## Availability checked during authoring

On **2026-11-12**, the published
[runner-images inventory](https://github.com/actions/runner-images/blob/8ec03657a18e1821924600eb437f16c87d40c8b4/README.md)
listed:

- `windows-2022`: Windows Server 2022 x64.
- `ubuntu-22.04`: Ubuntu 22.04 x64.
- `macos-15`: macOS 15 **ARM64** (not Intel).
- `macos-15-intel`: macOS 15 x64.

The inventory states macOS 14 GitHub images retired on November 2, 2026; the
workflows do not use that retired label. The Intel-specific label is deliberate;
a generic macOS label must never be treated as evidence of x64 execution.
Inventory availability is **not proof** that this fork has scheduled and passed
jobs on all images; verify account eligibility, future retirement notices, and
actual run evidence before claiming coverage.

The Rust distribution endpoint returned HTTP 200 for
[`channel-rust-1.99.1.toml`](https://static.rust-lang.org/dist/channel-rust-1.99.1.toml).
Its manifest date was **2026-10-15**, reporting
`1.99.1 (58cf8f9c3 2026-10-13)`, with available `rust`, `rustc`, `cargo`,
`clippy-preview`, and `rustfmt-preview` for all four required triples:

```text
x86_64-pc-windows-msvc
x86_64-unknown-linux-gnu
aarch64-apple-darwin
x86_64-apple-darwin
```

This checks published component availability, **not installation or execution**
on each runner. CI explicitly installs 1.99.1 and fails if unavailable; it never
falls back to `stable` or changes the pin. `native_host.py` verifies actual OS,
machine architecture, `rustc -vV` host/release, and installed native target. A
cross-compiled binary is not native test evidence. It also rejects reported
Rosetta translation and Windows architecture-emulation mismatches.

A lightweight local host probe during authoring actually passed on macOS
**27.2 ARM64**, with native `aarch64-apple-darwin` rustc/Cargo **1.99.1** and the
target installed. It performed only metadata/version/OS queries, not compilation
or executable tests. This is evidence for that local host, not a hosted-runner
result or a minimum supported macOS version.

## Compatibility baselines are not yet established

No minimum supported Linux glibc, Windows release, or macOS deployment version
has been certified by native execution in this lane. Runner names are build-host
choices, not product minimums. No numerical baseline is inferred from them.

The host-evidence artifact records observed OS/image version, actual compiler,
architecture, Linux glibc or macOS version. Package provenance also records ELF
dynamic/version requirements (`readelf`) or Mach-O load commands and dependent
libraries (`otool`). Observed build-host glibc is not the binary's minimum glibc;
Mach-O deployment commands are not proof of successful execution on that OS.
Minimum-tested fields remain `null` until the integration/release owner tests
the **exact bundle** on candidate clean baseline hosts and records results.

The implementation authoring checks are lightweight script/source validation
only. Full combined-workspace and four-platform native evidence belong to
integration; official scans/signatures/notarization belong to the separate
trusted release process.

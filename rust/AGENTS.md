<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Rust migration contributor instructions

Read [migration guidance](docs/migration-guidance.md),
[architecture](docs/architecture.md), and [compatibility](docs/compatibility.md).
Repository-wide contribution and team-review rules still apply.

## Required working rules

- Translate existing behavior first. Submit redesigns, optimizations, bug fixes,
  new command namespaces, and plugins as separate decisions and changes.
- Record the Python/upstream source commit and complete behavior covered by each
  slice. Do not declare a port complete while omitting I/O, orchestration, failure
  paths, or consumers that belong to that approved slice.
- Preserve the independent oracle. Do not delete or weaken existing tests,
  regenerate expected outputs, change compatibility baselines, lower gates, or
  apply waiver labels to make a port pass without explicit owner approval.
- Work only in your assigned branch and file area. Do not copy unfinished peer
  worktrees, change another session's branch, or merge peer work unilaterally.
  Integrate only explicitly agreed commits through the integration owner.
- The shared foundation branch is `feature/rust-workspace-scaffold`. Use the
  exact agreed checkpoint, not an assumed latest version. Lane sessions develop
  on their own branches; only the integration owner advances the shared branch.
  Pushes, PRs, releases, and live operations require their own authorization.
- On a shared host, coordinate one heavy build/test job at a time. Do not infer
  an active lease from this file. Get an explicit grant from the integration
  owner; if messaging is unavailable, ask the owner rather than polling or
  inventing a lease. Read-only work can proceed while waiting.
- Read/format/narrow checks first; run the smallest tests covering the slice.
  Required full/native checks still gate integration; resource limits are not
  permission to omit validation.
- Keep dependencies and features centralized in `Cargo.toml`, preserve the lock,
  document public APIs, and respect workspace lints. Coordinate shared core,
  manifest, lockfile, toolchain, and architecture changes with integration.
- SDK consumers inject `azure_core::credentials::TokenCredential`. Libraries
  never prompt, exit, initialize a global logger/runtime, or use mutable CLI
  globals. Reuse the caller's async runtime.
- Tests stay offline and secret-free. Do not discover ambient credentials,
  implement static-token production fallback, persist plaintext credentials, or
  add a Python/az/azd runtime requirement. The production identity backend remains
  unresolved and requires team review.

Run the pinned checks from this directory, not the repository root. There is no
fixed crate count. Prefer focused crates that enable independent ownership,
implementation, and testing, with explicit interfaces and acyclic dependencies.
Crates under `crates/` are discovered automatically; declare each crate's
`package.metadata.fabric.layer` as described in the architecture guide. Coordinate
shared interfaces and cross-layer changes with integration; do not add empty
future modules or runtime loaders as scaffold shortcuts. `fab-rs` help/version/error
contracts are temporary bootstrap contracts, not proof of parity with Python `fab`.

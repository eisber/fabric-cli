<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Rust migration contributor instructions

Read [migration guidance](docs/migration-guidance.md),
[architecture](docs/architecture.md), and [compatibility](docs/compatibility.md).
Follow [agent-operated delivery](docs/agentic-delivery.md) for authorized task
execution, independent stage reviews, repair, and integration. Repository-wide
CLA, required checks, restricted-area review, and authority boundaries still apply.

## Autonomous task lifecycle

- Research authoritative sources and repository evidence before asking for
  clarification. Within an authorized task, proceed through planning,
  implementation, independent review, validation, and repair without routine
  human handoffs. Do not invent missing product requirements or permission.
- At every stage, use the two distinct review perspectives in
  [stage reviews](docs/agentic-delivery.md#stage-reviews). Reviewers examine the
  actual change and evidence independently of the authoring context. Required
  review must be real, not a self-review presented as independent approval.
- Bind findings and actual check results to the full tested revision and
  feature/platform scope. Revalidate the combined integration candidate;
  passing component checks do not prove the executable works.
- Repair blocking findings and have them independently verified. Unless the
  assignment is stricter, escalate after three failed repair cycles per blocker,
  counted across handoffs; honor all assigned time/cost/resource limits.
- Escalate only unresolved intent, missing authority or required evidence,
  unresolved material review disagreement, or exhausted repair budget. Send the
  evidence, attempts, exact decision needed, and recommended option. Continue
  safe independent work rather than polling or repeatedly asking to begin.
- These are instructions, not installed orchestration or merge permission.
  Preserve current writer ownership and all runtime permission gates.

## Required working rules

- Translate existing behavior first. Submit redesigns, optimizations, bug fixes,
  new command namespaces, and plugins as separate decisions and changes.
- Record substantive implementation oddities in the shared Tachikoma forum as
  discussion items and improvement ideas, with pinned source, a synthetic
  reproduction, impact, current parity treatment, and a separate proposal.
  Link existing discussions rather than duplicating them. A proposal is not
  approval to change behavior or the oracle; nonblocking ideas do not stop the
  port. See [discussion guidance](docs/migration-guidance.md#record-oddities-and-improvement-ideas).
- Record the Python/upstream source commit and complete behavior covered by each
  slice. Do not declare a port complete while omitting I/O, orchestration, failure
  paths, or consumers that belong to that approved slice.
- Preserve the independent oracle. Do not delete or weaken existing tests,
  regenerate expected outputs, change compatibility baselines, lower gates, or
  apply waiver labels to make a port pass without explicit owner approval.
- Work only in your assigned branch and file area. Do not copy unfinished peer
  worktrees, change another session's branch, or merge peer work unilaterally.
  Integrate only explicitly agreed commits through the integration owner.
- The shared integration branch is `feature/rust`. Use the
  exact agreed checkpoint, not an assumed latest version. Lane sessions develop
  on their own branches and target that trunk directly with small changes.
  Do not add permanent family-merger chains. Only the assigned integration owner
  advances the shared branch until separately authorized automation takes over.
  Pushes, PRs, releases, and live operations require their own authorization.
  Follow the [safe synchronization handoff](docs/integration-handoff.md); never
  reset, force-push, or switch a peer worktree to adopt a checkpoint.
- On a shared host, coordinate one heavy build/test job at a time. Do not infer
  an active lease from this file. Get an explicit grant from the integration
  owner; if messaging is unavailable, ask the owner rather than polling or
  inventing a lease. Read-only work can proceed while waiting.
- Read/format/narrow checks first; run the smallest tests covering the slice.
  Include affected downstream consumers and relevant optional features using
  existing Cargo-metadata-based validation plans, not per-agent package lists.
  Required full/native checks still gate integration; resource limits are not
  permission to omit validation.
- New test crates must deny `unnameable_test_items`. After adding or moving a
  helper or test, verify its enclosing scope and obtain an actual all-target
  check from the authorized validator. Formatting and metadata cannot establish
  method visibility or test registration. Preserve test bodies and source
  expectations when repairing accidental nesting; never suppress the warning.
- Keep dependencies and features centralized in `Cargo.toml`, preserve the lock,
  document public APIs, and respect workspace lints. Coordinate shared core,
  manifest, lockfile, toolchain, and architecture changes with integration.
- SDK consumers inject `azure_core::credentials::TokenCredential`. Libraries
  never prompt, exit, initialize a global logger/runtime, or use mutable CLI
  globals. Reuse the caller's async runtime. The owner-approved experimental
  Windows login API may initiate broker UI only when the CLI caller explicitly
  permits it; silent SDK refresh must never prompt.
- Implement typed REST endpoints by hand over shared transport and behavioral
  policies; do not duplicate HTTP, paging, or LRO machinery per endpoint or adopt
  the generator PoCs. Keep Swagger as a contract/reference input. See the
  [REST client decision](docs/decisions/rest-client-implementation.md).
- Tests stay offline and secret-free. Do not discover ambient credentials,
  implement static-token production fallback, persist plaintext credentials, or
  add a Python/az/azd runtime requirement. The `windows-broker` feature now includes
  experimental user-invoked login/status/logout, not a production support claim.
  Native login/MFA/tenant-policy proof and team review remain required.

Run the pinned checks from this directory, not the repository root. There is no
fixed crate count. Prefer focused crates that enable independent ownership,
implementation, and testing, with explicit interfaces and acyclic dependencies.
Crates under `crates/` are discovered automatically; declare each crate's
`package.metadata.fabric.layer` and add safe crates to `default-members` as
described in the architecture guide. The owner-approved optional
`fabric-identity-windows` native boundary stays outside default members.
Existing crates retain
`unsafe_code = "forbid"`. Only the native crate's private interop module may allow
documented unsafe blocks; its public API is safe Rust. Default consumers exclude
that crate; opting in does not make a binary transitively unsafe-free. Coordinate
shared interfaces and cross-layer changes with integration; do not add empty
future modules or runtime loaders as scaffold shortcuts. Default/broker-only `fab-rs` recognizes
bounded read arguments but still requires a read host. Explicit `native-read`
builds provide the bounded Windows host described in `docs/native-read-host.md`;
its preview
help/version/error contracts are not proof of parity with Python `fab`.

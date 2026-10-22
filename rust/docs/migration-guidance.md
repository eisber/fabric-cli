<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Translation-first migration guidance

The owner-approved strategy is **pure behavioral translation first, improvements
second**. The goal is a reusable native Rust SDK, thin human/agent CLI, optional
identity implementation, and native deployment library, with no production
requirement for Python, Azure CLI, or Azure Developer CLI. A compiling scaffold
is the foundation, not service, deployment, MSAL, or cross-platform parity.

These rules apply lessons from Stephen Toub's
[GitHub Copilot runtime migration retrospective](https://github.blog/ai-and-ml/generative-ai/migrating-the-github-copilot-runtime-to-rust-using-copilot/).
The retrospective reports a successful large migration with significant
performance benefits, alongside behavioral, lifecycle, omission, host, test-oracle,
and performance regressions. Its results are not performance promises for Fabric.
We adopt the process lessons, not its Node/FFI/runtime architecture or automation
prompts.

## Preserve behavior before changing it

For each bounded slice, record its source commit, entry points, consumers,
successful and failed outputs, side effects, ambient inputs, and cleanup.
Preserve the source algorithm and externally observable behavior before
redesigning around Rust. Do not combine a port with an unrelated bug fix,
optimization, namespace change, or plugin architecture.

Existing product requirements can intentionally differ from Python, especially
explicit identity selection, no credential fallback, and installation without
Python/az/azd. Record these as approved compatibility exceptions, not accidental
translation or unqualified parity. Do not select an interactive backend simply
to keep a port moving. Unsupported behavior must fail explicitly.

The SDK belongs below the CLI. Do not reproduce an SDK that launches the CLI or
a permanent Python deployment bridge. No Copilot agent runtime, generic plugin
loader, or multi-language FFI layer is implied by this migration.

## Protect an independent behavioral oracle

Validation owns reference fixtures and compatibility expectations separately
from implementation. Existing tests and snapshots must not be silently rewritten,
deleted, skipped, or weakened; neither compatibility baselines nor waiver labels
may be changed to hide a regression. Any necessary oracle change needs an
explicit rationale and owner approval independent of the implementation.

Use the Python implementation as a development-time comparison oracle when
appropriate. It is not a dependency of the shipped Rust product. Capture
synthetic or sanitized inputs/outputs, exact versions, fixture provenance, and
intentional exceptions. Failing comparisons should fix the translation first.
Review old and new behavior after rebases, not just a mechanically clean merge.

Keep regression tests at the public behavior boundary. Unit tests and static
checks complement, but do not replace, command/SDK end-to-end contracts.
Measure current test coverage as an inventory of exercised code, not proof of
correctness; passing high coverage can still validate the wrong oracle.

## Make hidden semantics explicit

| Risk seen in migration | Required Fabric evidence |
| --- | --- |
| Numeric and optional-value changes | Integer vs fractional JSON, missing vs null vs empty, defaults, Unicode and ordering contracts |
| Ambient behavior | Capture/refresh rules for working directory, paths, environment, account, tenant, audience, time zone, proxy, and trust |
| Half-ported paired operations | Request/cancel, create/cleanup, refresh/cache, accept/poll, persist/project, callback/result ordering |
| Lifecycle and races | Cancellation at each boundary, bounded work, timeouts, disposal, retry/reconciliation; accepted is not completed and cancellation is not rollback |
| Library substitution | YAML duplicate case/typing/merge/emission, Python regex replacements, JSONPath mutation and distinct JMESPath projection |
| Host boundaries | Native Windows/macOS/Linux path and process behavior, no blocking async worker with heavy synchronous work, platform credential/persistence support |
| Missing feature or drift | Inventory all slice callers/failure paths; record source commit and reconcile incoming Python/upstream changes |
| Performance regression | Preserve caching, streaming and bounds; check copying/allocation, locks, task fan-out, polling, startup and memory on comparable workloads |

For network slices, inject mock transport and credentials, validate continuation
origins and bounds, classify service failures, and test operation handles rather
than returning successful-looking stubs. No live service call is authorized by
a test plan.

## Stage in a folder, integrate in small slices

`rust/` is a staging and ownership boundary, not an instruction to hold the
whole rewrite until a big-bang cutover. Start with pure offline leaf behavior and
independent fixtures, then a read-only workspace-list vertical slice, and later
stateful/mutating operations once prerequisites and permissions are established.
Integrate reviewable slices regularly, preserving the working Python product.

Each slice needs a complete definition of done: source behavior and consumers
mapped, native implementation and failure/cleanup paths present, targeted tests
and independent contracts passed, required native evidence recorded, and no
unapproved fallback or omitted behavior. Planned CI is not passed CI. Report
actual runner architecture and unexecuted platforms.

Controlled previews and rollback arrangements are separate, explicitly approved
rollout steps. Do not shadow mutating deployment operations against a live
workspace to compare implementations. Eventual folder promotion and Python
retirement follow demonstrated compatibility and rollout readiness, not file
counts or lines translated.

## Coordinate parallel work without annexing peers

The integration session owns shared layout, core interfaces, manifest/lockfile
and toolchain policy, integration, and cutover. Lane branches use an exact
committed checkpoint of `feature/rust-workspace-scaffold`; the branch is the
shared code handoff, not a substitute for delivered instructions or approval.
Do not independently push competing lane changes to that shared branch.

| Lane | Initial bounded remit | Outside its initial authority |
| --- | --- | --- |
| SDK and CLI porting | Offline workspace-list contract/transport design and translation after agreed interfaces | Broad commands, auth backend, unreviewed shared core changes |
| Native deployment | Pinned-source config/transform reference fixtures, then native translation | Live publish/unpublish, permanent Python bridge |
| Compatibility and validation | Independent Python behavior oracle, golden harness and parity matrix | Silently redefining expected behavior to match Rust |
| Build and distribution | Native CI from `rust/`, build resource coordination and packaging evidence | Release/signing/install-format decisions without approval |
| Identity | Requirements/evidence, backend-neutral contracts, explicit unsupported capabilities | Selecting production interactive backend or relaxing enterprise requirements |
| Plugins and skills | Clarify concrete use cases and document extension/workflow boundaries | Inventing a loader/protocol or new namespaces without approval |
| Unit-test coverage | Measure the existing Python unit suites and report line/branch gaps | Editing tests or production code to increase the number |

No session may commandeer a peer branch, copy unfinished peer worktree changes,
or interpret silence as agreement to integrate. Only agreed commits move through
the integration owner, with revalidation after integration. A blocked lane can
continue independent read-only analysis or authorized fixture work; it cannot
reconstruct a missing shared scaffold.

## Keep the shared-host inner loop usable

The initial shared-host budget is one heavy build/test job at a time, including
coverage runs. The integration owner grants a bounded lease with named work and
release/expiry; the build lane can propose scheduler automation later. Sessions
must not infer a lease, poll continuously, launch recursive agents, or skip
required validation because the host is busy. Use read-only work while waiting.

Centralize dependency features, keep crates focused, use targeted checks, and
measure cold/warm build costs before introducing caching or scheduling machinery.
Do not share mutable build-output directories between concurrent worktrees without
a deliberate locking strategy.

## Turn failures into durable prevention

Add a regression case when a porting defect is found. Repeated failure patterns
belong in this guidance, contributor instructions, a protected contract, or
tooling rather than repeated conversational reminders. Keep evidence references
and decisions durable so context compaction does not erase them.

Before integrating a slice, review semantics and completeness, independent
oracle results, dependency/native prerequisites, actual validation and platform
coverage, resource/lifecycle behavior, and upstream drift. Human approval remains
the gate for contract exceptions, high-risk architecture, and release decisions.

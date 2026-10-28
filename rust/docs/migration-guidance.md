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

For current authorized work, follow [agent-operated delivery](agentic-delivery.md):
independent perspectives at each stage, revision-bound evidence, bounded repair,
and humans only for unresolved blockers outside established authority.
The historical lane remits and checkpoints below are provenance, not a requirement
to repeat kickoff approvals or create layers of merger branches.

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

The assigned integration role coordinates shared layout, core interfaces,
manifest/lockfile and toolchain policy, integration, and cutover. It can be
performed by an authorized agent; it is not a routine human approval step.
Crate-family owners and independent reviewers divide implementation and review,
not the final merge path. Lane branches use an exact
committed checkpoint of `feature/rust`; the branch is the
shared code handoff, not a substitute for delivered instructions or approval.
Do not independently push competing lane changes to that shared branch.
Use small changes targeting that trunk directly; use atomic cross-crate changes
or compatible interface migrations rather than permanent family-merger chains.

The initial assignments below are historical. The current task and protected
contracts determine authority; these rows do not expand or revoke an assignment.

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

### Current integration and safe handoffs

The owner standardized the integration branch as `feature/rust` in
`eisber/fabric-cli` on October 23, 2026. Only the integration owner advances that
branch until separately authorized automation takes that role. This instruction
update installs no controller or merge queue. Lane owners continue on separate
branches and merge the exact published,
validated checkpoint into their own clean worktrees. Preserve divergent commits
and unrelated edits; do not reset, force-push, commandeer peer worktrees, or
silently chase a moving branch tip. See the
[integration handoff](integration-handoff.md) for the captured heads and sync
commands. Upstream remains read-only for this work.

### Historical first foundation and initial handoffs

The first shared foundation was published at
[`407c595095ca326ee4a7d8355232346f219b6912`](https://github.com/eisber/fabric-cli/commit/407c595095ca326ee4a7d8355232346f219b6912)
on `feature/rust-workspace-scaffold` in the owner's `eisber/fabric-cli` fork.
That branch name records the historical foundation, not the current integration
destination. The table below records the initial lane handoffs, not the current
implementation status or a renewed work authorization.
Upstream `microsoft/fabric-cli` is not the push destination for lane work.
The foundation leaves Python source/tests unchanged and stages all Rust-only
files under `rust/`, with GitHub Actions at the repository root.

Fetch this explicit existing fork and verify the agreed commit before integrating
it into a clean lane branch. Preserve unrelated changes and active user requests.
If a branch has diverged, coordinate an agreed integration rather than resetting,
force-pushing, or taking a peer worktree. Run Cargo from `rust/`.

| Lane | Next handoff after adopting the agreed checkpoint |
| --- | --- |
| SDK and CLI | Define the offline workspace-list request/response, transport-injection seam, pagination/error behavior, and Python source mapping with validation; begin translation only within the agreed slice |
| Native deployment | Inspect the pinned `fabric-cicd` source from the compatibility baselines; define synthetic config/transform reference cases before native implementation; report unavailable source explicitly |
| Compatibility and validation | Prepare the proposed eight-case Python workspace-list golden harness and preserve independent expected outputs; attach the Rust adapter only after its injection interface is agreed |
| Build and distribution | Inspect the now-present native workflow running from `rust/`; address real CI gaps, report actual job outcomes/architectures, and coordinate heavy local work; defer installer/release choices |
| Identity | Document platform/policy acceptance requirements and backend evidence without choosing or implementing a production backend; ask the owner which enterprise policy/platform must be proven first |
| Plugins and skills | Obtain the owner's first concrete use case before selecting executable extensions, a protocol, or a loader; workflow skills and built-in modularity do not imply an external plugin runtime |
| Unit-test coverage | Measure existing Python core/utils/parser unit suites, with line and branch coverage over all product source, exact test counts, exclusions and artifacts; do not modify tests or product code to inflate coverage |

This branch makes code and written handoffs accessible. It does not itself wake
another session, grant cross-session management capability, answer an outstanding
interactive question, or prove that a recipient has read the handoff. Record
actual delivery/continuation separately. If direct contact is refused, report
the missing capability to the owner and use the existing authorized continuation
path; do not change identities or bypass the session's approval gates.

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

## Record oddities and improvement ideas

The owner requested on October 24, 2026 that implementation discoveries become
forum discussion items and improvement ideas, not just notes buried in a final
handoff. Use the shared Rust migration forum `tk:forum/12/4`. Start a distinct
additive post for each substantive topic; search existing discussion and link
follow-ups rather than reposting the same finding.

Each entry should include:

- A stable topic and classification: observed behavior, suspected issue, or
  improvement idea. Separate evidence from hypotheses and proposals.
- The exact source repository/commit, file and symbol, plus a minimal synthetic
  reproduction and observed versus expected behavior.
- Consumer impact and the current port treatment: preserved quirk, approved
  exception, blocked behavior, or unported branch.
- A proposed improvement, tradeoffs, any decision needed, and evidence/test/wiki
  references.

Keep examples secret-free and within the authorized knowledge scope. Do not
publish private source or inputs externally. Include discussion references in
the lane handoff and curate durable conclusions into the wiki after discussion.
If forum tools are unavailable, retain the structured entry in session artifacts
and send it to the coordinator for relay; do not bypass tool permissions.

Preserve Python behavior during translation unless a scoped change was already
approved. Recording an idea does not authorize fixing the source quirk, changing
an external contract, weakening an oracle, or expanding dependencies or scope.
Nonblocking ideas must not pause implementation. Genuine correctness, safety,
or contract blockers still need explicit assigned question/blocked reporting;
forum persistence alone is neither escalation delivery nor owner approval.

## Turn failures into durable prevention

Add a regression case when a porting defect is found. Repeated failure patterns
belong in this guidance, contributor instructions, a protected contract, or
tooling rather than repeated conversational reminders. Keep evidence references
and decisions durable so context compaction does not erase them.

Before integrating a slice, apply the independent stage reviews for semantics
and completeness, oracle results, dependency/native prerequisites, actual
validation and platform coverage, resource/lifecycle behavior, and upstream drift.
Repair findings within the task's budget instead of requesting routine human
approval. Contract exceptions, high-risk architecture, and releases still require
their established decision authority and restricted-area team review. Escalate
only when that authority or required evidence is absent or repair is exhausted;
these instructions do not grant it.

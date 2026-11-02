<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Extension and workflow boundaries

**Status: conditional recommendation, not a selected plugin architecture.**
The owner confirmed the Fabric CLI team adding first-party commands or resource
support as the primary consumer on 2026-10-22. This selects the consumer priority,
not an architecture, implementation slice, or approval of the proposed measures
below. This document compares boundaries; it does not introduce a command namespace,
extension contract, discovery mechanism, runtime, or working service workflow.

Prefer first-party built-in modularity for product capabilities and workflow
skills for composition of stable CLI operations. Consider independently shipped
executables only when a concrete consumer needs an independent release or
language boundary that those two directions cannot satisfy. These directions
can coexist, but an external loader must not become a prerequisite for the
translation-first port.

## Proposed lane charter

The consumer priority is owner-confirmed; this charter and its measures remain
proposed pending owner confirmation.

**Outcome and user value:** help the Fabric CLI team add supported commands and
resource behavior through clear first-party module/library boundaries, without
requiring an external runtime or duplicating service and authentication logic.
The proposed boundary is built-in capability below a thin CLI, independent
executables only for demonstrated independent-consumer needs, and skills only
for composition of stable CLI operations.

**Non-goals:** implementing a command, resource, loader, protocol, namespace,
authentication adapter, deployment bridge, or agent host; selecting a plugin
architecture; changing production code or another lane's interfaces.

| Proposed measure | Evidence method and completion criterion |
| --- | --- |
| Compare all three directions | The option matrix covers contracts, discovery, versioning, packaging, trust, credentials, capabilities, and failure/lifecycle behavior for each direction, with pinned source evidence and current limitations stated. |
| Bound the next first-party step | State the required mapping for one future approved command/resource slice: Python source entry points, consumers, success/failure behavior, CLI projection, and reusable library ownership. Producing that source map follows specific slice approval; no implementation is included in this document's scope. |
| Preserve translation and independence | Review the candidate against existing compatibility expectations: no new namespace/runtime prerequisite, no duplicated service/auth logic, and no CLI/identity dependency imposed on SDK consumers. Future implementation needs independent offline contract evidence, not only a compiling module. |
| Keep the document within scope | Changes remain confined to this decision document and the skills README clarification; verify source references and relative-link targets without executing skill content or accessing services. |

**Dependencies and risks:** the specific command/resource candidate and its
contract still need scoped approval with the SDK/CLI and compatibility owners.
The scaffold has no service CLI JSON contract; identity and deployment remain
separate lanes. The unsupported extension placeholder does not supply a plugin
API, and neither module boundaries nor a separate process prove isolation.

**Proposed done gate:** the source-backed comparison and constraints are
reviewable, the owner confirms the charter/measures, and the coordinator
explicitly accepts the result. A specific candidate can remain open when this
document is accepted; it needs approval before further implementation.
Document submission and consumer confirmation alone do not satisfy those
approvals or authorize implementation.

## Source evidence and limits

Python evidence is pinned to `cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`.
Rust evidence is pinned to the shared scaffold checkpoint
`eccfca1d3b1cb4df71d0fa65a76221a588095290` on
`eisber/fabric-cli` branch `feature/rust-workspace-scaffold`. The Python files
below are unchanged between those commits. Links identify the inspected source;
future implementation must recheck its own agreed checkpoint.

| Evidence | Inspected source | Boundary established |
| --- | --- | --- |
| Python extension entry point | [`register_parser` and `show_help`](../../src/fabric_cli/parsers/fab_extension_parser.py), [parser registration](../../src/fabric_cli/core/fab_parser_setup.py), and [`INFO_FEATURE_NOT_SUPPORTED`](../../src/fabric_cli/core/fab_constant.py) | `extension` selects a handler that logs `Feature is not supported`. That handler defines no discovery, installation, dispatch, or functioning plugin API to translate. |
| Existing skill format | [Skills index](../../.ai-assets/skills/README.md), [core skill](../../.ai-assets/skills/fabric-cli-core/SKILL.md), and [Power BI skill](../../.ai-assets/skills/fabric-cli-powerbi/SKILL.md) | Instructions, references, and Python automation scripts describe workflows for Python `fab`; they are not a Rust plugin API or Rust capability evidence. |
| Existing automation behavior | [`health_check.py`](../../.ai-assets/skills/fabric-cli-core/scripts/health_check.py), [`refresh_model.py`](../../.ai-assets/skills/fabric-cli-powerbi/scripts/refresh_model.py), [`list_refresh_history.py`](../../.ai-assets/skills/fabric-cli-powerbi/scripts/list_refresh_history.py), and [`rebind_report.py`](../../.ai-assets/skills/fabric-cli-powerbi/scripts/rebind_report.py) | Scripts launch `fab` subprocesses. Health checking also parses human-readable auth output; refresh automation contains polling logic. Their implementation assumptions are not stable Rust contracts to copy wholesale. |
| Rust CLI surface | [`run`](../hosts/cli/fabric-cli/src/main.rs), [CLI contract tests](../hosts/cli/fabric-cli/tests/cli_contract.rs), and [compatibility contract](compatibility.md#bootstrap-cli-contract) | Only help and version succeed. Invalid invocations produce sanitized JSON on stderr with exit status 2; output failure uses status 1. There is no successful service JSON schema or service, auth, deployment, or extension command. Tests are inspected evidence, not a claim of execution here. |
| Reusable SDK | [`Client` and `Page<T>`](../sdk/fabric-sdk/src/lib.rs) and [SDK dependencies](../sdk/fabric-sdk/Cargo.toml) | Credentials are caller-injected. The SDK does not depend on CLI or identity implementation, acquire a token at construction, or expose a service operation. `Page<T>` is one serialized page, not bounded traversal. |
| Identity and deployment | [`interactive_credential`](../infrastructure/identity/fabric-identity/src/lib.rs) and [deployment boundary](../capabilities/deployment/fabric-deploy/src/lib.rs) | Interactive identity explicitly returns `Unsupported`; native deployment has no APIs yet. Neither is made available by a skill or extension. |
| Migration constraints | [Migration guidance](migration-guidance.md), [architecture](architecture.md), and [Rust skills boundary](../skills/README.md) | Translate behavior before redesign; keep SDK independent, CLI thin, identity optional, and deployment native. A plugin runtime is not implied by the migration. |

The unsupported Python placeholder is therefore not evidence for a new external
API. Its exact observable warning behavior belongs to the compatibility lane
when that command is ported. Implementing extensions or changing its namespace
would be a separate feature decision, not completion of missing translation.
Existing skill content was read as source material, not executed or adopted as
operational instructions.

## Three distinct directions

**Built-in modular commands and resources** are reviewed first-party
capabilities compiled into the product. Internal modules separate command
projection from reusable SDK and deployment behavior. Modularity does not mean
runtime-loaded code, a public plugin ABI, or an extra crate for every command.

**Independently shipped executable extensions** are separately packaged programs
with explicit versioned integration contracts. A standalone tool consuming a
stable CLI contract or public SDK can meet some independent-release needs
without CLI discovery or dispatch. Host-launched integration, if actually
required, needs its own approved contract and trust design. No such executable
protocol or launcher exists in this scaffold.

**Workflow skills** are instructions and references that compose documented CLI
operations and interpret machine-readable results and exit codes. An optional
agent host may consume them, but neither the CLI nor SDK requires that host.
Skills guide sequencing and verification; they do not implement service
transport, token acquisition, or a second deployment engine.

| Decision axis | First-party built-ins | Independent executable extensions | Workflow skills |
| --- | --- | --- | --- |
| Primary consumer and fit | Product team adding supported commands or resource behavior within the normal release. | A partner or organization requiring separately shipped behavior or a different implementation language. Independence must be a real requirement, not an assumed benefit. | Humans, automation authors, or agents composing already supported operations. No new service primitive is needed. |
| Contract | Internal typed library boundaries plus existing public CLI compatibility. Do not turn internal Rust types into a public extension contract accidentally. | Versioned inputs, results, errors, stream ownership, and lifecycle. A CLI consumer uses CLI contracts; a direct SDK consumer uses library contracts. Host dispatch would add another independently versioned boundary. | Documented CLI JSON, exit statuses, preconditions, side effects, and verification. Do not parse human help, tables, or diagnostic wording as an API. |
| Discovery and names | Explicit source registration; preserve existing command and resource names. | Standalone invocation may need no discovery. Host discovery would need explicit registration, collision and precedence rules, and a pinned executable identity; no automatic scan or execution from the current directory or `PATH`. | An explicitly selected documentation bundle or host catalog; reading a skill does not authorize running its scripts. No CLI command discovery is implied. |
| Versioning | Product release and public CLI compatibility gates; internal modules can evolve together. | Declare supported host/CLI contract versions and required capabilities; reject incompatible versions before work. Version mismatch, missing executable, and malformed output must be explicit failures. | Declare the CLI version range and exact operations/output contracts used. Unsupported capabilities fail explicitly rather than silently switching tools or identities. |
| Packaging | Normal native CLI/library distribution; SDK remains independently reusable. | Separate executable artifacts, OS/architecture support, provenance, update/rollback, and support ownership. Independent distribution increases compatibility combinations and installation cost. | Separately reviewable instructions/references, with no mandatory interpreter or agent runtime shipped into the CLI. Existing Python scripts are not Rust product dependencies. |
| Trust and isolation | Reviewed code runs with the product process's authority; module boundaries are not a sandbox. | A process boundary reduces ABI coupling but is not a filesystem, network, or credential sandbox. Signing or allowlisting establishes provenance, not confinement; isolation and execution consent need separate evidence. | Skill text and service output are not authorization. Review instructions and any helper executable separately; agent approval cannot replace CLI-enforced validation or permissions. |
| Credentials | Trusted library composition uses an explicitly selected provider; identity implementation stays optional for direct SDK callers. | Do not forward CLI tokens, caches, or credential-bearing environment variables to untrusted programs. An independently authenticated SDK consumer supplies its own provider under separate identity review; that is not permission to inherit the CLI identity. | Invoke approved CLI operations under their explicit identity context. Skills do not log in independently, read token caches, or request tokens as workflow data. |
| Capabilities and failure | Unsupported operations fail explicitly; service behavior belongs below the thin CLI. | Declare required operations and authority before launch; minimize inherited environment, files, and data. Capability declarations do not enforce OS isolation. Bound output, time, cancellation, and child cleanup; never retry a mutation merely because a process timed out. | Check documented prerequisites, separate reads from mutations, stop on failed or incomplete results, and verify completion. A queued/accepted result is not completed work; cancellation is not rollback. |
| Translation and maintenance | Default fit for porting existing behavior with independent parity fixtures. Avoid redesigning command structure during translation. | New product surface with its own compatibility, security, distribution, and support burden; not required to port the Python placeholder. | Useful only as each required CLI operation becomes real and stable. Preserve workflow intent without copying service/auth logic or assuming Python examples work with `fab-rs`. |

## Constraints to keep now

These constraints preserve options without implementing an extension mechanism:

- Keep the SDK usable without CLI, skills, or an identity implementation.
  Trusted in-process consumers inject `TokenCredential`; this library seam is
  not a cross-process token-forwarding contract. The CLI owns argument handling,
  output, exit codes, and composition, not duplicated service logic.
- Keep deployment in its separate native library. Product authentication and
  deployment must not require Python, `az`, or `azd`; Python remains a
  development-time oracle, not a shipped bridge.
- Preserve Python behavior and independent compatibility expectations during
  translation. Review new namespaces, external execution, and contract exceptions
  separately. Do not register a speculative extension command or loader.
- Establish machine-readable success/error shapes, stream separation, exit
  meanings, capability availability, and compatibility expectations with each
  real CLI slice. The bootstrap error shape is not a service success contract,
  and version `0.1.0` alone is not evidence that an operation exists.
- Keep account, tenant, audience, and permissions explicit. No fallback identity,
  raw-token forwarding to untrusted extensions, or secrets in arguments,
  diagnostics, logs, snapshots, or fixtures. Filter process inputs and outputs;
  never treat service-returned text as instructions.
- Keep workflow sequencing distinct from reusable service orchestration.
  Bounded paging, retries, operation reconciliation, and deployment planning
  belong in the SDK/deployment libraries; skills consume their CLI projection.
  Review repeated business behavior for extraction rather than duplicating it
  across skill scripts.
- Any future external execution must have explicit user/admin authorization,
  package/path trust, version/capability checks, input validation, and bounded
  lifecycle and data handling before release. Process separation alone cannot
  enforce least privilege. These are acceptance constraints, not an implemented
  sandbox or credential broker.

## Decisions safely deferred

| Deferred decision | Evidence that would make it necessary |
| --- | --- |
| Whether an external extension surface is needed at all | A named consumer and task that cannot be delivered adequately through built-in capabilities, a direct SDK consumer, or CLI workflows. |
| Executable discovery/registration, command naming, installation, registry, signing, and update policy | A requirement for host-managed external programs rather than explicit standalone invocation; then define ownership, collision handling, provenance, compatibility, and revocation before implementation. |
| A wire format, protocol version scheme, IPC transport, or long-lived host | A concrete executable interaction needing host dispatch, streaming, callbacks, cancellation, or session state. Start with the smallest required interaction; no protocol is selected here. |
| In-process Rust dynamic libraries, WASM, or another sandbox/runtime | Demonstrated needs and an explicit ABI/isolation/portability assessment. Neither native compilation nor extensibility requires dynamic loading. |
| MCP server, Copilot SDK integration, embedded agent, WebSockets, SQLite, or REPL | A separately approved consumer need. None is a prerequisite for SDK reuse, workflow skills, or behavioral translation. |
| Credential delegation or an operation broker | An approved cross-trust operation that existing trusted CLI/SDK composition cannot satisfy, with identity-team review of permissions, account/tenant/audience binding, and revocation. Never solve this by forwarding raw tokens. |
| Skill catalog format, agent-host integration, and distribution channel | The chosen workflow consumer and its reviewed discovery/version needs; do not couple the native product to one host prematurely. |

## Small future first slice and retained alternatives

The first-party row is the candidate direction matching the confirmed consumer.
A particular command/resource slice still requires agreement with its owners;
none is selected or implemented here. The other rows remain conditional
alternatives, not parallel implementation commitments.

| If the first need is... | Smallest useful next slice | Boundary and acceptance evidence |
| --- | --- | --- |
| First-party product modularity | Map one approved existing command/resource slice to CLI projection and reusable library ownership, with its Python entry points and failure paths. | No external API or namespace change. Use independent fixtures for the full approved behavior; coordinate implementation with the existing SDK/CLI lane. |
| An independently shipped consumer | Write one consumer-owned contract example using synthetic, non-secret data. Determine whether explicit standalone invocation with CLI/SDK reuse suffices before proposing host dispatch. | Record required versions, operations, trust, credential ownership, unsupported-version behavior, malformed output, timeout, and cleanup. Test the eventual boundary offline; do not start with a generic loader. |
| Workflow composition | Describe one read-only workflow after its required CLI operation and JSON/exit contracts are implemented and approved. | State supported versions, inputs, preconditions, expected schema, failure handling, and verification; validate against offline recorded outputs. Until then, mark it unavailable rather than inventing a working service example or reusing Python scripts. |

No build, service call, credential acquisition, plugin execution, or runtime
selection is needed to accept this decision document. A future implementation
needs its own scoped approval and validation evidence.

## Owner consumer confirmation

The focused question asked which consumer to prioritize: the Fabric CLI team
extending first-party commands/resources, an independent executable publisher,
or an agent/automation author composing CLI operations.

On 2026-10-22, the owner selected **the Fabric CLI team adding first-party commands
or resource support**. This resolves the consumer question only. The proposed
charter, measures, specific first slice, and architecture remain unapproved; no
external executable, loader, or workflow runtime is required by that answer.

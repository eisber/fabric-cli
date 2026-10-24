<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Offline workspace listing: source mapping

This slice translates `microsoft/fabric-cli` at
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. It does not implement a CLI
command, real HTTP transport, or identity backend. SDK callers supply an Azure
`TokenCredential` independently of `fabric-identity` and the CLI.

`Audience`, `CredentialSelection`, and `TokenCredential` are reexported by this
facade. Consumers can construct an explicit selection without depending on the
core implementation crate or changing the architecture's dependency directions.

## Generic item LIST/GET

The additive `items` module and `Client::{list_items,get_item}` delegate to
`fabric-sdk-items` with the same injected credential and selection. Existing
workspace imports remain unchanged. See the [item SDK](../fabric-sdk-items/README.md)
for typed models, ordered query/token handling, partial failures, cancellation,
safety exceptions and the exact pinned Python source. This is generic metadata
LIST/GET, not specialized item definitions, mutations or CLI parity.

## Capacity listing

The additive `fabric_sdk::capacities` module reexports the focused
[`fabric-sdk-capacities`](../fabric-sdk-capacities/README.md) API and
`Client::list_capacities` delegates with the existing credential `Arc`, selection,
executor, bounds, and cancellation. `models::Capacity` is a reexport, and page/
service-error types remain shared. This implements only Fabric `/v1/capacities`,
not ARM management or CLI capacity collection/output behavior.

## Focused crate and facade compatibility

The workspace implementation is extracted from SDK checkpoint
`c07228b5ed5266b899ff3d2b37c7a3778cb855b8` into `fabric-sdk-workspaces`.
Its `WorkspaceClient`, workspace models, page type, executor, typed failures,
pagination/cancellation implementation and operation tests are owned there.
It can be built and consumed independently of this facade.

Existing consumers keep `fabric_sdk::Client::new`, credential/selection accessors,
`Client::list_workspaces`, `fabric_sdk::Page`, `fabric_sdk::models`, and
`fabric_sdk::workspaces`. Models/errors are reexports of the same types, not
serialization adapters. Delegation preserves the caller's credential provider,
selection, executor, bounds, cancellation, returned data and partial errors.
The facade client remains owned here so other service methods do not require
changing the workspace crate.

No new service behavior, production policy, identity implementation, CLI command,
or deployment implementation is introduced by extraction. Public API source
compatibility is exercised through external consumer tests; concrete Rust type
ownership for moved models/pages is now the focused crate, so diagnostic
`type_name` output is not a promised compatibility surface.

From `rust/`, under a named build slot:

```text
cargo test --offline --locked -p fabric-sdk-workspaces -p fabric-sdk --all-targets -- --test-threads=1
cargo clippy --offline --locked -p fabric-sdk-workspaces -p fabric-sdk --all-targets -- -D warnings
cargo test --offline --locked -p fabric-sdk-workspaces -p fabric-sdk --doc
cargo test --offline --locked -p fabric-cli --test workspace_contract -- --test-threads=1
```

## Python behavior and compatibility boundary

| Source | Bounded behavior |
| --- | --- |
| `src/fabric_cli/client/fab_api_workspace.py:list_workspaces` | Sets `args.uri = "workspaces"` and `args.method = "get"`, then calls `do_request`. |
| `src/fabric_cli/client/fab_api_client.py:do_request` | Defaults to `https://api.fabric.microsoft.com/v1/workspaces`, Fabric `.default` scope, JSON content type, bearer authorization, and a 240-second request timeout. Supports ambient endpoint overrides, authentication/context globals, arbitrary query/header options, and private-link policy in the generic client. |
| `_handle_successful_response` | A present `x-ms-continuation` header takes priority over body `continuationToken`, including when the header is empty. Repeats the original request with a truthy token encoded as a query parameter. Ignores `continuationUri`; false/empty/null token ends traversal. Recursion has no explicit bound or cycle detection. |
| `src/fabric_cli/client/fab_api_types.py:ApiResponse.append_text` | Concatenates matching list-valued fields in page order, retaining duplicates. Removes both continuation properties after appending. Called with the default `total_pages = 0`, so writes `total_pages = 1` at every recursive merge, even for more than two pages. Does not implement a reliable page count; the independent three-page oracle also observes 1. |
| `do_request` failures | 401/403 use `x-ms-public-api-error-code`, then `x-ms-error-code`, then `Unauthorized`/`Forbidden`; 404 always uses `NotFound`. Other Fabric HTTP errors use body `errorCode`, `message`, details and request ID, with `UnknownError` fallback. Request failures become `UnexpectedError`; append JSON decoding failures become `InvalidJson`. |
| Generic retry/polling | Requests' adapter retries 502/503/504 up to three times; the outer loop handles four 429 attempts, waiting on integer `Retry-After` or the default before `MaxRetriesExceeded`. Generic 202 handling enters an LRO poller; 201/204 are accepted by the generic client. These are not successful workspace-list payloads by themselves. |
| `src/fabric_cli/utils/fab_mem_store.py:_get_workspaces_from_api` | Requires `id`, `displayName`, `type`; accepts 200/201. Recognizes Workspace/Personal case-insensitively and skips unknown workspace types in this hierarchy adapter, not in the service client. Optional tenant-keyed cache has 60-second TTL; name resolution can clear/retry it. |
| `src/fabric_cli/core/hiearchy/fab_workspace.py` | Projects names to `<displayName>.Workspace` or `.Personal`; escapes `/` as `\/` in tenant paths. Service IDs are not newly constrained to UUIDs. |
| `src/fabric_cli/commands/fs/ls/fab_fs_ls_workspace.py` and `utils/fab_cmd_ls_utils.py` | `ls` sorts projected names case-insensitively; normal output selects `name`. Long output adds ID/capacity fields by separately listing capacities and workspaces. Missing capacity details become `N/A`/`Unknown`. JMESPath projection, hidden virtual workspaces, streams, and formatting belong above the SDK. |

## Proposed CLI seam

The SDK returns ordered, unfiltered typed service workspaces and explicit traversal
progress. Preserve service names, IDs, types, duplicates, extra fields and their
missing/null distinction. A future CLI adapter owns hierarchy type filtering,
name/path projection, caching, sorting, capacity enrichment, JMESPath, output
formatting and exit codes. Do not invent a `workspace list` namespace.

The existing scaffold `Page<T>` JSON example is only a serialization example,
not the Python CLI oracle. Independent workspace compatibility fixtures belong
to the validation lane and must not be rewritten to accommodate implementation.

## Approved product constraints, not source parity

- Explicit account/tenant selection and Fabric audience replace ambient auth.
  Provider binding to that selection remains the caller's responsibility.
- The operation is public-cloud only, with no endpoint/header overrides or
  credential fallback; it never follows unvalidated service URLs.
- Bounded pagination, cycle detection, cancellation and explicit incomplete
  failures replace unbounded recursive traversal and accidental empty success.
- Accurate traversal progress is SDK metadata, not Python's `total_pages` field.
- Libraries do not prompt, exit, log globally, or create a runtime.

The integration owner approved the operation-specific executor as a temporary
offline mock seam and provided runtime dependencies at checkpoint
`4d740138ae738ffdca64415638ca491593da7c86`. Shared retry/timeout/transport policy
still needs an agreed core interface before production transport or CLI exposure.
This slice does not present an unimplemented policy or an LRO/empty response as
a complete workspace listing.

## Confirmed slice contract and outstanding decisions

The owner confirmed the bounded offline contract, subject to the integration
owner's interface approval. Defaults are 100 pages, 100,000 accumulated items
including duplicates, and 4 MiB (4,194,304 bytes) per response. Exact-bound
terminal results succeed; exceeding a bound returns explicit partial progress.
The executor must enforce the byte limit while reading, not merely after
allocating the whole response.

The owner also confirmed token-driven terminal-page behavior: when there is no
nonempty continuation token, ignore an unused malformed or foreign
`continuationUri`. No additional token acquisition or request occurs. When a
continuation token exists, validate any supplied URI before another credential
request or HTTP execution; construct the next request from the fixed endpoint
and opaque token rather than following the supplied URI.

The temporary operation-specific executor owns retry and timeout policy; 202
and 204 are explicitly unsupported, not completed workspace-list pages. This
is not generic Python request/LRO parity. The `azure_core`/`serde_json`
runtime-dependency promotions have been integrated from the exact checkpoint
above. Production shared-policy design remains an integration-owner decision.
No broader core policy or new dependency version/feature is introduced here.

## Review cycle

Use bounded, separate-context reviewers without transferring source ownership.
The implementation owner makes SDK changes; the compatibility lane owns oracle
expectations; the integration owner decides shared interfaces and integration.
Reviewer suggestions are evidence, not approval to change scope or baselines.

| Gate | Responsible role | Required evidence before advancing |
| --- | --- | --- |
| Contract and design | Owner and integration owner, informed by architecture review | Pinned Python/base commits, consumer-visible request/response/error behavior, injected executor and credential lifetimes, cancellation/drop semantics, limits, and explicit compatibility exceptions. No implementation-invented shared policies. |
| Implementation review | SDK reviewer, separate from the author | Concrete findings on typed errors/partial progress, malformed responses, continuation validation, ownership across `await`, `Send` requirements, public construction patterns, and test-module wiring. Track each finding as fixed, pending, or explicitly rejected with rationale. |
| Independent compatibility | Validation lane | Unchanged independent fixtures and request-sequence/error comparisons against the pinned Python source. Helpers-only or serialization tests do not prove operation parity. |
| Bounded validation | SDK owner under a named host slot | From `rust/`: targeted SDK library tests, all-target Clippy with warnings denied, and doc tests. Record compiler/toolchain, collected test names/count, outcomes and unrun checks; formatting alone is not validation. |
| Fix and re-review | SDK owner and existing reviewer | Regression cases for confirmed defects; rerun affected checks. Substantive fixes or source/interface drift require another whole-slice semantic comparison, not only inspection of the latest patch. |
| Integration | Integration owner | Exact local commit, source mapping, reviewer dispositions, independent oracle results, actual validation output, unresolved decisions, and native-platform gaps. Never weaken a gate or alter an oracle to obtain a pass. |

The initial independent design/Rust review identified a misplaced file-backed
test-module declaration, missing exact item/byte boundary cases, missing `Send`
evidence, and the terminal-page URI question. The declaration is now at module
scope; boundary and compile-time `Send` assertions are added; terminal-page
behavior follows the owner's decision above. These began as source-level fixes,
not passing-test evidence. The dependency checkpoint and temporary executor
approval have since been integrated, and the SDK checks below ran under a
named serial build slot. Independent validation and integration approval remain
separate gates.

A separate focused follow-up review confirmed the source-level dispositions
and found no concrete regression in those fixes. The first executed SDK run
collected and passed 28 tests; adding the immutable-oracle adapter raised this
to 29. Clippy also passed after boxing optional service-error details rather than
suppressing the large-error lint. A subsequent whole-slice review found a valid
service error code was lost if its message was missing/null; code extraction is
now independent of typed detail decoding, with regression cases. Final
exact-commit results are recorded separately at handoff; source review does not
replace independent validation or integration approval.

## Independent reference adapter and remaining compatibility scope

The focused workspace SDK unit suite reads all eight existing cases from
`rust/tests/compatibility/workspaces/cases.json` without editing or regenerating
them. It compares raw service-order IDs, unmodified workspace fields, request
URLs, credential scopes/counts, and service error-code precedence. CLI name
suffixes, unknown-type filtering, sorting, JMESPath, paths, streams and exit codes
remain outside the SDK.

Two observed differences are explicitly tested rather than hidden:

- `token_pagination` supplies foreign continuation URIs. Python ignores them and
  requests three pages. This SDK returns `InvalidContinuation` after the first
  decoded page with explicit progress and makes no second credential/request.
  Separate valid-origin tests check three-page ordering and opaque-token encoding.
- `missing_value` raises a Python `KeyError` mapped by the CLI to `UnexpectedError`.
  This SDK returns typed `InvalidResponse`, with no success-shaped empty list.

For HTTP errors with valid error codes but incomplete messages, retain the code
even when a fully typed `FabricServiceError` is unavailable. Error `Display` stays
sanitized; the future CLI owns human-readable Python error-message projection.
Accepted/LRO responses, transport retries and sleeping/timeouts are explicitly
unimplemented SDK policies, not claimed parity.

The adapter is an implementation-side consumer of independently authored
expectations. It does not mean the validation lane independently ran the new Rust
code, and it does not rerun or replace the Python oracle. Full-workspace checks,
native macOS/Linux execution, production HTTP/auth, retry/LRO parity and CLI
compatibility remain integration or later-slice gates.

The item-bound subtraction relies on the invariant that accumulated items never
exceed the bound: validate a whole page before extending progress. Preserve that
ordering; exact-bound and over-bound cases guard it. A speculative redesign of
this arithmetic is not needed for the current implementation.

## Copilot migration practices adopted for this slice

The [Copilot runtime migration retrospective][runtime-retrospective] names
`github/copilot-agent-runtime` as private. Its internal `AGENTS.md`, coding
skills and review automation were not available for inspection. The article's
rebase-review/coding-skill descriptions are process evidence, not verified skill
bundles to install. No public runtime skill was copied or enabled.

The separate public `github/copilot-sdk` is a secondary comparison, not the
runtime repository. References below use Fabric's existing pinned SDK baseline
`51e9556d2607f00f9f72038fa16accfe2f5d150d`, rather than silently tracking HEAD.

| Public evidence | Adaptation to Fabric SDK |
| --- | --- |
| [Rust API/error and handwritten/generated type guidance][sdk-rust-guidance] | Review downstream construction and error matching, not just function signatures. Preserve required/optional fields and unknown workspace types deliberately. Do not add code generation or an extra model layer without a demonstrated need. |
| [Exact serialization and unknown-wire-value tests][sdk-wire-tests] | Assert required fields, wire names, unknown types, missing/null distinctions and round trips. Independently test the public operation; these model checks do not replace the oracle. |
| [Synthetic public-client protocol tests][sdk-protocol-tests] | Exercise the actual workspace-list method through the injected executor, asserting request order, termination and cleanup. Add a downstream consumer-style contract once the interface is approved. Do not import Copilot's JSON-RPC architecture. |
| [Async-specific Clippy configuration][sdk-clippy] | Review captures and guards across `await`, and prove the operation future is `Send` without creating a library runtime. If tracing is introduced, do not hold an entered span guard across `await`. Do not transplant unrelated lints or numeric future-size thresholds. |
| [Runtime retrospective: review iterations and migration failures][runtime-retrospective] | Keep semantics review distinct from Rust idiom review, preserve independent gates, and re-review the entire slice after substantive fixes or drift. Human owners retain architecture, compatibility-exception and integration decisions. |

The public SDK snapshot did not contain the runtime's skill files or the
referenced complete test/CI harness. Adopt the verified principles above, not
unavailable scripts, a Node/Python runtime requirement, forced rebases, automatic
merges, or new repository-wide lint/dependency policies.

[runtime-retrospective]: https://github.blog/ai-and-ml/generative-ai/migrating-the-github-copilot-runtime-to-rust-using-copilot/
[sdk-rust-guidance]: https://github.com/github/copilot-sdk/blob/51e9556d2607f00f9f72038fa16accfe2f5d150d/rust/AGENTS.md
[sdk-wire-tests]: https://github.com/github/copilot-sdk/blob/51e9556d2607f00f9f72038fa16accfe2f5d150d/rust/tests/api_types_test.rs
[sdk-protocol-tests]: https://github.com/github/copilot-sdk/blob/51e9556d2607f00f9f72038fa16accfe2f5d150d/rust/tests/protocol_version_test.rs
[sdk-clippy]: https://github.com/github/copilot-sdk/blob/51e9556d2607f00f9f72038fa16accfe2f5d150d/rust/clippy.toml

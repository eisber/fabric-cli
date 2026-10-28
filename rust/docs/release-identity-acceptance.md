<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Identity release acceptance packet

**Disposition: offline preparation complete; live acceptance and production
support remain unproved.** This packet inspects published Rust integration
`b7d45d3cc1a1c80f17fe0cebccbdc397439b81fd` from `eisber/fabric-cli`, not the
old scaffold. Python compatibility remains pinned independently to
`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`. No live authentication, provider or
account enumeration, token acquisition, service calls, consent, tenant policy,
registration, trust-store or settings changes were performed for this packet.

The [identity requirements and E1-E6 exceptions](identity-requirements.md),
[backend decision](decisions/identity-backend.md),
[native host contract](native-read-host.md) and
[architecture](architecture.md) remain the product contracts.
This checklist does not select a production backend, approve live execution,
waive compatibility checks or replace Fabric CLI team authentication/core review.

## What the current source actually implements

All paths below are under `rust/` at the inspected integration SHA. Source
inspection is evidence of code presence, not evidence that native acquisition or
tenant policy succeeded.

| Boundary | Current source and invariant | Evidence limit |
| --- | --- | --- |
| Optional feature and API | `infrastructure/identity/fabric-identity/src/lib.rs`: only Windows plus `windows-broker` exports the real credential. The original synchronous `interactive_credential` still returns `Unsupported`. SDK consumers inject `TokenCredential`, independent of CLI/identity implementation. | The unsupported original entry does **not** mean the experimental async provider is missing. |
| Explicit selectors | `fabric-identity-windows/src/entra_contract.rs::EntraOptions`: tenant/client GUIDs, explicit UPN, fixed public-cloud tenant authority. `entra.rs::find_account` matches the UPN case-insensitively, rejects duplicate matches, and honors a pinned native ID during refresh. | An unpinned initial search matches UPN, not a decoded tenant claim. Returned authority is checked later. Aliases/guest forms require proof, not relaxed matching. |
| Acquisition and audiences | `fabric-identity/src/windows.rs::WindowsCredential::login` requests Fabric, OneLake, ARM sequentially. `entra.rs::acquire` attempts selected-account silent acquisition first; interactive acquisition requires explicit caller permission and either interaction-required or an uncached explicit identity. All audiences must succeed before publication. | Cancellation, account switch, malformed metadata and generic provider failure do not authorize UI/fallback. Partial failure may already have changed WAM cache. |
| Provider response | `entra_contract.rs::response_identity`, `unix_expiry`, `request_status`: UPN/native-ID continuity and exact selected public-cloud tenant authority; expiry is authoritative `TokenExpiresOn` seconds since 1601, subtracting 11,644,473,600. Missing/malformed/expired metadata fails. | Tokens remain opaque. Scope selection/response metadata is not independent proof of service authorization or tenant policy. |
| Silent refresh/status | `windows.rs::TokenCredential::get_token` allows exactly one known Fabric, Storage or ARM scope and silently reacquires with the established native ID. `hosts/cli/fabric-cli/src/auth_commands.rs` forces status silent, using the same three-audience login path. | `metadata()` is the prior successful acquisition's metadata, not a new status check. SDK error wrapping retains `BrokerError`; CLI maps native/provider failures to bounded sanitized categories, not a complete CA/CAE taxonomy. |
| Native lifetime | `fabric-identity-windows/src/native.rs` owns a dedicated STA, hidden parent window, message pumping, completion/cancellation and window close. `request.rs` suppresses a late success after cancellation before delivery. | Drop requests cooperative cancellation, not rollback or guaranteed immediate termination. `CompletionUnconfirmed` can be delivered while native resources remain pending. A delivered credential is not globally revoked by dropping another future. |
| Logout | `entra.rs::logout` finds only the explicit application's matching account and calls `SignOutWithClientIdAsync`; missing account is idempotent success. | App/client on this device only: no Windows-account removal, global revocation or Python settings/navigation reset. No cross-operation lock/generation barrier prevents an independent refresh racing logout; race acceptance is open, not claimed fixed. |
| CLI path | `auth.rs::WindowsBroker` delegates to this credential and sign-out, through `session::run_auth` and `auth_commands`. Success projects scopes/expiry plus `mfa_evidence: not_checked` and `service_access_verified: false`. Off Windows it returns `unsupported`. | CLI success is not MFA/CA or Fabric/OneLake/ARM service proof. The command's test broker substitutes whole login, not WAM internals. |
| Native service host | `native_read.rs::execute` uses explicit `FAB_RUST_TENANT`, `FAB_RUST_ACCOUNT`, `FAB_RUST_CLIENT_ID`; `WindowsCredential::connect` silently acquires **Fabric only**. Native reads cannot open login UI or imply OneLake/ARM consent. | Separate explicit login still requires all three audiences. Environment selectors are non-secret identity configuration, not raw token inputs. Missing/invalid configuration fails before credential setup. |
| Service networking | `native_read.rs::execute` selects `ProxyPolicy::None` and platform trust with no additional roots. `infrastructure/http/fabric-sdk-transport/src/config.rs` exposes caller-selected direct/static unauthenticated HTTP proxy and platform/additional DER roots. | No CLI proxy/extra-root configuration is wired here; SDK capability is not CLI policy. PAC, integrated/authenticated proxies and client TLS certificates are unsupported. Broker networking is separate from service transport. |

No automatic alternate-account/tenant/audience/backend, browser-only,
`DeveloperToolsCredential`, `az`, `azd`, Python, static/raw-token or plaintext-cache
fallback is introduced by this packet. WAM owns token persistence; the
experimental provider does not migrate Python cache or persist a Rust auth
profile. The remembered-profile workstream artifact is a synthetic proposal,
not implemented persistence or a production support approval.

### Exact existing experimental evidence

[Architecture's October 23 record](architecture.md#native-isolation-validation-on-october-23-2026)
records Rust 1.99.1 on `x86_64-pc-windows-msvc`: 32 targeted actual-login-slice
tests (19 native/Entra-contract, two identity, three core, five CLI, three
architecture), plus default contracts, targeted Clippy, formatting, safe consumers
and compile/link probes. The earlier isolation result names
`5f3294e58e8e054c8ade0a4e3de7d6cc3c4685de` plus lifecycle changes; the later
32-test paragraph does not identify a complete immutable tested tree. Neither
record can be promoted to fresh combined-candidate acceptance.

Current source contains seven pure Entra contract tests, nine synthetic
request-lifecycle tests, two real STA/hidden-window tests and a token/ID Debug
redaction test, plus identity scope rejection. These are **inventoried tests,
not tests rerun by this packet**. STA/window tests do not acquire a provider or
token. The six command-engine test functions in `tests/auth_commands.rs` cover
injected login/status/logout, errors, cancellation/deadline and output failure;
they cannot prove WAM's internal sequence or policy. See
[command-engine evidence](auth-command-evidence.md).

[Native-host evidence](native-read-host.md#offline-evidence-versus-live-setup)
separately records source-synchronous GET at
`c72860db65e821ada2ec3cbac8f34c4bc7458042` and frozen observations
`5ff98d8360cbd6f2939790b3aa8c7f2b4cd90781` /
`075375250875c0c1b0a43de5993c4c673dd9d0c1`. Its synthetic production-session
executor and actual offline transport wire tests are not tenant proof. The
document preserves failed artifact receipts and subsequent corrections rather
than claiming one green batch. Private receipts were not fetched or rerun here.
All required native combined-head checks remain the integration owner's gate.

Repository research already pins official prior art in
[requirements R15-R25](identity-requirements.md#public-reference-register):
desktop WAM interop, silent requests, request statuses, app sign-out, cancellation,
opaque access-token guidance, released WinGet and historical MSAL.NET 4.30.1
`0661b34a41255bfdd7980663572be0544578117e`. The provider fields have released
implementation evidence; calling them wholly unknown based on the old scaffold
would be wrong. Those sources do not certify support for this custom Rust adapter.
No new third-party research upload or production dependency is needed.

## Future authorized acceptance, not an execution script

Before any live action, the environment owner must approve a restricted scope
record: exact test tenant GUID, explicit account/principal aliases mapped to
private selectors, client registration, guest home/resource tenant and mismatch
identities, one disposable workspace ID, one item/resource ID, ARM subscription
ID, exact endpoints, candidate source/artifact digests, native platforms/features,
time window, allowed operations, cleanup/recovery plan and responsible operator.
No UPNs, private account names, tokens, assertions, certificates, provider dumps
or secrets belong in this repository. Opaque references resolve only through
the owner's existing protected evidence store; the checker never resolves them.

The bounded schema describes Windows x64, public cloud, the existing experimental
WAM provider, read-only service probes, and a separate non-Windows negative
artifact at the same source revision. It is not approval of even a preview
release or a support promise for the negative platform. One scope per packet
avoids ambiguous workspace/resource cross-products. Other platforms, clouds,
endpoints, mutations or identity modes require a separately reviewed contract,
not loosening this checklist to make it pass.

Allowed side effects must explicitly include broker cache updates and app-scoped
sign-out; service writes/deletes are excluded. MFA/CA tests use existing
owner-approved policy scenarios. Do not change consent, policies, app settings,
OS accounts or trust stores to manufacture a passing result. If a necessary
scenario cannot be safely exercised under that scope, record `blocked`/`not_run`;
the checklist must remain rejected. Approval cannot be inferred from a report.

| Case IDs in the schema | Required observation in owner-held sanitized evidence |
| --- | --- |
| `selection_missing`, `account_mismatch`, `tenant_mismatch`, `account_ambiguous` | Missing/invalid/ambiguous selectors fail explicitly; attempted substitution/account switch never publishes success or selects another identity. Record selected aliases, safe numeric error/category and zero unintended effects, not enumeration dumps. |
| `guest_tenant` | Approved guest matches the exact resource tenant and native identity across all audiences/refresh. Unknown alias/guest combinations fail explicitly; guest support is not assumed. |
| `audience_sequence`, `partial_acquisition` | Fabric `https://api.fabric.microsoft.com/.default`, OneLake `https://storage.azure.com/.default`, ARM `https://management.azure.com/.default`, in that order, with consistent identity. Failure partway produces no successful/partial session; disclose broker-cache effects and recovery. No raw tokens or decoded claims. |
| `mfa`, `conditional_access` | Actual approved MFA challenge and representative CA allow/deny/step-up results, exact policy evidence references and platform/device prerequisites. SSO, MFA, device compliance, token protection and CAE are different claims. A login success or `not_checked` cannot prove these. CAE/challenge replay remains unimplemented, not waived. |
| `expiry_metadata`, `silent_refresh`, `silent_interaction_required` | Broker expiry/renewal without guessed lifetimes; silent status and SDK refresh remain identity-pinned and show no UI. Expired/revoked/step-up paths fail distinctly without retry storms or fallback. Pure malformed-expiry tests complement, but cannot replace, native renewal proof. |
| `cancel_lifecycle` | Cancel before native launch, during UI/request, after callback/before delivery and on deadline. Capture no successful late publication, bounded observation interval and pending-cleanup uncertainty. Cancellation does not roll back cache effects. |
| `signout_recovery`, `signout_refresh_race` | App/device-only sign-out, repeated/missing-account behavior and failure visibility; approved concurrent refresh/logout cannot silently resurrect the intended signed-out application session. If the current implementation fails this requirement, keep the gate failed and route remediation to auth owners. Never remove an OS/shared account. |
| `provider_absent`, `platform_absent` | Approved broker-unavailable Windows condition and actual non-Windows candidate: explicit classified failure, no provider/browser downgrade or tool launch. Non-Windows `unsupported` is the **expected negative result**, not a supported-login claim. Do not uninstall a broker to create the scenario. |
| `no_developer_tools`, `no_raw_token_fallback` | Clean native machine with Python/az/azd absent; isolated trap processes/ambient synthetic token-variable sentinels prove no invocation/probing/fallback. Never use real tokens as sentinels. Missing capability fails, not successful login. |
| `fabric_read`, `onelake_read`, `arm_read` | Separately approved minimal reads of the exact scope's Fabric resource, OneLake resource and ARM subscription, using their correct audiences. A broker token acquisition is not a service test. If the public frontdoor lacks a required read probe, leave it blocked pending a separately authorized consumer, not a raw-token workaround. |
| `secret_safe_outputs`, `teardown_recovery` | Safe metadata-only streams/errors, no secret/provider dump capture; validate redaction with distinctive synthetic values, not captured real credentials. After interrupted/partial tests, account for effects, complete scoped cleanup, retry only explicit same-identity recovery and retain failures. Report cleanup owner and outcome. |

For each grouped row, the evidence receipt must retain all subcases, actual
commands, platform/build, times, exits, expected/observed safe result categories,
effects, remaining uncertainty and cleanup. An unsupported/not-run subcase cannot
be summarized as a passed case. The metadata checker cannot inspect those
receipts; an independent reviewer must verify the hashes, contents, completeness,
authority and authenticity through approved read paths.

## Offline metadata checker

Files in `rust/tests/acceptance/identity/`:
`report.schema.json`, `check_report.py`, `test_check_report.py`,
`synthetic-complete.json`, `synthetic-negative-cases.json`.
All are Microsoft copyright, MIT licensed. JSON fixtures are newly authored,
synthetic test-only metadata; they contain no captured account, token or live
receipt. The complete fixture's apparent passes/approvals are fictitious.

The checker uses Python's standard library only as **development tooling**,
not a shipped authentication dependency. It reads one bounded local JSON file
and the adjacent pinned schema. No subprocess, network, credential/environment
discovery or login execution occurs in the checker. Its unit tests spawn only
the checker itself. No Cargo/root dependency or product source is changed.

Run from the repository root:

```powershell
python -B -m unittest discover -s rust\tests\acceptance\identity -p test_check_report.py -v
python -B rust\tests\acceptance\identity\check_report.py rust\tests\acceptance\identity\synthetic-complete.json --synthetic-test-only
```

The second command exits 0 with **`synthetic_test_only_not_acceptance`**.
Without that flag the committed fixture is rejected, exits 1 and writes only a
fixed diagnostic code to stderr. It must never enter a release evidence numerator.
Do not turn this fixture into a live report by changing its marker.

For a future, independently collected real report, omit the synthetic flag.
The only successful result is
`metadata_complete_not_verified_not_release_approval`, exit 0. This means
metadata completeness, **not** that authentication was executed, evidence was
retrieved, signatures checked, permission granted or release approved. Exit 1
has no success stdout. Unknown fields, malformed/oversized/duplicate-key JSON,
missing scope/evidence, ambiguous targets, stale source/artifact/scope bindings,
offline-only policy evidence, missing cases, unobserved/not-run/blocked/failing
statuses, invalid execution windows and incomplete signoffs are rejected.
The allowlist is not a secret scanner: never supply secrets, even in a hash/ref field.

Scope SHA-256 uses ASCII JSON with sorted keys, compact `,`/`:` separators,
no trailing newline. The packet hash uses the same canonicalization over all
top-level fields except `signoffs`. Authorization binds the exact scope;
receipts bind scope, source, target-specific artifact and case IDs; each signoff
binds both scope and complete packet. A changed candidate needs fresh evidence
or an independently justified reuse decision, never automatic hash relabeling.
Execution must fall within the approval window; receipt/signoff times cannot
precede their observations or be in the future. Historical evidence may be
reviewed after its execution authorization expires; that grants no new execution.

The JSON Schema is the closed structural contract. The checker deliberately
implements only its used local `$defs` references, types, constants/enums,
full-string patterns, object properties/required/unknown-field rejection, array
bounds/items/uniqueness. Unknown encountered schema keywords fail closed.
Semantic gates (all cases, native evidence kinds, bindings, dates and four
signoffs) are additional; a generic structural schema pass alone is insufficient.

Required human roles are release owner, identity support owner, Fabric CLI team
reviewer and environment owner. Alias/ref strings and hashes alone do not prove
those humans signed anything. Independent semantics/completeness and
counterexample/evidence reviews remain required by
[agent-operated delivery](agentic-delivery.md#stage-reviews).
No independent review or live support signoff is claimed by this lane.

## Decisions for the owner and Fabric CLI team

These are recommendations for consolidation, **not selected defaults**. They
block only the named release/implementation gates, not this offline packet.

| ID / accountable owner | Options | Recommendation and tradeoff | Blocking task |
| --- | --- | --- | --- |
| I1 / release owner | Windows x64 opt-in preview; broader platform preview; GA replacement | Evaluate bounded Windows preview first with explicit exclusions and the same required checks. Smaller evidence surface; no GA or cross-platform promise. | Published platform/support contract and release promotion. |
| I2 / owner + auth team | Renewable service-principal federation; service-principal secret/certificate; managed identity on an approved Azure host | Prioritize renewable federation for hosted CI, then the actual deployment environment's MI requirement; evaluate certificate only with concrete format/rotation/native-dependency needs. Federation needs an issuer/trust and assertion-renewal contract; MI is host-bound; secrets add rotation/storage risk; certificates add key/native prerequisites. No adapter or trust change chosen here. | Explicit unattended mode/input/renewal acceptance and separate reviewed adapter work. |
| I3 / auth team + support owner | Windows-only unsupported elsewhere; entirely Rust platform-specific supported integration investigation; reconsider a packaged official-platform helper as a new decision | Keep non-Windows explicitly unsupported until the team identifies a supported interface, packaging path and named support owner. Honors entirely-Rust direction; delays multi-platform availability. Browser OAuth/community crate is not automatic broker/CA parity. | macOS/Linux backend, secure persistence and native support promises. |
| I4 / CLI + enterprise networking owners | Explicit per-client CLI config; documented opt-in ambient/system policy; direct-only preview exclusion | Prefer validated explicit CLI config and explicit direct-only exclusion until implemented. Predictable/no bypass fallback; administrators may prefer ambient policy, which needs precedence, PAC/auth/bypass and error contracts. Current host is direct-only although SDK supports static HTTP proxy. Do not add implicit environment parsing. | Enterprise proxy CLI design plus broker/service-network acceptance separately. |
| I5 / security/platform support owner | Platform-managed roots only; explicit per-client additional roots; centrally managed deployment policy | Prefer platform roots initially and reviewed per-client additions only for demonstrated needs. No TLS-disable or root-store mutation. Deployment-owned roots simplify control but require rotation/revocation responsibility. Unix platform-loader `SSL_CERT_FILE`/`SSL_CERT_DIR` behavior is documented in transport `TrustPolicy`; it is not CLI proxy discovery or Windows WAM configuration. | Trust configuration/support ownership, root rotation and enterprise TLS proof. |
| I6 / release + cloud owners | Public cloud only; explicitly named sovereign cloud(s) | Public cloud first. Existing audience/authority allowlists and eligible Fabric endpoint overrides are not sovereign-cloud support. Broader clouds need coordinated authority, audiences, endpoints, registration and policy proof. | Cloud support matrix and additional-cloud implementation authorization. |
| I7 / test environment owner | Exact disposable tenant/resources with read-only services plus explicit cache/sign-out effects; defer live acceptance | Approve the bounded scope described above through the existing owner path, or explicitly defer release. Requires exact tenant/workspace/item/subscription, private alias resolution, app/registration review, time window, approved MFA/CA scenarios and cleanup operator. No broad tenant wildcard or inferred permission. | All actual native broker/MFA/CA/guest/service acceptance. |
| I8 / Fabric CLI team + release owner | Named independent restricted-area reviewer/support owner now; defer production claim | Name reviewers for the real WAM/provider contract, native lifetime, logout races and error/unsupported boundaries, and a support owner for registration/provider drift. Maintains accountability; may require implementation repairs before any support promise. Historical official source is not vendor certification. | Auth/core approval, independent packet review and production support signoff. |

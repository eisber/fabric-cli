<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Identity requirements and evidence

**Status: backend-neutral requirements, not an authentication implementation or
backend selection. Evidence reviewed on October 22, 2026 (UTC).**

The [identity backend decision](decisions/identity-backend.md) remains unresolved.
This document inventories the Python contract, approved Rust constraints,
documented platform capabilities, and the proof needed before production use.
Authentication changes require Fabric CLI team review. Nothing here authorizes
login, consent, tenant configuration, installation, certificate or OS-store
operations, or live Fabric, OneLake, or ARM calls.

## Evidence scope and status vocabulary

The Python source baseline is
[`cf67fd52ea8b47e24c725e73d792de6f4a08e2e4`](https://github.com/microsoft/fabric-cli/tree/cf67fd52ea8b47e24c725e73d792de6f4a08e2e4).
The Rust foundation inspected is
[`eccfca1d3b1cb4df71d0fa65a76221a588095290`](https://github.com/eisber/fabric-cli/tree/eccfca1d3b1cb4df71d0fa65a76221a588095290),
fetched from `eisber/fabric-cli`, branch `feature/rust-workspace-scaffold`, and
integrated by fast-forward into the identity lane. Python `src`, `tests`, `docs`,
and `pyproject.toml` are unchanged between these commits.

| Label | Meaning in this document |
| --- | --- |
| Supported: scaffold | Present in the inspected offline Rust contract; not evidence of authenticated service behavior |
| Supported: documented | A named official library documents a capability, with platform prerequisites; not a Rust product support claim or an executed tenant test |
| Unknown | Required integration, support ownership, packaging, or policy proof is missing |
| Unsupported: scaffold | No implementation exists at the foundation checkpoint |
| Unsupported: product constraint | Excluded by the approved Rust requirements, even if Python or an upstream library implements it |

No production Rust authentication mode is supported by the scaffold.
`interactive_credential` returns `IdentityError::Unsupported`; there is no
unattended adapter, cache, browser launch, helper, credential chain, or token
acquisition. Existing offline tests and source inspection are evidence of
boundaries, not successful native authentication. No live authentication or
platform-policy tests were executed for this document.

## Pinned Python behavior

Source IDs below identify files at the Python baseline, not current upstream
`main`. Test names identify existing behavioral evidence; they were inspected,
not executed as part of this documentation task.

| ID | Exact baseline source | Relevant surface |
| --- | --- | --- |
| P1 | [core/fab_auth.py](../../src/fabric_cli/core/fab_auth.py) | `FabAuth`, environment precedence, account selection, token acquisition, persistence, logout, certificate loading, authority |
| P2 | [commands/auth/fab_auth.py](../../src/fabric_cli/commands/auth/fab_auth.py), [parsers/fab_auth_parser.py](../../src/fabric_cli/parsers/fab_auth_parser.py) | Login choices/arguments, cancellation, eager scope acquisition, status, logout |
| P3 | [core/fab_constant.py](../../src/fabric_cli/core/fab_constant.py), [client/fab_api_client.py](../../src/fabric_cli/client/fab_api_client.py) | Scope/audience constants, authority/client ID, defaults, endpoint routing |
| P4 | [core/fab_state_config.py](../../src/fabric_cli/core/fab_state_config.py), [utils/fab_secure_io.py](../../src/fabric_cli/utils/fab_secure_io.py) | Configuration location, read/write behavior, POSIX permissions and Windows limitations |
| P5 | [core/fab_msal_bridge.py](../../src/fabric_cli/core/fab_msal_bridge.py) | Headless Python Azure credential bridge, Fabric-only scope allowlist and expiry conversion |
| P6 | [tests/test_core/test_fab_auth.py](../../tests/test_core/test_fab_auth.py) | Mocked user/unattended acquisition, drift, certificates, migrations, authority and POSIX permissions |
| P7 | [tests/test_core/test_fab_auth_azure_cli.py](../../tests/test_core/test_fab_auth_azure_cli.py), [tests/test_core/test_fab_msal_bridge_azure_cli.py](../../tests/test_core/test_fab_msal_bridge_azure_cli.py) | Azure CLI dispatch/isolation/drift, bridge token conversion and rejected scope |
| P8 | [tests/test_commands/test_auth.py](../../tests/test_commands/test_auth.py), [tests/test_parsers/test_fab_auth_parser.py](../../tests/test_parsers/test_fab_auth_parser.py) | Prompt/argument handling, cancellation, status output and logout side effects |
| P9 | [docs/examples/auth_examples.md](../../docs/examples/auth_examples.md), [docs/essentials/settings.md](../../docs/essentials/settings.md), [docs/troubleshooting.md](../../docs/troubleshooting.md) | Published Python usage, managed-identity validation limitation and plaintext-cache advice |
| P10 | [pyproject.toml](../../pyproject.toml) | `msal>=1.34,<2`, broker extra only when the platform is not Linux, unpinned `msal_extensions`, `azure-identity>=1.25.0` |

P10 gives dependency ranges, not the exact environment used for every Python
installation. Do not infer a locked MSAL version, native binary inventory, or
successful platform coverage from those declarations.

### Observed behavior versus migration obligation

| Surface | Observed Python behavior | Translation/proof obligation |
| --- | --- | --- |
| CLI modes (P2, P8, P9) | Interactive browser, Azure CLI, service principal secret/certificate/federated assertion, and managed identity are choices. Argument-based unattended modes also exist. `--username` is a client ID, not a user-account selector. | Map each mode's inputs, outputs, failures and side effects independently; Rust has not ported these commands. Developer-tool and raw-token paths are exceptions below. |
| User acquisition (P1, P6) | `acquire_token` tries silent acquisition using `get_accounts()[0]`, then interactive acquisition when allowed; interactive requests use `prompt="select_account"` and the MSAL console window handle. Successful interactive `tid` becomes the session tenant. | Preserve silent-before-explicit-interaction ordering, but do not port first-account selection. Confirm stable account and target-tenant binding in both paths. |
| Restored account recovery (P1, P6) | `_select_cached_account` prefers an account whose `realm` matches the session tenant, then returns `accounts[0]`. This helper is used for principal recovery, not the main silent-acquisition selection above. Guest home tenant is not the target realm. | Keep recovery and acquisition traces distinct. Prove guest/resource-tenant handling without an unmatched-account fallback. |
| Authority (P1, P3, P6) | No saved tenant gives `https://login.microsoftonline.com/common`; otherwise authority is `https://login.microsoftonline.com/{tenant}`. Legacy saved authority is removed. Public-client ID is `5814bfb4-2705-4994-b8d6-39aabeb5eaeb`. | Treat the client ID as a public identifier, not evidence that Rust/helper redirect URIs, consent, or redistribution are approved. Explicit tenant selection differs from Python discovery. |
| Broker defaults (P1, P10) | User application construction enables Windows and macOS broker flags. It does not enable Linux/WSL broker flags; the Linux package declaration omits the broker extra. | Opt-in flags and declared extras are not proof the broker was available or used. Linux broker support is not observed Python parity. |
| Eager login scopes (P2, P3) | Login acquires Fabric, OneLake, then ARM tokens before reporting success and updating navigation context. | Record ordering and partial-failure behavior. Lazy/per-operation acquisition or omitting ARM is a separate behavior decision, not silent translation. |
| Environment inputs (P1, P6) | `FAB_TENANT_ID`, service-principal variables and `FAB_MANAGED_IDENTITY` can configure the singleton. Service-principal/managed-identity/Azure-CLI modes bypass raw-token selection. User/other paths can use paired `FAB_TOKEN` and `FAB_TOKEN_ONELAKE`; ARM additionally requires `FAB_TOKEN_AZURE`. | Record precedence, missing-input and identity-drift cases as Python oracle data. No automatic ambient provider discovery or raw-token Rust fallback. |
| Identity drift (P1, P6, P7) | Raw environment tokens are compared across tenant/principal and saved session. Azure CLI identity changes trigger `logout_session`, clearing authentication, memory caches and navigation state. | Preserve the invariant that identity-bound cached data is not reused after a switch; excluded providers do not remove this requirement for future native adapters. |
| Cache/configuration (P1, P4, P9) | `~/.config/fab/` holds `config.json`, non-secret auth metadata in `auth.json`, and `cache.bin`. `PersistedTokenCache` uses encrypted persistence; opt-in `encryption_fallback_enabled == "true"` permits `FilePersistence`. Default is `"false"`. | Secure persistence failure must remain an error; the Python troubleshooting recommendation to enable plaintext is not valid Rust guidance. Path and migration compatibility need separate proof. |
| Permissions (P4, P6) | POSIX helpers use owner-only file/directory modes `0600`/`0700`, tighten existing paths, and log chmod failures at debug level. Windows chmod helpers do nothing and rely on user-profile ACLs. Acquisition tightens the cache in a `finally` block. | Inspect native ACLs and secure-store behavior; POSIX-only tests explicitly skip Windows and do not establish Windows isolation. Restrictive permissions do not substitute for encryption. |
| Failure/cancellation (P1, P2, P6, P8) | Prompt cancellation returns `False` or `None` depending on the path; the prompt branch catches `KeyboardInterrupt`. Many missing-token/provider-error outcomes become `AuthenticationFailed`. The bridge disables interactive renewal. | Preserve the oracle, but prove the approved distinct Rust error categories. A generic authentication/refresh error is not acceptable classification for every event. |
| Status (P2, P8) | Noninteractive token lookups can refresh silently. Status shows metadata and the first four token characters followed by masking; lookup failures/drift can change session state. | Do not describe status as purely local or side-effect-free. Any Rust output change, including removing token fragments, needs an explicit compatibility expectation. Never include real token fragments in fixtures. |
| Logout (P1, P8) | Deletes the local cache file, clears auth metadata and cached app/CLI credential, resets several CLI settings (including capacity/ARM defaults), then clears memory caches and navigation context. | Inventory all local effects before porting. This code does not demonstrate revoking server sessions, signing out the OS broker, clearing browser cookies, or removing another application's account. |
| Certificate input (P1, P6, P9) | Accepts PEM and PKCS12 (`.pfx`/`.p12`), including passwords; parses key/cert, computes a SHA-1 certificate identifier and passes a private-key credential to MSAL. PKCS12 is converted to PEM in memory. | Match format/error/password behavior, not just one successful PFX case. Thumbprint modernization, native-store/HSM keys and format changes are separate decisions. No certificate/store operation is part of this task. |
| Python SDK bridge (P5, P7) | Wraps global `FabAuth`, accepts only the Fabric `.default` scope, never prompts, converts `expires_on` or `expires_in`, and errors if expiry is unavailable. `tenant_id` and `enable_cae` are documented as unused; `claims` is not forwarded. | Rust uses caller injection instead of this global bridge. This source does not establish claims-challenge or CAE support; adding them is new behavior requiring its own contract and proof. |

Representative existing oracle cases include
`test_get_access_token_user_silent_success`,
`test_get_access_token_user_interactive_success`,
`test_get_access_token_user_interactive_error_response_failure`,
`test_select_cached_guest_account_matches_realm_not_home_tenant`,
`test_env_var_token_session_principal_drift_logs_out_session_failure`,
`test_env_var_token_recovery_error_logs_out_session_failure`,
`test_acquire_token_tightens_cache_file_on_error_success` (P6),
`test_tenant_drift_logs_out_failure`,
`test_principal_drift_logs_out_failure`,
`test_user_identity_does_not_invoke_azure_cli_success` (P7), and
`test_auth_logout`, `test_init_when_user_cancels_the_prompt`,
`test_azure_cli_token_acquisition_error_propagates_failure` (P8).
These mocked cases do not prove broker, secure-store, or tenant-policy behavior.

## Approved constraints and explicit compatibility exceptions

These constraints come from the agreed foundation's
[migration guidance](migration-guidance.md), [architecture](architecture.md),
[compatibility backlog](compatibility.md), [identity decision](decisions/identity-backend.md),
and this lane's assigned requirements. They are not conclusions that Python
already behaves this way. Backend selection and any additional UX redesign
remain unapproved.

| Exception | Approved Rust constraint | Required compatibility evidence |
| --- | --- | --- |
| E1: selection | No implicit first-account, tenant, audience, or backend fallback. Require an explicit non-secret selection; an explicitly confirmed selection may be restored only within its isolation boundary. | Multiple accounts in one tenant, multiple tenants, guest accounts, empty/missing selectors and reordered caches must never pick an unintended identity. Define CLI selection UX separately. |
| E2: installation | No production dependency on Python, Azure CLI, or Azure Developer CLI. No `DeveloperToolsCredential`/`az`/`azd` chain. | Native clean-machine runs with these tools absent and a trap executable/environment proving no invocation or credential probing. Explain the excluded Python `--azure-cli` surface in rollout expectations. |
| E3: raw tokens | No production raw/static-access-token provider, environment-token fallback, or credential command-line arguments. | Python token variables alone must not create a session or trigger a service call. Their presence must not replace an explicitly injected provider. An isolated, expired, test-only in-memory credential is not a production mode. A renewable federated assertion input is distinct from a pre-acquired Fabric access token and needs a separately approved input contract. |
| E4: persistence | No plaintext credential persistence, including opt-in fallback. Do not copy the Python troubleshooting workaround. | Secure-store missing/locked/denied and encryption failures produce explicit errors, no plaintext files, and no success-shaped login. Decide any explicit memory-only mode separately; never select it silently. |
| E5: layering | Direct SDK credentials are caller-injected, independent of `fabric-identity` and CLI/global session state. Libraries never prompt, exit, initialize global logging/runtime, or depend on mutable CLI context. | A standalone consumer using a supplied `TokenCredential` must compile/run offline without CLI/identity dependencies or ambient authentication. Missing configuration must not launch UI. |
| E6: failure semantics | Preserve distinct unsupported, missing-configuration, interaction-required, cancelled, and refresh-failed outcomes; do not map all Python `AuthenticationFailed` cases to refresh failure. | Provider-specific error fixtures, sanitized CLI output/exit contracts and cancellation cleanup must be reviewed against the Python oracle and this intentional difference. |

Exceptions require explicit expected results and review before the affected slice
claims compatibility. This requirements record is not a waiver or a parity test
result. Other changes (lazy scope acquisition, new device-code UX, cache-path
migration, logout redesign, CAE/claims-challenge support, different status output)
must be proposed separately rather than bundled into translation.

## Backend-neutral credential boundary

The SDK accepts `Arc<dyn azure_core::credentials::TokenCredential>`, reusing the
caller's runtime. It must not construct a default credential chain or import
`fabric-identity`/CLI to obtain credentials. The provider owns acquisition and
refresh; selection metadata alone does not bind or verify it.

At the foundation checkpoint, `CredentialSelection` checks only nonblank account
and tenant strings and carries an `Audience`. It does not validate GUIDs,
authority, cloud, account existence, identity drift, or returned credentials.
An opaque account selector is not a password, access token, display name
guaranteed unique across tenants, or a proven OS-account binding.

| Dimension | Required contract and proof | Foundation status |
| --- | --- | --- |
| Account/principal | Caller-selected stable provider identifier; guest/resource-tenant identity must be distinguishable from home-account identity. Reject missing, ambiguous or mismatched selections without falling back. | Supported: scaffold metadata; actual provider binding unknown |
| Tenant | Explicit target tenant for the selected account/application. Changing tenants requires a new approved selection and invalidation of identity-bound state. | Supported: scaffold nonblank metadata; validation unknown |
| Authority/cloud | Bind a selected tenant to an allowed authority and matching service endpoints. Never trust an arbitrary authority from a challenge or silently use `common`. Specify public-cloud defaults explicitly. | Unknown; no authority field or validation in the scaffold. Sovereign clouds unsupported: scaffold |
| Audience | Exact per-operation scope; no cross-service token reuse or automatic widening. Test endpoint-to-audience routing before sending credentials. | Supported: scaffold public-cloud scope mapping; HTTP enforcement unknown |
| App/client registration | Identify the public client or unattended application, redirect URIs, allowed tenants and support owner. Reusing Python's client ID does not prove helper/platform approval. | Unknown; no registration changes authorized |
| Provider binding | The supplying application must bind the injected credential to the selected identity. A trait object and a successful `get_token` do not certify that account/tenant or policy was used. | Caller responsibility; production verification unknown |
| Policy capability | Declare supported broker, interactive, persistence and challenge capabilities explicitly. Reject a required capability that is unavailable. | Unsupported: scaffold acquisition; no capability negotiation API yet |

The SDK must not assume every access token is a locally parseable JWT. Scope
requests, authoritative provider identity metadata, and approved service/policy
evidence have different roles. Unverified decoded claims or the presence of an
`oid`/`tid` string must not become a new identity-proof mechanism.

### Audience and scope inventory

| Service | Python request scope (P3), also represented by Rust `Audience` | Python raw-token validation audience (not a Rust implementation) |
| --- | --- | --- |
| Fabric | `https://api.fabric.microsoft.com/.default` | `https://analysis.windows.net/powerbi/api` |
| OneLake | `https://storage.azure.com/.default` | `https://storage.azure.com` |
| ARM | `https://management.azure.com/.default` | `https://management.azure.com` |

P3 routes both Python Fabric and Power BI API choices through the Fabric scope.
Do not silently rewrite that mapping or conflate a requested scope, an API
hostname, and a token's `aud` claim. Official OneLake documentation explicitly
requires the Storage audience (R9). Capture independently reviewed service
fixtures and, later, authorized service acceptance evidence before asserting
equivalence for each route.

## Authentication-mode requirements matrix

All future modes must be selected explicitly. A failure of one mode must not
try another provider or become interactive in an unattended process.

| Mode | Baseline/evidence | Rust status and exact acceptance work |
| --- | --- | --- |
| Interactive user, broker/OS SSO | P1/P2 opt into Windows/macOS brokers; R2-R5 document platform integrations. | Unsupported: scaffold. Establish a supported interface and package, explicit selection and cancellation, then prove each approved platform/policy/resource combination below. |
| Interactive user, browser | Python's selected interactive flow can use a browser; upstream broker initialization may fall back (R5). | Unsupported: scaffold; policy parity unknown. Browser MFA is not broker, device identity, device-bound token or CA parity. No silent downgrade when a broker capability is required. |
| Headless use of a previously selected user session | Python bridge/status disable interactive renewal; MSAL can refresh silently (P1/P2/P5). | Unsupported: scaffold. Test refresh, expiry, revoked consent/session and unavailable broker/store; return `InteractionRequired` when user action is needed, without UI or alternate-account discovery. Not equivalent to an unattended workload identity. |
| Device-code user flow | Not an exposed Python login mode in P1/P2. | Unsupported: scaffold; separate feature decision, not a parity shortcut. Requires explicit policy/device/resource proof and cancellation/expiry contract. |
| Service principal with secret | P1/P2/P6 document and mock client-credentials acquisition; R8 documents native Azure identity options. | Unsupported: scaffold. A separately reviewed explicit native adapter can be considered. Prove tenant/client identity, secret handling and renewal, error classification, no UI/persistence/argument leakage, and per-API service authorization. |
| Service principal with certificate | P1 supports PEM and PKCS12; R10 documents the released Rust certificate adapter's PKCS12/RSA and OpenSSL implementation. | Unsupported: scaffold. Prove format/password/rotation/error compatibility, native prerequisites and clean-machine packaging; do not infer PEM/native-store/HSM support from PKCS12 support. |
| Federated workload assertion | Python accepts a client assertion (P1/P2/P9); R8 shows a `ClientAssertionCredential` option. | Unsupported: scaffold. Specify renewable assertion source, intended issuer/subject/audience, expiry/rotation and cancellation. Prove exchange without treating an assertion as a reusable service access token or adding a fallback chain. |
| Managed identity, system/user assigned | P1 uses explicit managed-identity types and a resource derived by removing `/.default`; P9 limits its validation claim to Azure VMs. R8 documents a native option. | Unsupported: scaffold. Explicit system-assigned choice or specific user-assigned selector; prove supported host endpoints, bounded failure off-host, resource conversion, tenant/principal binding, and no ambient fallback. No blanket Azure-host or desktop support claim. |
| Azure CLI / Azure Developer CLI | Python offers `--azure-cli`; R8 documents that developer-tool credentials invoke `az`/`azd`. | Unsupported: product constraint (E2), not a production option or fallback. |
| Raw Fabric/OneLake/ARM token input | Python environment path (P1/P6). | Unsupported: product constraint (E3), including a static-token implementation presented as a temporary production provider. |
| Caller-injected credential | Accepted Rust SDK boundary; no production acquisition is performed by client construction. | Supported: scaffold interoperability only. Continue mock-credential/transport work offline; an injected provider does not establish enterprise policy or service success. |

Fabric identity support depends on the operation and underlying item, not only
successful token issuance. R7 requires the Fabric tenant setting for service
principals/managed identities and per-API support checks. Fabric, OneLake and ARM
authorization/roles must be proven separately; none may be inferred from another
audience's successful acquisition. A tenant administrator, not a library or this
document, owns permission and policy changes.

## Platform and enterprise-policy evidence matrix

| Platform/host | Supported: documented in official sources | Unknown or unsupported for this Rust product | Required proof before a support claim |
| --- | --- | --- | --- |
| Windows interactive desktop | R2 documents MSAL Python WAM on Windows 10+ / Server 2019+ with broker extra, app redirect URI and parent window handle. R3 documents MSAL.NET WAM, OS-account integration, device-bound refresh tokens and PoP capabilities. | Rust-to-supported-interface integration, broker-required enforcement, actual OS SSO and per-resource CA are unknown. WAM support in .NET/Python is not a supported generic Rust C ABI. Browser fallback in upstream libraries is not approved Rust behavior. | Record OS build/architecture, library/helper version, support owner, registration and UI handle; explicitly select among multiple/guest accounts; verify silent and prompted outcomes, cancellation, broker unavailable, and policy sign-in evidence for each target service. |
| Windows service, noninteractive task or impersonated session | R3 says WAM requires an active interactive user session and UI; service/impersonation contexts fail by design. | WAM is not an unattended workload credential. No Rust unattended adapter exists. | Prove no broker/UI invocation; use only a separately approved explicitly configured workload identity for an approved host. Classify unavailable UI/context clearly. |
| macOS interactive desktop | R4 documents MSAL Python broker support from 1.31, broker extra, Company Portal, redirect URI differences for signed/unsigned applications, and x64/ARM64 on macOS 10.15+. Broker owns token caching. | Rust integration/redistribution/signing, actual SSO, device-bound and CA acceptance remain unknown. Broker minimum OS support does not establish Token Protection policy eligibility. | Record OS/architecture, enrollment/Company Portal/SSO plug-in, app identity/redirect, keychain behavior, helper/signing/notarization and selected tenant/account. Prove the exact policy/service combination; R6 has different policy prerequisites and resource limits. |
| Linux interactive desktop | R5 documents MSAL Python opt-in `enable_broker_on_linux`, Linux broker/Intune prerequisites, redirect URI, device-bound refresh tokens, and required native packages. | Python baseline does not enable/package Linux broker. Rust interface, supported distro/desktop/architecture and policy support are unknown. R5's install example still names `1.33.0b1` although its parameter table says introduced in 1.33.0; do not treat a recipe as release certification. | Pin an officially supported released component/interface and supported distro/architecture matrix; prove broker/desktop dependencies, account selection, secure-store availability and exact tenant policies. Distinguish native Linux from WSL. |
| WSL, SSH, containers, headless macOS/Linux | R5 distinguishes Linux and WSL opt-ins; broker availability is host-specific. R11 documents Linux LibSecret persistence, which is not evidence of a broker. | No generic host/broker support or desktop SSO claim. A keyring and a browser do not create device-bound identity. | Explicitly classify supported/unsupported host configurations; absence of GUI/session bus/keyring/broker must fail safely without discovery, UI attempts or plaintext fallback. |

### Do not conflate policy capabilities

MFA proves completion of an authentication requirement in that flow. Browser
cookie SSO, OS-account SSO, a compliant-device grant, device-bound refresh tokens,
PoP access tokens, CA Token Protection session control and CAE are distinct.
Evidence of one is not evidence of the others.

R6 currently lists native Token Protection resources as Exchange Online,
SharePoint Online and Teams, with additional Windows resources; its ARM support
is a selected **web-application preview**, not arbitrary native CLI ARM support.
It does not establish Fabric/OneLake coverage or Linux availability. Its
availability table and Apple device section also differ in GA/preview wording.
Therefore generic "Token Protection works on every platform/audience" is
**unsupported by this evidence**, not a product commitment. Resolve the exact
resource, client type, platform and current support statement before using it as
an acceptance gate. Device-bound broker refresh tokens alone do not resolve that
question.

For any later authorized policy proof, retain a sanitized test record containing:
exact OS/architecture, broker/helper/library package versions, app registration
configuration, target resource/API, selected test identity/tenant aliases,
policy/grant/session-control configuration, device enrollment/compliance state,
expected allow/deny result, actual interactive and noninteractive sign-in
outcomes, and evidence date/reviewer. Test both an eligible device and a
disallowed/missing-capability configuration. Keep tokens, secrets and personal
claims out of fixtures/logs; store any permitted tenant evidence in its approved
location, not this public repository. No such proof is supplied here.

## Lifecycle, failure, persistence and isolation acceptance

These are future acceptance cases, not tests implemented by this document.
Use synthetic providers/fixtures first. Real store/broker/tenant checks require
separate authorization and platform ownership.

| ID | Requirement | Exact offline case and later native/policy proof | Current status |
| --- | --- | --- | --- |
| L1 | Missing selection never starts acquisition | Empty account/tenant, absent provider and ambiguous account fixtures must return `NotConfigured` or explicit selection error with zero UI/process/network/cache activity. Verify native account picker cannot silently replace the selection. | Supported: scaffold checks nonblank strings only; remaining proof unknown |
| L2 | Unsupported capability is explicit | Broker-required on unavailable platform, missing helper, unsupported audience/cloud/host: return `Unsupported` or a specifically classified configuration failure, no browser/provider downgrade. | Supported: scaffold interactive entry always rejects; real capability detection unknown |
| L3 | Interaction is caller controlled | Silent miss, expired/revoked refresh and consent/CA step-up fixtures distinguish an interaction requirement from acquisition failure; headless callers return `InteractionRequired`, never prompt. Later prove the same selected account/tenant is retained. | Error vocabulary present; provider mapping unsupported: scaffold |
| L4 | Cancellation is not failure or success | Cancel before launch, during UI/token request, after provider callback and before persistence; expect `Cancelled`, bounded cleanup, no new session publication and no late callback restoring a logged-out/cancelled session. Verify native UI/helper cancellation and cleanup separately. | Unknown; no production lifecycle |
| L5 | Refresh preserves identity and bounds | Fake clock and provider cases for expiry, renewal, denial, malformed/missing token/expiry and concurrent refresh. Return `RefreshFailed` only for actual selected-identity refresh failures, not cancellation/missing config. Prove no retry storm or silent identity/backend change. | Unknown; no adapter/cache |
| L6 | Transport and policy errors remain distinguishable | Proxy/DNS/TLS/timeouts and CA denial/interaction fixtures must not collapse into `RefreshFailed`. Preserve safe actionable diagnostics through the future shared error boundary. Decide typed mappings with core owner before implementation; current identity enum is not a complete network/store error taxonomy. | Unknown; error-boundary work required |
| L7 | Claims challenges are bounded and trusted | P5 does not forward claims; R12 describes challenge handling. Future work must validate origin/authority/tenant, bound challenge size and retry count, preserve headless behavior and selected identity, and reject invalid/repeated challenges. Only advertise CAE/client capability after full handling is proven. | Unsupported: scaffold; new feature, not established Python parity |
| L8 | Persistence is encrypted or explicitly fails | Simulate secure-store missing/locked/denied, malformed cache, disk-full and interrupted write. No plaintext file/fallback or successful login on failure; preserve prior valid state according to reviewed atomicity rules. Native ACL/keychain/LibSecret and recovery checks on each approved host remain required. | Unsupported: scaffold persistence |
| L9 | Isolation includes identity and application context | Test two accounts in one tenant, a guest in two tenants, two authorities/clouds, applications, audiences and OS users. Reject unauthorized cross-boundary credential access, account substitution, access-token misrouting and identity-bound service-cache reuse. Define cache keys/namespace and locking; test parallel processes/reload. | Unknown; metadata alone provides no isolation |
| L10 | Logout is local, scoped and race-safe | List Python's auth/config/cache/context effects separately. Fake logout concurrent with refresh/acquisition, repeated logout and deletion failure; no token/session resurrection or false success. Native proof must distinguish application cache deletion, broker account removal and server revocation; do not remove OS/shared accounts or promise global sign-out. | Unsupported: scaffold; exact native logout semantics unresolved |
| L11 | No credential disclosure | Inject distinctive dummy secret/assertion/token/password values into every success/failure/cancel path; none may appear in Debug, errors, logs, stdout/stderr, argv, snapshots, helper crash output or persistent metadata. Do not record real tokens to prove redaction. Review any Python status-output exception separately. | SDK client omits provider Debug; full adapter/CLI proof unknown |
| L12 | Library/CLI ownership stays separate | Mock acquisition failure must return to the SDK caller without exit, prompt, global logger/runtime or CLI context access. CLI alone maps approved errors to streams/exit codes and initiates explicit interactive UX. Test with ambient credential variables/tools present and absent. | Supported: scaffold separation; future paths require evidence |

An isolation design should explicitly bind OS user/application namespace,
authority/cloud, tenant, stable principal/application selector and audience.
This is a requirement, not a cache format selection. Broker-owned cache entries,
local metadata and service/navigation caches have different deletion and
sharing semantics. R11 documents encrypted persistence with file locking and
reload in the Python extension; it does not prove a Rust equivalent or automatic
interoperability with Python cache files. Approved broker SSO or provider-owned
multi-resource refresh may share an underlying sign-in session; isolation must
prevent unauthorized disclosure or substitution, not prohibit those capabilities.
An access token still belongs to its intended resource.

## Enterprise transport, native dependencies and distribution

| Area | Observed/documented evidence | Required proof and decision before implementation/release |
| --- | --- | --- |
| Service versus identity transport | Rust foundation has no HTTP transport; architecture provisionally points to a shared Rustls-based service pipeline. P1 uses MSAL defaults, explicit `requests.Session()` for managed identity, and a timed Requests JWKS lookup. R13 documents MSAL HTTP client/proxy/TLS options. | Specify supported proxy/trust/environment behavior separately for service requests, identity HTTP, broker and helper. A Rustls service pipeline does not configure an OS broker/helper's network stack. |
| Enterprise proxies | No custom identity proxy policy is wired in P1; upstream HTTP behavior is not a complete Fabric guarantee. | Test explicit proxy settings and approved environment inputs, bypass/NO_PROXY, authenticated proxy failures, blocked identity endpoints, timeout/cancellation and no proxy-secret logging. Do not forward proxy credentials to untrusted hosts or silently bypass a required proxy. |
| TLS trust | Upstream MSAL Python `verify` defaults to true and custom HTTP clients own their behavior (R13). No Rust trust policy is implemented. | Prove enterprise root acceptance, untrusted/expired/wrong-host rejection and any custom-CA configuration on each native target and helper path. Never disable verification as a compatibility workaround; do not modify OS trust stores for these checks. |
| Certificate authentication | Released Rust `azure_identity` 1.0.0 certificate source imports OpenSSL, parses PKCS12, requires an RSA key and uses the `client_certificate` feature (R10). This differs from Python PEM/PKCS12 handling. | Inventory feature-unified dependencies, OpenSSL build/runtime linkage, architecture/ABI, certificate formats/passwords/rotation, redistribution and patch ownership before choosing that adapter. Rustls for HTTP does not remove certificate-crypto prerequisites. |
| Windows packaging | Current offline foundation needs its normal MSVC/Windows SDK linker, not a new auth installation. WAM interfaces have platform/session/registration prerequisites (R2/R3). | Prove clean-machine architecture-specific helper/native dependencies, signed executable/package identity, update/uninstall, terminal-window behavior and cache ACLs. No Windows ARM64 or installer support follows from CI labels. |
| macOS packaging | R4 ties broker redirect URIs to signed bundle identity and requires a Microsoft broker application. | Establish helper/runtime redistribution, bundle ID/entitlements and redirect, signing/notarization, x64/ARM64 dependencies, enrollment/SSO setup and keychain prompts. A Python broker recipe is not a Rust packaging plan. |
| Linux packaging | R5 names Microsoft identity broker/Intune and WebKit-related dependencies, with distro-specific additions; R11 uses LibSecret for encrypted persistence. | Pin supported released packages/distro/architecture and runtime dependencies, desktop/session-bus/keyring availability, OpenSSL where applicable and broker update ownership. No generic musl, headless/container or WSL claim. |
| Optional helper boundary | An official platform library could run in a packaged helper; no such helper/interface is chosen. | Establish supported public interface and support owner, license/redistribution/runtime footprint, authenticated/local IPC and process ownership, request/response schema with explicit identity/scope/capabilities, cancellation, bounded lifetime, secret-safe streams, version negotiation and crash/error classification. No general-purpose command execution or az/azd/Python bridge. |

The future acceptance artifact must record build and runtime prerequisites
separately, including dynamically loaded libraries and broker/store services.
Run approved install/uninstall/update and no-az/no-azd/no-Python checks on clean
native machines, not just a developer box where missing dependencies are masked.
This document does not add dependencies or select an installer, helper, TLS
stack, certificate backend or OS-store implementation.

## Options comparison without selection

The official MSAL overview (R1) does not list Rust among its supported libraries.
This review has **not established an official Microsoft Rust MSAL SDK or a
supported generic cross-platform MSAL C ABI**. That is an evidence limit, not
proof that no private/native component exists. A crate name such as `msal` or
`msal-rs` does not make it official.

| Option | Potentially useful scope | Support/evidence limits and disposition |
| --- | --- | --- |
| Caller-injected Azure `TokenCredential` | SDK interoperability and mock/service development independent of login UX | Accepted boundary, not a production backend selection or a guarantee about the injected provider's policy behavior |
| Explicit native Azure unattended adapter | Service principal/assertion/managed identity, independently of desktop interactive login | Official released Rust Azure Identity references exist (R8/R10); prove chosen mode/version/features, authority/identity binding, renewal, native prerequisites and service permissions. Consider later as an explicit adapter, never a chain or interactive fallback. |
| Packaged helper around an official platform library | Possible broker integration on platforms with supported official libraries | Unknown until supported interface/support ownership, IPC, redistribution/runtime, native packaging and tenant-policy proof are established. The official library's support does not automatically cover a custom wrapper. No Python production helper. |
| Native protocol/browser OAuth | Possible browser user authentication without MSAL | Browser MFA can work, but broker/SSO/device-bound/CA parity is unknown; protocol correctness, supported app registration, refresh, secure cache and lifecycle still need proof. Not a substitute for mandatory broker policy; no selection made. |
| Community MSAL Rust crate or assumed generic C binding | Possible experimentation only under a separate decision | Official support and complete platform/policy coverage not established. Do not market community crates as Microsoft-supported or assume a native ABI/redistribution contract. |
| Copilot runtime Entra/EMU flow | Specialized GitHub EMU authentication in its own runtime | The existing identity decision records that this is not a verified general Fabric/OneLake/ARM provider. No independent integration proof is supplied here; runtime dependence and audience/support scope remain unresolved. It is not a backend shortcut. |
| `DeveloperToolsCredential`, `AzureCliCredential`, `AzureDeveloperCliCredential` | Upstream local-development convenience | Documented az/azd dependency (R8); unsupported by E2, even if an example authenticates successfully |
| Static token or plaintext persistence | Superficial demo/login shortcut | Unsupported by E3/E4; no production fallback or successful-looking stub |

## Offline work that remains unblocked

Continue the read-only workspace-list SDK/CLI slice with caller-injected mock
credentials and transport, exact scope/endpoint assertions, bounded pagination
and typed service errors. This does not require interactive auth, an installed
broker, or a production identity adapter.

The identity lane can also specify synthetic cases for explicit-selection
ambiguity/guest tenants, provider error classification, clock-driven renewal,
cancellation/logout races, secure-persistence failure and redaction. Preserve the
Python oracle and record E1-E6 expectations independently; do not change shared
core types or add providers merely to make these plans compile.

Actual auth implementation remains outside this document. Before any candidate
backend is adopted, the team must resolve supported interface/owner and
redistribution, exact platform/policy acceptance, client registration/authority,
secure-cache/logout semantics and any unattended mode's secret/assertion input
contract. No backend decision is needed to finish this evidence inventory.

### One prioritized owner question

**Which single OS/version/architecture and concrete tenant-policy/resource
combination should be the first identity acceptance target?**

For example, the owner could identify Windows 11 x64 with WAM/OS SSO, MFA and
compliant-device Conditional Access for Fabric and OneLake, with ARM separately
scoped. This example is not a chosen target or backend. If device-bound tokens
or Token Protection are required, name the exact supported client/resource
combination and expected allow/deny evidence rather than assuming those terms
are equivalent. This answer guides later proof; it does not block document
completion or authorize tenant operations.

## Public reference register

All references below were read on **October 22, 2026 (UTC)**. Learn revision
identifiers are the page's returned source metadata, not assertions that a
private documentation-source repository is publicly accessible. Public
documentation is evidence of the named library, not of this Rust product.

| ID | Public reference and exact version/revision inspected | Evidence used and limits |
| --- | --- | --- |
| R1 | [MSAL overview](https://learn.microsoft.com/en-us/entra/msal/overview), source revision `ee2d0c07d0c6257915208c74efbde85357907e97`, updated July 30, 2025 | Official library/platform list and acquisition/cache responsibilities; no Rust library listed, no generic C ABI established |
| R2 | [MSAL Python WAM](https://learn.microsoft.com/en-us/entra/msal/python/advanced/wam), revision `a4640c1cd45ba0e5aa9f0162e289cc5032fba2cf`, updated July 18, 2025 | Windows versions, broker extra, redirect, window handle and account UX |
| R3 | [MSAL.NET WAM](https://learn.microsoft.com/en-us/entra/msal/dotnet/acquiring-tokens/desktop-mobile/wam), revision `1cb0d817cf6e13e59667137a7765c70a7b1f6fb8`, updated August 8, 2025 | Official .NET interface, OS SSO, refresh-token binding/PoP capabilities, browser fallback and interactive-session restrictions; not a Rust interface |
| R4 | [MSAL Python macOS broker](https://learn.microsoft.com/en-us/entra/msal/python/advanced/macos-broker), revision `cc2cf1e446b37b7650293d363ed2ec357ee985d8`, updated February 2, 2026 | Company Portal, MSAL 1.31+, signed/unsigned redirects, cache ownership, OS/architecture matrix; future token-protection wording is not acceptance proof |
| R5 | [MSAL Python Linux broker](https://learn.microsoft.com/en-us/entra/msal/python/advanced/linux-broker-py), revision `7bdb129be0245909f50e2711092bd1834733e4d1`, updated May 18, 2026 | Linux/WSL opt-ins, Microsoft broker, device-bound refresh, prerequisites and fallback rules; beta install-example inconsistency recorded above |
| R6 | [Conditional Access Token Protection](https://learn.microsoft.com/en-us/entra/identity/conditional-access/concept-token-protection), revision `c13a0417503d82113f700cdc2337751ea801dd83`, updated August 20, 2026 | Resource/client/platform restrictions; GA/preview wording differs across sections. Does not prove Fabric/OneLake/native ARM support |
| R7 | [Fabric identity support](https://learn.microsoft.com/en-us/rest/api/fabric/articles/identity-support), revision `b7d81b288eaf69b98ffa3733037aabf0f5524268`, updated April 8, 2026 | User/SP/MI identity categories, administrator-owned tenant setting, per-API and underlying-item restrictions |
| R8 | [Azure Identity for Rust 1.0.0](https://docs.rs/azure_identity/1.0.0/azure_identity/) | Released package documentation links the official `Azure/azure-sdk-for-rust` source; developer-tool dependencies and native assertion/managed-identity examples. No dependency is added here; no claim this is installed or validated against the workspace graph |
| R9 | [OneLake API access](https://learn.microsoft.com/en-us/fabric/onelake/onelake-access-api), revision `3eba429817f2e81ad534e7c0505a22886468f3fb`, updated June 23, 2026 | Explicit Storage audience requirement; sample login/token commands were not executed and are not the Rust product design |
| R10 | [Azure Rust certificate credential API](https://docs.rs/azure_identity/1.0.0/azure_identity/struct.ClientCertificateCredential.html) and [released source](https://docs.rs/azure_identity/1.0.0/src/azure_identity/client_certificate_credential.rs.html), version 1.0.0 | `client_certificate` feature, PKCS12/RSA, OpenSSL imports and parsing; does not prove Python PEM parity or a native packaging solution |
| R11 | [Microsoft authentication extensions for Python README, tag 1.3.1](https://github.com/AzureAD/microsoft-authentication-extensions-for-python/blob/1.3.1/README.md) | DPAPI/macOS Keychain/LibSecret, encrypted persistence, file lock and reload; reference release, not a claim that P10 resolves to this version |
| R12 | [Claims challenges and client capabilities](https://learn.microsoft.com/en-us/entra/identity-platform/claims-challenge), revision `a4be4ac419c4e857b1c4de7dee22c9f7e0c750f9`, updated June 15, 2026 | Challenge/claims/capability responsibilities; does not establish that Python bridge or Rust SDK implements them |
| R13 | [MSAL Python ClientApplication reference](https://learn.microsoft.com/en-us/python/api/msal/msal.application.clientapplication?view=msal-py-latest), revision `1d96efd4dcab4166b898bf69981ce37e19afdb01`, updated March 6, 2026 | `http_client`, `verify`, `proxies`, `timeout` and client capabilities; generated latest reference, not a pinned Python dependency resolution |

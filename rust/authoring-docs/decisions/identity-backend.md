<!-- Copyright (c) Microsoft Corporation. Licensed under the MIT License. -->

# Identity backend decision

**Status: optional implementations exist; production support and native policy
acceptance remain unresolved.** Windows broker login and remote reads have
user-reported smoke evidence. This does not certify all accounts, guest tenants,
MFA, Conditional Access, refresh/logout races or other operating systems.
Direct SDK credential injection and default offline builds remain independent.

## Current optional implementations

| Backend | Selection and persistence | Acceptance boundary |
| --- | --- | --- |
| Windows broker (`windows-broker`) | Windows default; explicit hints or native picker; WAM retains tokens and a non-secret generation-bound binding retains selection | Controlled broker, guest/account, MFA and tenant-policy scenarios |
| Unattended (`headless`) | Explicit client-secret, renewable assertion, workload-identity or supported managed-host contract; no credential chain | Native host and service-permission matrix |
| Device code (`interactive`) | Explicit device-code method, first-login public-cloud tenant/client GUIDs; refresh credentials only in an OS protected store | Windows Credential Manager, Linux no-prompt Secret Service and macOS isolated protected-store helper require native acceptance |
| Windows certificate (`certificate-store-windows`) | Explicit native certificate/private-key selection; no private-key export | Native store policy and enrolled application acceptance |
| Linux certificate (`certificate-store-pkcs11`) | Explicit direct module digest, slot/token identity, full certificate fingerprint, CKA_ID and PIN callback or protected authentication path | Trusted native module/token policy and enrolled application acceptance |
| macOS certificate (`certificate-store-macos`) | Explicit Keychain/certificate and independently pinned, root-custodied isolated signer helper | Native extractability/signing attributes, helper installation and enrolled application acceptance |

These features are default-off. Explicit CLI method/provider configuration wins
over platform defaults; an unavailable explicit provider does not silently
switch backend. Windows does not choose device code merely because broker
support was omitted at build time. SDK credential refresh and status never
start an interactive login.

Device code uses the selected TLS-authenticated Microsoft token endpoint and
requires all three Fabric, Storage and ARM acquisitions before publishing a
session. Returned identity metadata is not a claim of local ID-token signature
verification. This flow is **not broker SSO, device-bound authentication or
proof that a tenant permits device code**.

No protected-store or certificate operation may invoke an implicit prompt.
macOS no-UI process-global policy belongs only in an isolated helper, not the
SDK process. Locked/missing stores, required interaction and uncertain cleanup
must remain explicit errors; no plaintext cache or alternate credential is a
fallback. See the [native certificate contract](../../engine/core/auth/provider/native-certificates.md)
and [identity guide](../../engine/core/auth/provider/README.md).

Authentication/core changes still require Fabric CLI team review. Offline
protocol/lifecycle tests, compiled feature flags and successful login metadata
do not establish production support, service authorization, signing or native
three-platform acceptance.

## Historical decision record

The sections below preserve the original scaffold decision and later dated
approvals. References to an absent adapter or unvalidated implementation describe
those stages; the table above is the current implementation/acceptance map.

## Accepted boundary

SDK consumers inject an
`Arc<dyn azure_core::credentials::TokenCredential>`. They do not need
`fabric-identity` or the CLI. Account, tenant, and audience selection are explicit;
the caller must bind the injected provider to that selection. Selection metadata
does not independently verify token claims.

The scaffold has no production credential provider. The example's in-memory,
expired dummy token is test-only and cannot establish a real session.
`interactive_credential` returns `IdentityError::Unsupported` without launching
a browser, probing ambient credentials, invoking tools, or persisting data.

The shared error vocabulary distinguishes unsupported capability, missing
configuration, interaction required, cancellation, and refresh failure.
Future adapters must preserve these distinctions, sanitize diagnostics, and
never silently fall back to another account, tenant, audience, or backend.
There is no current adapter translating Azure provider errors into these
variants; returning one generic refresh error for all failures would be wrong.

## Why no backend is selected

The assignment's baseline research has not established an official Microsoft
Rust MSAL SDK or a supported generic cross-platform MSAL C ABI. Community crates
named `msal` or `msal-rs` must not be treated as official Microsoft bindings.

Browser MFA can work without MSAL, but that alone does not demonstrate OS-account
SSO, device-bound tokens, broker capabilities, or satisfaction of tenant
Conditional Access requirements. Generic OAuth must not silently substitute for
those enterprise requirements.

Copilot Rust's broker-aware flow delegates to its runtime and derives GitHub EMU
credentials. It is not a verified general Fabric, OneLake, or ARM credential
provider. Its Azure local-development example uses `DeveloperToolsCredential`,
which invokes `az`/`azd` and violates this product's installation requirement.
Neither Copilot's SDK nor its runtime is included here.

## Required proof before choosing an implementation

1. Establish supported interfaces and official support ownership per platform.
2. Demonstrate explicit account, tenant, and audience handling, including the
   relevant Fabric, OneLake, and ARM flows.
3. Validate enterprise SSO, MFA, device-bound and Conditional Access requirements
   against representative tenant policies. Document unsupported combinations.
4. Prove cancellation, refresh, interaction-required and error classification
   without fallback or credential disclosure.
5. Establish secure platform persistence, cache isolation, and logout behavior.
   No plaintext persistence or successful-looking stub login is acceptable.
6. Resolve enterprise proxies/trust, certificate authentication prerequisites,
   helper packaging, redistribution, signing, and notarization.
7. Verify authentication without installed Azure CLI, Azure Developer CLI, or
   Python.

A packaged official-platform helper remains an option, subject to supported
interfaces, redistribution rights, native packaging, and tenant-policy proof.
No helper or generic OAuth implementation is selected by this scaffold.

Explicit unattended Azure identity adapters can be considered independently,
adding `azure_identity` only when exercised. They must not use a developer-tool
fallback chain, expose credentials in arguments/logs/snapshots, or silently
persist credentials. Authentication implementation requires Fabric CLI team
review.

## Native isolation approved October 23, 2026

The owner selected entirely Rust authentication investigation, retaining
Python-like interactive MFA, Windows broker/SSO, session reuse, and all three
Fabric/OneLake/ARM audiences. The owner subsequently approved unsafe interop
**only in a separate crate**, so executable/library consumers can retain
`forbid(unsafe_code)` and optionally exclude that native dependency.

`fabric-identity-windows` now contains the desktop interop boundary behind
`fabric-identity/windows-broker`. Existing crates retain the workspace unsafe
prohibition. This approves isolation, not a production backend or an assumption
that feature-enabled binaries are transitively unsafe-free. See
[architecture](../architecture.md#optional-native-unsafe-boundary).

The low-level request API has an owned STA/window lifetime and a safe public Rust
surface. It does not supply a configured `TokenCredential`, parse token expiry,
silently bind an account/tenant, implement local persistence/logout, or launch
authentication from a test. The original public `interactive_credential`
continues to return `Unsupported`. Provider-contract and live-policy proof gates
remain open; no success-shaped login or browser fallback is introduced.

## Experimental direct-WAM login, October 23, 2026

After the owner requested actual login, the missing provider-field reference was
established from official MSAL.NET **4.30.1**, commit
`0661b34a41255bfdd7980663572be0544578117e`:
[AadPlugin](https://github.com/AzureAD/microsoft-authentication-library-for-dotnet/blob/0661b34a41255bfdd7980663572be0544578117e/src/client/Microsoft.Identity.Client/Platforms/Features/WamBroker/AadPlugin.cs),
[WamAdapters](https://github.com/AzureAD/microsoft-authentication-library-for-dotnet/blob/0661b34a41255bfdd7980663572be0544578117e/src/client/Microsoft.Identity.Client/Platforms/Features/WamBroker/WamAdapters.cs),
[WebAccountProviderFactory](https://github.com/AzureAD/microsoft-authentication-library-for-dotnet/blob/0661b34a41255bfdd7980663572be0544578117e/src/client/Microsoft.Identity.Client/Platforms/Features/WamBroker/WebAccountProviderFactory.cs)
and [CoreHelpers](https://github.com/AzureAD/microsoft-authentication-library-for-dotnet/blob/0661b34a41255bfdd7980663572be0544578117e/src/client/Microsoft.Identity.Client/Utils/CoreHelpers.cs).
These MIT-licensed released sources were inspected on October 23, 2026. They are
historical official implementation evidence, not a current Microsoft support
promise for a custom Rust adapter.

The request sets `wam_compat=2.0`, explicit `authority`, `validateAuthority=yes`,
and `LoginHint`, using the requested `.default` scope plus MSAL's OIDC scopes.
Interactive requests use the selected-account prompt; silent calls never show
UI. Response `Authority` must match the explicit public-cloud tenant.
`TokenExpiresOn` is **seconds since January 1, 1601**, not FILETIME ticks or Unix
seconds: subtract 11,644,473,600, reject missing/malformed/expired timestamps and
never invent expiry or inspect access-token JWT claims. Native account ID and
UPN are checked across all three audiences and subsequent SDK refresh.

`WindowsCredential::login` and the opt-in CLI now execute actual broker calls;
the original synchronous metadata-only `interactive_credential` remains
unsupported because it has no explicit client registration or async interaction
contract. WAM owns secure token persistence. CLI success publishes only
non-secret metadata after all three acquisitions; no local plaintext cache,
provider-error text, developer-tool fallback or service call is added.
`status`/SDK refresh are silent; logout is explicit/app-scoped. Cancellation and
partial failures do not promise rollback of broker state.

This is runnable experimental Windows code, **not live-validated MFA or CA
parity**. The supplied client registration's redirects/permissions, current
provider behavior (including guests/aliases), real expiry fields and policy
outcomes still require manual acceptance. See [usage](../../README.md#run-an-actual-windows-broker-login).
